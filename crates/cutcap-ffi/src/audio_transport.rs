//! Phase 4B audio transport: the monitored audio track rendered sample by
//! sample into the output ring, and the audio-master clock.
//!
//! Policy (see the Phase 4B report): when an output device is open, timeline
//! playback is clocked by audio. Output sample `k` of an epoch that starts at
//! timeline position `P0` is presented at `P0 + k / rate`. Inside an audio
//! clip it plays source sample `k + offset` (one ceiling rounding per clip,
//! `cutcap_core::source_sample_offset`); between clips and wherever the
//! source has no samples it writes silence. Real samples are never invented
//! and nothing is resampled in time. The clock is the device's played
//! position mapped back through the output's segment map, so it advances only
//! while timeline audio is actually being played.
//!
//! One feeder thread owns up to two decoder slots (the current clip and the
//! next one, prefetched); each slot is an FFmpeg process streaming PCM into a
//! bounded queue. A seek, edit or stop starts a new epoch: decoders are
//! cancelled and the output flushed, so stale audio can never leak.
use crate::audio_output::{AudioOutput, OutputStatus};
use cutcap_contracts::*;
use cutcap_core::{Clip, ClipStream, samples_until, source_sample_offset, timeline_at_sample};
use cutcap_media::{Cancellation, MediaBackend, MediaError, PcmChunk, PcmRequest};
use serde::Serialize;
use std::{
    collections::VecDeque,
    sync::{
        Arc, Condvar, Mutex,
        mpsc::{self, Receiver},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

/// Decoded PCM queued per decoder slot: 24 chunks (about 0.5 s at 48 kHz).
const SLOT_QUEUE: usize = 24;
/// The next clip's decoder starts this long before its first sample.
const PREFETCH_MS: u64 = 1500;
/// Most frames written per feeder step.
const STEP_FRAMES: usize = 4096;

/// One clip of the monitored audio track, with what playback needs.
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
}

/// An epoch's exact sample layout of one clip, in output frames since `P0`.
#[derive(Clone, Copy, Debug)]
struct Span {
    s0: u64,
    s1: u64,
    offset: i64,
    clip: usize,
}

struct Slot {
    span: usize,
    rx: Receiver<PcmChunk>,
    cancel: Cancellation,
    done: Arc<Mutex<Option<Result<(), MediaError>>>>,
    worker: Option<JoinHandle<()>>,
    pending: Option<PcmChunk>,
}
impl Slot {
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
    extent_frames: u64,
    written: u64,
    finished: bool,
    shutdown: bool,
    decoders: usize,
    decoder_starts: u64,
    source_silence: u64,
    silent_clips: u64,
    detail: Option<String>,
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

    /// Starts a new epoch at `position`: decoders restart, the output is
    /// flushed and the device position restarts at zero.
    pub fn begin(&mut self, plan: AudioPlan, position: TimelineTime) {
        let (spans, extent_frames) = layout(&plan, position, self.rate);
        let mut feed = self.feed();
        feed.epoch += 1;
        feed.origin = position;
        feed.spans = Arc::new(spans);
        feed.extent_frames = extent_frames;
        feed.written = 0;
        feed.finished = false;
        feed.source_silence = 0;
        feed.silent_clips = 0;
        feed.detail = None;
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

/// Exact epoch layout: each clip's output-frame interval and source offset.
fn layout(plan: &AudioPlan, origin: TimelineTime, rate: u32) -> (Vec<Span>, u64) {
    let frames = |position: TimelineTime| {
        samples_until(origin, position, plan.timebase, rate).unwrap_or(i64::MAX)
    };
    let mut spans = Vec::new();
    for (index, entry) in plan.clips.iter().enumerate() {
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
        });
    }
    spans.sort_by_key(|s| s.s0);
    (spans, frames(plan.extent).max(0) as u64)
}

fn start_slot(
    backend: &Arc<dyn MediaBackend>,
    plan: &AudioPlan,
    spans: &[Span],
    span: usize,
    from_frame: u64,
    rate: u32,
) -> Option<Slot> {
    let s = spans[span];
    let entry = &plan.clips[s.clip];
    let (Some(media), None, ClipStream::Audio { index }) =
        (entry.media.clone(), &entry.unavailable, entry.clip.stream)
    else {
        return None;
    };
    let request = PcmRequest {
        media,
        stream_index: index,
        start_sample: from_frame.max(s.s0) as i64 + s.offset,
        end_sample: Some(s.s1 as i64 + s.offset),
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
                    tx.send(chunk).map_err(|_| MediaError::Cancelled)
                });
                if let Ok(mut slot) = done.lock() {
                    *slot = Some(result);
                }
            })
            .ok()?
    };
    Some(Slot {
        span,
        rx,
        cancel,
        done,
        worker: Some(worker),
        pending: None,
    })
}

/// What one feeder step decided, applied under the feed lock.
enum Step {
    Wait,
    /// Frames of silence; `true` when the source itself has no samples there.
    Silence(usize, bool),
    /// Real decoded samples (interleaved stereo) for the current frames.
    Samples(Vec<f32>),
    Finish,
}

/// Outcome of a decoder that has stopped: `Some(Ok)` = no more samples.
fn finished(slot: &Slot) -> Option<Result<(), String>> {
    let done = slot.done.lock().ok()?;
    match done.as_ref()? {
        Ok(()) | Err(MediaError::NoFrame) => Some(Ok(())),
        Err(error) => Some(Err(error.to_string())),
    }
}

/// The feeder: writes the epoch's timeline audio into the output ring.
fn feed(backend: &Arc<dyn MediaBackend>, output: &AudioOutput, shared: &Shared) {
    let rate = output.rate();
    let lock = || shared.feed.lock().unwrap_or_else(|p| p.into_inner());
    let mut epoch = 0;
    let mut slots: VecDeque<Slot> = VecDeque::new();
    let mut silent: Vec<usize> = Vec::new();
    loop {
        let (plan, spans, extent, written) = {
            let mut feed = lock();
            if feed.shutdown {
                return;
            }
            if feed.epoch != epoch {
                epoch = feed.epoch;
                slots.clear();
                silent.clear();
                feed.decoders = 0;
            }
            match (&feed.plan, feed.finished) {
                (Some(plan), false) => (
                    plan.clone(),
                    feed.spans.clone(),
                    feed.extent_frames,
                    feed.written,
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
        let space = output.space().min(STEP_FRAMES);
        let k = written;
        // Retire decoders whose clip is behind; prefetch what is about to play.
        slots.retain(|slot| spans[slot.span].s1 > k);
        let horizon = k + u64::from(rate) * PREFETCH_MS / 1000;
        for (index, span) in spans.iter().enumerate() {
            if slots.len() < 2
                && span.s1 > k
                && span.s0 <= horizon
                && !slots.iter().any(|s| s.span == index)
                && let Some(slot) = start_slot(backend, &plan, &spans, index, k, rate)
            {
                slots.push_back(slot);
                lock().decoder_starts += 1;
            }
        }
        let step = if written >= extent {
            Step::Finish
        } else if space == 0 {
            Step::Wait
        } else {
            match spans.iter().position(|s| s.s0 <= k && k < s.s1) {
                None => {
                    // A gap between clips: silence up to the next clip or the end.
                    let next = spans
                        .iter()
                        .map(|s| s.s0)
                        .filter(|s0| *s0 > k)
                        .min()
                        .unwrap_or(extent)
                        .min(extent);
                    Step::Silence((next - k).min(space as u64) as usize, false)
                }
                Some(index) => {
                    let span = spans[index];
                    let remaining = (span.s1 - k).min(space as u64) as usize;
                    match slots.iter_mut().find(|s| s.span == index) {
                        None => {
                            // Missing source or no decodable stream: the clip is silent.
                            if !silent.contains(&index) {
                                silent.push(index);
                                let mut feed = lock();
                                feed.silent_clips += 1;
                                feed.detail =
                                    plan.clips[span.clip].unavailable.clone().or_else(|| {
                                        Some(
                                            "A clip's audio could not start; silence plays.".into(),
                                        )
                                    });
                            }
                            Step::Silence(remaining, true)
                        }
                        Some(slot) => {
                            let needed = k as i64 + span.offset;
                            let chunk = slot.pending.take().or_else(|| slot.rx.try_recv().ok());
                            match chunk {
                                None => match finished(slot) {
                                    // The stream has no more samples inside this clip.
                                    Some(Ok(())) => Step::Silence(remaining, true),
                                    Some(Err(error)) => {
                                        lock().detail = Some(error);
                                        Step::Silence(remaining, true)
                                    }
                                    // Still decoding: write nothing; the output pads if it must.
                                    None => Step::Wait,
                                },
                                Some(chunk) if chunk.end_sample() <= needed => Step::Wait,
                                Some(chunk) if chunk.first_sample > needed => {
                                    // A real gap in the source: silence until its next sample.
                                    let gap =
                                        ((chunk.first_sample - needed) as usize).min(remaining);
                                    slot.pending = Some(chunk);
                                    Step::Silence(gap, true)
                                }
                                Some(mut chunk) => {
                                    let skip = (needed - chunk.first_sample) as usize;
                                    let take = (chunk.frames() - skip).min(remaining);
                                    let samples =
                                        chunk.samples[skip * 2..(skip + take) * 2].to_vec();
                                    chunk.samples.drain(..(skip + take) * 2);
                                    chunk.first_sample = needed + take as i64;
                                    if !chunk.samples.is_empty() {
                                        slot.pending = Some(chunk);
                                    }
                                    Step::Samples(samples)
                                }
                            }
                        }
                    }
                }
            }
        };
        let mut feed = lock();
        if feed.epoch != epoch {
            continue;
        }
        feed.decoders = slots.len();
        match step {
            Step::Wait => {
                drop(feed);
                thread::sleep(Duration::from_millis(2));
            }
            Step::Finish => {
                feed.finished = true;
                output.finish();
            }
            Step::Silence(frames, source) => {
                let written = output.push_silence(frames);
                feed.written += written as u64;
                if source {
                    feed.source_silence += written as u64;
                }
            }
            Step::Samples(samples) => {
                // Space was reserved above and only this thread pushes.
                let written = output.push(&samples);
                debug_assert_eq!(written * 2, samples.len());
                feed.written += written as u64;
            }
        }
    }
}

/// The plan for the monitored audio track of a timeline plan.
pub fn plan_for(plan: &crate::timeline_plan::TimelinePlan) -> AudioPlan {
    let clips = plan
        .index
        .monitored_audio()
        .map(|track| plan.index.clips(track.id).to_vec())
        .unwrap_or_default()
        .into_iter()
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
