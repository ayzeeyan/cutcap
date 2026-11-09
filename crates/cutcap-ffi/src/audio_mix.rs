//! Device-independent mixing shared by live playback and offline export:
//! the exact sample layout of clips, per-track audibility and gain, summing,
//! clipping, and the bounded per-clip PCM decoders. Nothing here knows about an
//! output device or a clock (PHASE_4C_AUDIO_MIXING_CONTRACT.md).
use cutcap_contracts::*;
use cutcap_core::{
    Clip, ClipStream, TimelineIndex, samples_until, source_sample_offset, timeline_at_sample,
};
use cutcap_media::{Cancellation, MediaBackend, MediaError, PcmChunk, PcmRequest};
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        mpsc::{self, Receiver},
    },
    thread::{self, JoinHandle},
};

/// Audible tracks that may have overlapping clips.
pub const MAX_MIX_TRACKS: usize = 16;
/// Decoder processes at once: every active track plus bounded prefetch.
pub const MAX_DECODERS: usize = MAX_MIX_TRACKS + 8;
/// Decoded PCM queued per decoder: 24 chunks of at most [`MAX_CHUNK_FRAMES`].
pub(crate) const SLOT_QUEUE: usize = 24;
pub(crate) const MAX_CHUNK_FRAMES: usize = 4096;

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
pub(crate) struct GainRamp {
    pub(crate) current: f32,
    pub(crate) target: f32,
    pub(crate) step: f32,
}

impl GainRamp {
    pub(crate) fn at(gain: f32) -> Self {
        Self {
            current: gain,
            target: gain,
            step: 0.0,
        }
    }
    pub(crate) fn retarget(&mut self, target: f32, frames: u32) {
        if target != self.target {
            self.target = target;
            self.step = (target - self.current) / frames.max(1) as f32;
        }
    }
    pub(crate) fn advance(&mut self) {
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
    pub(crate) fn skip(&mut self, frames: usize) {
        for _ in 0..frames {
            if self.current == self.target {
                break;
            }
            self.advance();
        }
    }
    /// Adds `samples` (interleaved stereo) to `out`, scaled by this ramp.
    pub(crate) fn mix_into(&mut self, samples: &[f32], out: &mut [f32]) {
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
pub(crate) fn clip_mix(out: &mut [f32]) -> (u64, f32) {
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

/// An epoch's exact sample layout of one clip, in output frames since `P0`.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Span {
    pub(crate) s0: u64,
    pub(crate) s1: u64,
    pub(crate) offset: i64,
    pub(crate) clip: usize,
    /// Index into the epoch's tracks, in project order.
    pub(crate) track: usize,
}

pub(crate) struct Slot {
    pub(crate) span: usize,
    pub(crate) rx: Receiver<PcmChunk>,
    pub(crate) cancel: Cancellation,
    pub(crate) done: Arc<Mutex<Option<Result<(), MediaError>>>>,
    pub(crate) worker: Option<JoinHandle<()>>,
    pub(crate) pending: Option<PcmChunk>,
    /// This slot's PCM has reached the mix.
    pub(crate) joined: bool,
}

/// What a decoder can supply at one position.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Run {
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
    pub(crate) fn peek(&mut self, needed: i64, max: usize) -> Run {
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
    pub(crate) fn samples(&self, needed: i64, frames: usize) -> &[f32] {
        self.pending.as_ref().map_or(&[], |chunk| {
            let at = (needed - chunk.first_sample) as usize * 2;
            &chunk.samples[at..at + frames * 2]
        })
    }
    /// Waits briefly for the next chunk (offline mixing only; live playback
    /// never blocks here).
    pub(crate) fn wait_chunk(&mut self, timeout: std::time::Duration) {
        if self.pending.is_none()
            && let Ok(chunk) = self.rx.recv_timeout(timeout)
        {
            self.pending = Some(chunk);
        }
    }
    pub(crate) fn stop(&mut self) {
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

/// Exact epoch layout: each clip's output-frame interval and source offset,
/// the extent in frames, and the tracks in first-appearance (project) order.
pub(crate) fn layout(
    plan: &AudioPlan,
    origin: TimelineTime,
    rate: u32,
) -> (Vec<Span>, u64, Vec<TrackId>) {
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

pub(crate) fn start_slot(
    backend: &Arc<dyn MediaBackend>,
    plan: &AudioPlan,
    span: &Span,
    index: usize,
    from_frame: u64,
    rate: u32,
    background: bool,
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
        background,
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
pub(crate) fn finished(slot: &Slot) -> Option<Result<(), String>> {
    let done = slot.done.lock().ok()?;
    match done.as_ref()? {
        Ok(()) | Err(MediaError::NoFrame) => Some(Ok(())),
        Err(error) => Some(Err(error.to_string())),
    }
}

/// Every audio track's clips, in project order, with what playback needs.
pub fn audio_plan(
    timebase: Timebase,
    extent: TimelineTime,
    index: &TimelineIndex,
    media: &BTreeMap<MediaId, MediaSummary>,
    availability: &BTreeMap<MediaId, MediaAvailability>,
) -> AudioPlan {
    let clips = index
        .audio_tracks()
        .flat_map(|track| index.clips(track.id).iter().cloned())
        .map(|clip| {
            let summary = media.get(&clip.media_id).cloned();
            let ready = availability
                .get(&clip.media_id)
                .is_some_and(|a| a.state == Availability::Ready);
            let decodable = match clip.stream {
                ClipStream::Audio { index } => summary
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
                media: summary,
                unavailable,
            }
        })
        .collect();
    AudioPlan {
        timebase,
        clips,
        extent,
    }
}

/// The first point where more audible clips overlap than the mixer supports.
pub(crate) fn overlap_error(
    spans: &[Span],
    audible: impl Fn(usize) -> bool,
    plan: &AudioPlan,
    origin: TimelineTime,
    rate: u32,
) -> Result<(), String> {
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
            let at = timeline_at_sample(origin, frame, plan.timebase, rate).unwrap_or(origin);
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
