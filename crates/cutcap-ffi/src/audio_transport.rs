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
pub use crate::audio_mix::*;
use crate::audio_output::{AudioOutput, OutputStatus};
use cutcap_contracts::*;
use cutcap_core::timeline_at_sample;
use cutcap_media::MediaBackend;
use serde::Serialize;
use std::{
    sync::{Arc, Condvar, Mutex},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

/// A clip's decoder starts this long before its first sample.
const PREFETCH_MS: u64 = 1500;
/// A track made audible mid-clip decodes from this far ahead of the mix.
const LATE_JOIN_MS: u64 = 300;
/// Most audio mixed per step; controls are read once per block.
const BLOCK_MS: u64 = 10;
/// Length of the linear ramp applied when a track's gain changes.
pub const GAIN_RAMP_MS: u64 = 10;

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
        overlap_error(&spans, audible, plan, position, self.rate)
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
            if let Some(slot) = start_slot(backend, plan, &span, index, from, rate, false) {
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
    audio_plan(
        plan.timebase,
        plan.extent,
        &plan.index,
        &plan.media,
        &plan.availability,
    )
}

#[cfg(test)]
#[path = "audio_transport_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "audio_mixer_tests.rs"]
mod mixer_tests;
