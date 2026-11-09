//! Phase 4C audio transport: every audio track rendered sample by sample,
//! summed into one output ring, and the audio-master clock.
//!
//! Policy (see `docs/PHASE_4C_AUDIO_MIXING_CONTRACT.md`): when an output
//! device is open, timeline playback is clocked by audio. Output sample `k` of
//! an epoch that starts at timeline position `P0` is presented at
//! `P0 + k / rate`. Inside an audio clip a track plays source sample
//! `k + offset` (one ceiling rounding per clip,
//! `cutcap_core::source_sample_offset`); between clips and wherever the source
//! has no samples it contributes silence. Real samples are never invented and
//! nothing is resampled in time. The clock is the device's played position
//! mapped back through the output's segment map, so it advances only while
//! timeline audio (silence included) is actually being played.
//!
//! One feeder thread owns a bounded set of decoder slots; each slot is an
//! FFmpeg process streaming PCM into a bounded queue. The feeder mixes one
//! short block at a time, in project track order, reading the track controls
//! once per block. A seek, edit or stop starts a new epoch: decoders are
//! cancelled and the output flushed, and the epoch is rechecked under the feed
//! lock immediately before each push, so stale audio can never leak.
use crate::audio_output::{AudioOutput, OutputStatus};
use cutcap_contracts::*;
use cutcap_core::{Clip, ClipStream, samples_until, source_sample_offset, timeline_at_sample};
use cutcap_media::{Cancellation, MediaBackend, MediaError, PcmChunk, PcmRequest};
use serde::Serialize;
use std::{
    sync::{
        Arc, Condvar, Mutex,
        mpsc::{self, Receiver},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

/// Audible tracks that may have overlapping clips.
pub const MAX_MIX_TRACKS: usize = 16;
/// Decoder processes at once: every active track plus bounded prefetch.
pub const MAX_DECODERS: usize = MAX_MIX_TRACKS + 8;
/// Decoded PCM queued per decoder: 24 chunks of at most [`MAX_CHUNK_FRAMES`].
const SLOT_QUEUE: usize = 24;
const MAX_CHUNK_FRAMES: usize = 4096;
/// A clip's decoder starts this long before its first sample.
const PREFETCH_MS: u64 = 1500;
/// A track made audible mid-clip decodes from this far ahead of the mix.
const LATE_JOIN_MS: u64 = 300;
/// Most audio mixed per step; controls are read once per block.
const BLOCK_MS: u64 = 10;
/// Length of the linear ramp applied when a track's gain changes.
pub const GAIN_RAMP_MS: u64 = 10;

/// What the mixer applies to one track.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TrackMix {
    pub id: TrackId,
    /// Linear amplitude gain.
    pub gain: f32,
    pub audible: bool,
}

/// The mix of the given audio tracks, applying the mute/solo precedence.
pub fn track_mix(tracks: &[(TrackId, TrackControls)]) -> Vec<TrackMix> {
    let any_solo = tracks.iter().any(|(_, c)| c.solo);
    tracks
        .iter()
        .map(|(id, c)| TrackMix {
            id: *id,
            gain: c.linear_gain(),
            audible: c.audible(any_solo),
        })
        .collect()
}

/// A linear gain ramp: each frame's gain, advanced frame by frame.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct GainRamp {
    current: f32,
    target: f32,
    step: f32,
}

impl GainRamp {
    fn at(gain: f32) -> Self {
        Self {
            current: gain,
            target: gain,
            step: 0.0,
        }
    }
    fn retarget(&mut self, target: f32, frames: u32) {
        if target != self.target {
            self.target = target;
            self.step = (target - self.current) / frames.max(1) as f32;
        }
    }
    fn advance(&mut self) {
        if self.current != self.target {
            let next = self.current + self.step;
            let reached = if self.step > 0.0 {
                next >= self.target
            } else {
                next <= self.target
            };
            self.current = if reached { self.target } else { next };
        }
    }
    fn skip(&mut self, frames: usize) {
        for _ in 0..frames {
            if self.current == self.target {
                break;
            }
            self.advance();
        }
    }
    /// Adds `samples` (interleaved stereo) to `out`, scaled by this ramp.
    fn mix_into(&mut self, samples: &[f32], out: &mut [f32]) {
        if self.current == self.target {
            let gain = self.current;
            for (o, s) in out.iter_mut().zip(samples) {
                *o += *s * gain;
            }
            return;
        }
        for (o, s) in out
            .as_chunks_mut::<2>()
            .0
            .iter_mut()
            .zip(samples.as_chunks::<2>().0)
        {
            o[0] += s[0] * self.current;
            o[1] += s[1] * self.current;
            self.advance();
        }
    }
}

/// Hard-clips the final sum to [-1, 1]; returns the clipped samples and peak.
/// A non-finite sample cannot reach the device: it becomes silence.
fn clip_mix(out: &mut [f32]) -> (u64, f32) {
    let (mut clipped, mut peak) = (0, 0.0f32);
    for sample in out {
        if !sample.is_finite() {
            *sample = 0.0;
            continue;
        }
        let magnitude = sample.abs();
        peak = peak.max(magnitude);
        if magnitude > 1.0 {
            clipped += 1;
            *sample = sample.clamp(-1.0, 1.0);
        }
    }
    (clipped, peak)
}

/// One clip of an audio track, with what playback needs.
#[derive(Clone, Debug)]
pub struct AudioClip {
    pub clip: Clip,
    pub media: Option<MediaSummary>,
    /// Why the clip is silent (missing/changed source, no decodable stream).
    pub unavailable: Option<String>,
}

#[derive(Clone, Debug)]
pub struct AudioPlan {
    pub timebase: Timebase,
    pub clips: Vec<AudioClip>,
    pub extent: TimelineTime,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct AudioView {
    /// unavailable, idle, starting, playing, paused, ended, failed.
    pub state: &'static str,
    /// "audio" when playback is clocked by the device, "system" otherwise.
    pub clock: &'static str,
    pub output: OutputStatus,
    pub epoch: u64,
    pub decoders: usize,
    pub decoder_starts: u64,
    pub source_silence_ms: f64,
    pub silent_clips: u64,
    pub detail: Option<String>,
    pub mix: MixView,
}

/// Mixer diagnostics for the current epoch; never project data.
#[derive(Debug, Clone, Default, Serialize)]
pub struct MixView {
    /// Tracks with an active clip in the last block.
    pub active_tracks: usize,
    /// Of those, the tracks whose PCM was summed.
    pub mixed_tracks: usize,
    pub mixed_tracks_peak: usize,
    pub decoders_peak: usize,
    /// Samples (per channel) hard-clipped at the final sum.
    pub clipped_samples: u64,
    /// Largest absolute sample of the sum before clipping.
    pub peak: f32,
    /// Audible active clips that had no decoder because of the budget.
    pub over_limit: u64,
    pub blocks: u64,
    pub block_us_last: u64,
    pub block_us_max: u64,
    pub block_us_total: u64,
    pub mixed_ms: f64,
}

/// An epoch's exact sample layout of one clip, in output frames since `P0`.
#[derive(Clone, Copy, Debug)]
struct Span {
    s0: u64,
    s1: u64,
    offset: i64,
    clip: usize,
    /// Index into the epoch's tracks, in project order.
    track: usize,
}

struct Slot {
    span: usize,
    rx: Receiver<PcmChunk>,
    cancel: Cancellation,
    done: Arc<Mutex<Option<Result<(), MediaError>>>>,
    worker: Option<JoinHandle<()>>,
    pending: Option<PcmChunk>,
    /// This slot's PCM has reached the mix.
    joined: bool,
}

/// What a decoder can supply at one position.
#[derive(Clone, Debug, PartialEq)]
enum Run {
    /// Still decoding.
    Wait,
    /// The stream has no sample for this many frames.
    Silent(usize),
    /// This many frames of PCM are ready.
    Pcm(usize),
    /// The stream has ended (or failed: the error).
    End(Option<String>),
}

impl Slot {
    /// What is available at source sample `needed`, up to `max` frames. PCM
    /// behind `needed` is dropped. Nothing is consumed: a chunk stays until
    /// the mix has passed its end.
    fn peek(&mut self, needed: i64, max: usize) -> Run {
        loop {
            if self.pending.is_none() {
                match self.rx.try_recv() {
                    Ok(chunk) => self.pending = Some(chunk),
                    Err(_) => {
                        // A decoder publishes its result after its last chunk,
                        // so read the result first and then the queue again.
                        let Some(outcome) = finished(self) else {
                            return Run::Wait;
                        };
                        match self.rx.try_recv() {
                            Ok(chunk) => self.pending = Some(chunk),
                            Err(_) => return Run::End(outcome.err()),
                        }
                    }
                }
            }
            let Some(chunk) = self.pending.as_ref() else {
                continue;
            };
            if chunk.end_sample() <= needed {
                self.pending = None;
            } else if chunk.first_sample > needed {
                return Run::Silent(((chunk.first_sample - needed) as usize).min(max));
            } else {
                return Run::Pcm(((chunk.end_sample() - needed) as usize).min(max));
            }
        }
    }
    /// The `frames` of interleaved PCM at `needed`, after `peek` returned Pcm.
    fn samples(&self, needed: i64, frames: usize) -> &[f32] {
        self.pending.as_ref().map_or(&[], |chunk| {
            let at = (needed - chunk.first_sample) as usize * 2;
            &chunk.samples[at..at + frames * 2]
        })
    }
    fn stop(&mut self) {
        self.cancel.cancel();
        // Dropping the receiver unblocks a decoder waiting to send.
        let (_tx, rx) = mpsc::sync_channel(1);
        drop(std::mem::replace(&mut self.rx, rx));
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
impl Drop for Slot {
    fn drop(&mut self) {
        self.stop();
    }
}

#[derive(Default)]
struct Feed {
    epoch: u64,
    plan: Option<Arc<AudioPlan>>,
    origin: TimelineTime,
    spans: Arc<Vec<Span>>,
    tracks: Arc<Vec<TrackId>>,
    extent_frames: u64,
    written: u64,
    finished: bool,
    shutdown: bool,
    decoders: usize,
    decoder_starts: u64,
    source_silence: u64,
    silent_clips: u64,
    detail: Option<String>,
    /// The latest controls; tracks not listed play at unity.
    controls: Arc<Vec<TrackMix>>,
    mix: MixView,
}

struct Shared {
    feed: Mutex<Feed>,
    wake: Condvar,
}

pub struct AudioTransport {
    output: Arc<AudioOutput>,
    shared: Arc<Shared>,
    worker: Option<JoinHandle<()>>,
    rate: u32,
    playing: bool,
}

impl AudioTransport {
    pub fn start(
        backend: Arc<dyn MediaBackend>,
        output: AudioOutput,
    ) -> Result<Self, std::io::Error> {
        let rate = output.rate();
        let output = Arc::new(output);
        let shared = Arc::new(Shared {
            feed: Mutex::new(Feed::default()),
            wake: Condvar::new(),
        });
        let worker = {
            let (output, shared) = (output.clone(), shared.clone());
            thread::Builder::new()
                .name("cutcap-audio-feed".into())
                .spawn(move || feed(&backend, &output, &shared))?
        };
        Ok(Self {
            output,
            shared,
            worker: Some(worker),
            rate,
            playing: false,
        })
    }

    fn feed(&self) -> std::sync::MutexGuard<'_, Feed> {
        self.shared.feed.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Replaces the track controls. Takes effect at the mixer's next block,
    /// through a short ramp; never restarts anything.
    pub fn set_controls(&self, tracks: &[(TrackId, TrackControls)]) {
        self.feed().controls = Arc::new(track_mix(tracks));
    }

    /// Rejects a plan that needs more simultaneous audible tracks than the
    /// mixer supports, with an explicit message.
    pub fn check_limit(&self, plan: &AudioPlan, position: TimelineTime) -> Result<(), String> {
        let (spans, _, tracks) = layout(plan, position, self.rate);
        let controls = self.feed().controls.clone();
        let audible = |track: usize| {
            controls
                .iter()
                .find(|m| Some(m.id) == tracks.get(track).copied())
                .is_none_or(|m| m.audible)
        };
        let mut events: Vec<(u64, i32)> = spans
            .iter()
            .filter(|s| audible(s.track))
            .flat_map(|s| [(s.s0, 1), (s.s1, -1)])
            .collect();
        events.sort_unstable();
        let mut active = 0;
        for (frame, delta) in events {
            active += delta;
            if active as usize > MAX_MIX_TRACKS {
                let at = timeline_at_sample(position, frame, plan.timebase, self.rate)
                    .unwrap_or(position);
                let ms = at.0 as i128 * i128::from(plan.timebase.numerator()) * 1000
                    / i128::from(plan.timebase.denominator());
                return Err(format!(
                    "{active} audible audio tracks overlap at {}. CutCap mixes at most \
                     {MAX_MIX_TRACKS} at once: mute some tracks or shorten clips.",
                    crate::view::precise_duration(ms as i64)
                ));
            }
        }
        Ok(())
    }

    /// Starts a new epoch at `position`: decoders restart, the output is
    /// flushed and the device position restarts at zero.
    pub fn begin(&mut self, plan: AudioPlan, position: TimelineTime) {
        let (spans, extent_frames, tracks) = layout(&plan, position, self.rate);
        let mut feed = self.feed();
        feed.epoch += 1;
        feed.origin = position;
        feed.spans = Arc::new(spans);
        feed.tracks = Arc::new(tracks);
        feed.extent_frames = extent_frames;
        feed.written = 0;
        feed.finished = false;
        feed.source_silence = 0;
        feed.silent_clips = 0;
        feed.detail = None;
        feed.mix = MixView::default();
        feed.plan = Some(Arc::new(plan));
        self.output.reset();
        drop(feed);
        self.playing = false;
        self.shared.wake.notify_all();
    }

    /// Enough audio is queued to start without an immediate underrun.
    pub fn ready(&self) -> bool {
        self.feed().plan.is_some() && self.output.prefilled()
    }

    pub fn play(&mut self) {
        self.playing = true;
        self.output.start();
    }

    pub fn pause(&mut self) {
        self.playing = false;
        self.output.pause();
    }

    /// Ends the epoch: decoders stop, the output is flushed.
    pub fn cancel(&mut self) {
        let mut feed = self.feed();
        feed.epoch += 1;
        feed.plan = None;
        feed.spans = Arc::new(Vec::new());
        self.output.reset();
        drop(feed);
        self.playing = false;
        self.shared.wake.notify_all();
    }

    /// The audio-master timeline position at `now`; `None` until the device
    /// has reported a position in this epoch.
    pub fn clock(&self, now: Instant) -> Option<TimelineTime> {
        let feed = self.feed();
        let plan = feed.plan.as_ref()?;
        let frames = self.output.clock(now)?;
        timeline_at_sample(feed.origin, frames, plan.timebase, self.rate)
            .ok()
            .map(|t| t.min(plan.extent))
    }

    pub fn failed(&self) -> Option<String> {
        self.output.failed()
    }

    /// Virtual output only (tests): the device plays `frames` now.
    #[cfg(test)]
    pub fn consume(&self, frames: usize, now: Instant) {
        self.output.consume(frames, now);
    }

    /// Virtual output only (tests): like a real device, plays the period that
    /// was in flight when the stream was stopped.
    #[cfg(test)]
    pub fn consume_in_flight(&self, frames: usize, now: Instant) {
        self.output.consume_in_flight(frames, now);
    }

    /// Test helper: waits until the feeder has queued `frames` (or finished).
    #[cfg(test)]
    pub fn wait_buffered(&self, frames: usize) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while self.output.buffered_frames() < frames && !self.feed().finished {
            assert!(Instant::now() < deadline, "feeder did not fill the ring");
            thread::sleep(Duration::from_millis(1));
        }
    }

    /// Virtual output only (tests): the device fails.
    #[cfg(test)]
    pub fn simulate_device_failure(&self, message: &str) {
        self.output.simulate_failure(message);
    }

    /// Test helper: waits until the feeder has written the whole epoch and
    /// told the output so (the feed lock is held across both).
    #[cfg(test)]
    pub fn wait_finished(&self) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !self.feed().finished {
            assert!(Instant::now() < deadline, "feeder did not finish the epoch");
            thread::sleep(Duration::from_millis(1));
        }
    }

    pub fn view(&self) -> AudioView {
        let feed = self.feed();
        let output = self.output.status();
        let ms = |frames: u64| frames as f64 * 1000.0 / f64::from(self.rate.max(1));
        AudioView {
            state: if output.error.is_some() {
                "failed"
            } else if feed.plan.is_none() {
                "idle"
            } else if self.playing && output.state == "playing" {
                if feed.finished && output.buffered_ms == 0.0 {
                    "ended"
                } else {
                    "playing"
                }
            } else if self.playing {
                "starting"
            } else {
                "paused"
            },
            clock: "audio",
            output,
            epoch: feed.epoch,
            decoders: feed.decoders,
            decoder_starts: feed.decoder_starts,
            source_silence_ms: ms(feed.source_silence),
            silent_clips: feed.silent_clips,
            detail: feed.detail.clone(),
            mix: MixView {
                mixed_ms: ms(feed.written),
                ..feed.mix.clone()
            },
        }
    }

    pub fn shutdown(&mut self) {
        {
            let mut feed = self.feed();
            feed.shutdown = true;
            feed.epoch += 1;
        }
        self.shared.wake.notify_all();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        if let Some(output) = Arc::get_mut(&mut self.output) {
            output.shutdown();
        }
    }
}

impl Drop for AudioTransport {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Exact epoch layout: each clip's output-frame interval and source offset,
/// the extent in frames, and the tracks in first-appearance (project) order.
fn layout(plan: &AudioPlan, origin: TimelineTime, rate: u32) -> (Vec<Span>, u64, Vec<TrackId>) {
    let frames = |position: TimelineTime| {
        samples_until(origin, position, plan.timebase, rate).unwrap_or(i64::MAX)
    };
    let mut spans = Vec::new();
    let mut tracks: Vec<TrackId> = Vec::new();
    for (index, entry) in plan.clips.iter().enumerate() {
        let track = tracks
            .iter()
            .position(|t| *t == entry.clip.track_id)
            .unwrap_or_else(|| {
                tracks.push(entry.clip.track_id);
                tracks.len() - 1
            });
        let Ok(end) = entry.clip.end() else { continue };
        let (s0, s1) = (frames(entry.clip.start).max(0), frames(end));
        if s1 <= s0 || s1 <= 0 {
            continue;
        }
        let Ok(offset) = source_sample_offset(&entry.clip, origin, plan.timebase, rate) else {
            continue;
        };
        spans.push(Span {
            s0: s0 as u64,
            s1: s1 as u64,
            offset,
            clip: index,
            track,
        });
    }
    spans.sort_by_key(|s| (s.s0, s.track));
    (spans, frames(plan.extent).max(0) as u64, tracks)
}

fn start_slot(
    backend: &Arc<dyn MediaBackend>,
    plan: &AudioPlan,
    span: &Span,
    index: usize,
    from_frame: u64,
    rate: u32,
) -> Option<Slot> {
    let entry = &plan.clips[span.clip];
    let (Some(media), None, ClipStream::Audio { index: stream }) =
        (entry.media.clone(), &entry.unavailable, entry.clip.stream)
    else {
        return None;
    };
    let request = PcmRequest {
        media,
        stream_index: stream,
        start_sample: from_frame.max(span.s0) as i64 + span.offset,
        end_sample: Some(span.s1 as i64 + span.offset),
        rate: Some(rate),
        background: false,
    };
    let (tx, rx) = mpsc::sync_channel(SLOT_QUEUE);
    let cancel = Cancellation::default();
    let done = Arc::new(Mutex::new(None));
    let worker = {
        let (backend, cancel, done) = (backend.clone(), cancel.clone(), done.clone());
        thread::Builder::new()
            .name("cutcap-audio-decoder".into())
            .spawn(move || {
                let result = backend.stream_pcm(&request, &cancel, &mut |chunk| {
                    // Bounded queue, bounded chunks: a bound on queued frames.
                    let channels = usize::from(chunk.channels.max(1));
                    let mut first = chunk.first_sample;
                    for part in chunk.samples.chunks(MAX_CHUNK_FRAMES * channels) {
                        let frames = part.len() / channels;
                        tx.send(PcmChunk {
                            first_sample: first,
                            channels: chunk.channels,
                            samples: part.to_vec(),
                            process_id: chunk.process_id,
                        })
                        .map_err(|_| MediaError::Cancelled)?;
                        first += frames as i64;
                    }
                    Ok(())
                });
                if let Ok(mut slot) = done.lock() {
                    *slot = Some(result);
                }
            })
            .ok()?
    };
    Some(Slot {
        span: index,
        rx,
        cancel,
        done,
        worker: Some(worker),
        pending: None,
        joined: false,
    })
}

/// Outcome of a decoder that has stopped: `Some(Ok)` = no more samples.
fn finished(slot: &Slot) -> Option<Result<(), String>> {
    let done = slot.done.lock().ok()?;
    match done.as_ref()? {
        Ok(()) | Err(MediaError::NoFrame) => Some(Ok(())),
        Err(error) => Some(Err(error.to_string())),
    }
}

/// One epoch's feeder state, owned by the feeder thread.
#[derive(Default)]
struct Epoch {
    slots: Vec<Slot>,
    /// Spans that will never have a decoder.
    silent: Vec<usize>,
    /// Those of them whose reason has been reported.
    reported: Vec<usize>,
    /// Spans refused by the budget (reported once).
    refused: Vec<usize>,
    /// Spans before this index have finished.
    first: usize,
    ramps: Vec<GainRamp>,
    audible: Vec<bool>,
    seen: Option<Arc<Vec<TrackMix>>>,
    started: bool,
}

impl Epoch {
    /// Applies new controls to the per-track ramps; a new epoch starts at the
    /// targets with no ramp, so a seek or start is sample-exact.
    fn apply_controls(&mut self, tracks: &[TrackId], controls: &Arc<Vec<TrackMix>>, ramp: u32) {
        if self
            .seen
            .as_ref()
            .is_some_and(|seen| Arc::ptr_eq(seen, controls))
        {
            return;
        }
        self.seen = Some(controls.clone());
        self.ramps.resize(tracks.len(), GainRamp::at(1.0));
        self.audible.resize(tracks.len(), true);
        for (index, id) in tracks.iter().enumerate() {
            let mix = controls.iter().find(|m| m.id == *id);
            let audible = mix.is_none_or(|m| m.audible);
            let target = if audible {
                mix.map_or(1.0, |m| m.gain)
            } else {
                0.0
            };
            self.audible[index] = audible;
            if self.started {
                self.ramps[index].retarget(target, ramp);
            } else {
                self.ramps[index] = GainRamp::at(target);
            }
        }
        self.started = true;
    }

    fn has_slot(&self, span: usize) -> bool {
        self.slots.iter().any(|s| s.span == span)
    }

    /// Starts the decoders the next frames need, audible tracks first, inside
    /// the budgets. Spans are sorted by start, so the scan stops at the
    /// prefetch horizon.
    fn schedule(
        &mut self,
        backend: &Arc<dyn MediaBackend>,
        plan: &AudioPlan,
        spans: &[Span],
        k: u64,
        rate: u32,
        shared: &Shared,
    ) {
        let horizon = k + u64::from(rate) * PREFETCH_MS / 1000;
        let window = |first: usize| {
            spans[first..]
                .iter()
                .enumerate()
                .take_while(|(_, s)| s.s0 <= horizon)
                .map(move |(i, s)| (first + i, *s))
        };
        let wanted = |audible: bool, active: bool, this: &Self| {
            window(this.first)
                .filter(|(i, s)| {
                    s.s1 > k
                        && (s.s0 <= k) == active
                        && this.audible.get(s.track).copied().unwrap_or(true) == audible
                        && !this.has_slot(*i)
                        && !this.silent.contains(i)
                        && !this.refused.contains(i)
                })
                .collect::<Vec<_>>()
        };
        let audible_now = |this: &Self| {
            window(this.first)
                .filter(|(i, s)| {
                    s.s0 <= k
                        && s.s1 > k
                        && this.audible.get(s.track).copied().unwrap_or(true)
                        && this.has_slot(*i)
                })
                .count()
        };
        let start = |this: &mut Self, index: usize, span: Span, from: u64| {
            if let Some(slot) = start_slot(backend, plan, &span, index, from, rate) {
                this.slots.push(slot);
                let mut feed = shared.feed.lock().unwrap_or_else(|p| p.into_inner());
                feed.decoder_starts += 1;
                feed.mix.decoders_peak = feed.mix.decoders_peak.max(this.slots.len());
            } else if !this.silent.contains(&index) {
                this.silent.push(index);
            }
        };
        // A clip that is already under way when its decoder starts cannot be
        // joined at frame k: decode from a little ahead and join when it lands.
        let from = |span: &Span| {
            if k == 0 || span.s0 >= k {
                k.max(span.s0)
            } else {
                k + u64::from(rate) * LATE_JOIN_MS / 1000
            }
        };
        for (index, span) in wanted(true, true, self) {
            if audible_now(self) >= MAX_MIX_TRACKS || self.slots.len() >= MAX_DECODERS {
                self.refused.push(index);
                let mut feed = shared.feed.lock().unwrap_or_else(|p| p.into_inner());
                feed.mix.over_limit += 1;
                feed.detail = Some(format!(
                    "More than {MAX_MIX_TRACKS} audible tracks overlap; extra tracks are silent."
                ));
                continue;
            }
            if from(&span) < span.s1 {
                start(self, index, span, from(&span));
            }
        }
        for (index, span) in wanted(true, false, self) {
            if self.slots.len() < MAX_DECODERS {
                start(self, index, span, span.s0);
            }
        }
        // Warm standby for inaudible tracks, so unmuting is immediate.
        for (index, span) in wanted(false, true, self) {
            if self.slots.len() < MAX_MIX_TRACKS && from(&span) < span.s1 {
                start(self, index, span, from(&span));
            }
        }
    }
}

/// What one contribution does in the current block.
#[derive(Clone, Copy)]
struct Part {
    span: usize,
    slot: Option<usize>,
    /// Frames of PCM are summed from this slot.
    pcm: bool,
}

/// The feeder: mixes the epoch's timeline audio into the output ring.
fn feed(backend: &Arc<dyn MediaBackend>, output: &AudioOutput, shared: &Shared) {
    let rate = output.rate();
    let block = ((u64::from(rate) * BLOCK_MS / 1000) as usize).max(1);
    let ramp_frames = ((u64::from(rate) * GAIN_RAMP_MS / 1000) as u32).max(1);
    let lock = || shared.feed.lock().unwrap_or_else(|p| p.into_inner());
    let mut epoch_id = 0;
    let mut epoch = Epoch::default();
    let mut mix = vec![0.0f32; block * 2];
    let mut parts: Vec<Part> = Vec::with_capacity(MAX_DECODERS);
    let mut touched: Vec<bool> = Vec::new();
    loop {
        let (plan, spans, tracks, extent, written, controls) = {
            let mut feed = lock();
            if feed.shutdown {
                return;
            }
            if feed.epoch != epoch_id {
                epoch_id = feed.epoch;
                epoch = Epoch::default();
                feed.decoders = 0;
            }
            match (&feed.plan, feed.finished) {
                (Some(plan), false) => (
                    plan.clone(),
                    feed.spans.clone(),
                    feed.tracks.clone(),
                    feed.extent_frames,
                    feed.written,
                    feed.controls.clone(),
                ),
                _ => {
                    let _unused = shared
                        .wake
                        .wait_timeout(feed, Duration::from_millis(20))
                        .unwrap_or_else(|p| p.into_inner());
                    continue;
                }
            }
        };
        let k = written;
        epoch.apply_controls(&tracks, &controls, ramp_frames);
        epoch.slots.retain(|slot| spans[slot.span].s1 > k);
        while epoch.first < spans.len() && spans[epoch.first].s1 <= k {
            epoch.first += 1;
        }
        epoch.schedule(backend, &plan, &spans, k, rate, shared);
        if written >= extent {
            let mut feed = lock();
            if feed.epoch == epoch_id {
                feed.finished = true;
                output.finish();
            }
            continue;
        }
        let mut frames = output.mix_room().min(block).min((extent - k) as usize);
        if frames == 0 {
            thread::sleep(Duration::from_millis(2));
            continue;
        }
        let began = Instant::now();
        // The clips under frame k, in project track order; the block stops at
        // the next clip boundary so every part covers it entirely.
        parts.clear();
        for (index, span) in spans[epoch.first..].iter().enumerate() {
            let index = epoch.first + index;
            if span.s0 > k {
                frames = frames.min((span.s0 - k) as usize);
                break;
            }
            if span.s1 > k {
                frames = frames.min((span.s1 - k) as usize);
                let slot = epoch.slots.iter().position(|s| s.span == index);
                parts.push(Part {
                    span: index,
                    slot,
                    pcm: false,
                });
            }
        }
        parts.sort_by_key(|p| spans[p.span].track);
        // What each part can supply; the block shrinks to what all can.
        let mut wait = false;
        let mut silent_frames = false;
        for part in &mut parts {
            let span = spans[part.span];
            let audible = epoch.audible.get(span.track).copied().unwrap_or(true);
            let needed = k as i64 + span.offset;
            let Some(index) = part.slot else {
                if audible {
                    silent_frames = true;
                    if epoch.silent.contains(&part.span) && !epoch.reported.contains(&part.span) {
                        epoch.reported.push(part.span);
                        let reason = plan.clips[span.clip].unavailable.clone();
                        lock().detail = reason.or_else(|| {
                            Some("A clip's audio could not start; silence plays.".into())
                        });
                    }
                }
                continue;
            };
            let slot = &mut epoch.slots[index];
            // The first frame of a clip must be exact; afterwards a track that
            // is contributing keeps every track sample-aligned.
            let exact_start = audible && k == span.s0;
            match slot.peek(needed, frames) {
                Run::Pcm(run) => {
                    frames = frames.min(run);
                    part.pcm = true;
                }
                Run::Silent(run) => {
                    frames = frames.min(run);
                    silent_frames |= audible;
                    slot.joined |= exact_start;
                }
                Run::End(error) => {
                    silent_frames |= audible;
                    if let Some(error) = error {
                        lock().detail = Some(error);
                    }
                    // The rest of this clip is silence; the decoder is done.
                    slot.joined = true;
                }
                Run::Wait => {
                    if audible && (slot.joined || exact_start) {
                        wait = true;
                    } else {
                        silent_frames |= audible;
                    }
                }
            }
        }
        if wait {
            lock().decoders = epoch.slots.len();
            thread::sleep(Duration::from_millis(2));
            continue;
        }
        // Sum in project order. Tracks without PCM still advance their ramp.
        let out = &mut mix[..frames * 2];
        out.fill(0.0);
        touched.clear();
        touched.resize(epoch.ramps.len(), false);
        let mut mixed = 0;
        for part in &parts {
            let span = spans[part.span];
            touched[span.track] = true;
            let Some(ramp) = epoch.ramps.get_mut(span.track) else {
                continue;
            };
            match part.slot.filter(|_| part.pcm) {
                Some(index) => {
                    let slot = &mut epoch.slots[index];
                    slot.joined = true;
                    ramp.mix_into(slot.samples(k as i64 + span.offset, frames), out);
                    mixed += 1;
                }
                None => ramp.skip(frames),
            }
        }
        for (index, ramp) in epoch.ramps.iter_mut().enumerate() {
            if !touched[index] {
                ramp.skip(frames);
            }
        }
        let (clipped, peak) = clip_mix(out);
        let spent = began.elapsed().as_micros().min(u128::from(u64::MAX)) as u64;
        let mut feed = lock();
        if feed.epoch != epoch_id {
            continue;
        }
        // Space was reserved above and only this thread pushes.
        let accepted = output.push(out);
        debug_assert_eq!(accepted, frames);
        feed.written += accepted as u64;
        feed.decoders = epoch.slots.len();
        if silent_frames {
            feed.source_silence += accepted as u64;
        }
        feed.silent_clips = epoch.reported.len() as u64;
        let stats = &mut feed.mix;
        stats.active_tracks = parts.len();
        stats.mixed_tracks = mixed;
        stats.mixed_tracks_peak = stats.mixed_tracks_peak.max(mixed);
        stats.clipped_samples += clipped;
        stats.peak = stats.peak.max(peak);
        stats.blocks += 1;
        stats.block_us_last = spent;
        stats.block_us_max = stats.block_us_max.max(spent);
        stats.block_us_total += spent;
    }
}

/// Every audio track of a timeline plan, in project order.
pub fn plan_for(plan: &crate::timeline_plan::TimelinePlan) -> AudioPlan {
    let clips = plan
        .index
        .audio_tracks()
        .flat_map(|track| plan.index.clips(track.id).iter().cloned())
        .map(|clip| {
            let media = plan.media.get(&clip.media_id).cloned();
            let ready = plan
                .availability
                .get(&clip.media_id)
                .is_some_and(|a| a.state == Availability::Ready);
            let decodable = match clip.stream {
                ClipStream::Audio { index } => media
                    .as_ref()
                    .is_some_and(|m| m.metadata.audio_stream(index).is_some()),
                ClipStream::Video => false,
            };
            let unavailable = if !ready {
                Some("The audio source is missing or changed; silence plays.".to_owned())
            } else if !decodable {
                Some("This audio stream cannot be decoded; silence plays.".to_owned())
            } else {
                None
            };
            AudioClip {
                clip,
                media,
                unavailable,
            }
        })
        .collect();
    AudioPlan {
        timebase: plan.timebase,
        clips,
        extent: plan.extent,
    }
}

#[cfg(test)]
#[path = "audio_transport_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "audio_mixer_tests.rs"]
mod mixer_tests;
