//! Single-track video transport. No project mutations, I/O, waits or FPS clock.
use crate::{
    audio_transport::{AudioTransport, AudioView},
    media::{MediaHost, PresentedFrame},
    timeline_plan::TimelinePlan,
    timeline_preview::TimelinePreviewView,
    view,
};
use cutcap_contracts::*;
use cutcap_core::{Clip, SourceMapping, frame_timeline_time, map_clip_source};
use cutcap_media::*;
use serde::Serialize;
use std::{collections::VecDeque, sync::Arc, time::Instant};

#[cfg(test)]
#[path = "timeline_playback_tests.rs"]
mod tests;

#[derive(Debug, Clone, Serialize)]
pub struct TimelinePlaybackView {
    pub state: &'static str,
    pub desired_playing: bool,
    pub epoch: Option<String>,
    pub requested_ticks: i64,
    pub requested_text: String,
    pub clock_ticks: i64,
    pub clock_ms: i64,
    pub clock_text: String,
    pub offered_ticks: Option<i64>,
    pub copied_ticks: Option<i64>,
    pub offered_pts: Option<SourceTime>,
    pub copied_pts: Option<SourceTime>,
    pub copied_clip_id: Option<String>,
    pub decoded: u64,
    pub offered: u64,
    pub copied: u64,
    pub dropped: u64,
    pub buffer_stalls: u64,
    pub handoffs: u64,
    pub decoder_starts: u64,
    pub buffered: usize,
    pub process_id: Option<u32>,
    pub prepared_process_id: Option<u32>,
    pub error: Option<String>,
    pub request_to_ready_us: Option<u64>,
    /// Phase 4B: "audio" when the output device clocks playback, else "system".
    pub clock_source: &'static str,
    /// Times video could not keep up with the audio master (audio kept playing).
    pub video_late: u64,
    /// Presented frame position minus the audio clock when the frame was
    /// copied (negative: the picture trails the sound), and the largest |offset|.
    pub av_offset_ms: Option<f64>,
    pub av_offset_max_ms: f64,
    pub av_samples: u64,
}
impl Default for TimelinePlaybackView {
    fn default() -> Self {
        Self {
            state: "stopped",
            desired_playing: false,
            epoch: None,
            requested_ticks: 0,
            requested_text: "00:00.000".into(),
            clock_ticks: 0,
            clock_ms: 0,
            clock_text: "00:00.000".into(),
            offered_ticks: None,
            copied_ticks: None,
            offered_pts: None,
            copied_pts: None,
            copied_clip_id: None,
            decoded: 0,
            offered: 0,
            copied: 0,
            dropped: 0,
            buffer_stalls: 0,
            handoffs: 0,
            decoder_starts: 0,
            buffered: 0,
            process_id: None,
            prepared_process_id: None,
            error: None,
            request_to_ready_us: None,
            clock_source: "system",
            video_late: 0,
            av_offset_ms: None,
            av_offset_max_ms: 0.0,
            av_samples: 0,
        }
    }
}

/// Checked monotonic clock in project ticks. The anchor is never a source FPS.
struct TimelineClock {
    base: TimelineTime,
    anchor: Option<Instant>,
    timebase: Timebase,
}
impl Default for TimelineClock {
    fn default() -> Self {
        Self {
            base: TimelineTime::ZERO,
            anchor: None,
            timebase: Timebase::DEFAULT_TIMELINE,
        }
    }
}
impl TimelineClock {
    fn at(&self, now: Instant) -> Result<TimelineTime, TimeError> {
        let Some(anchor) = self.anchor else {
            return Ok(self.base);
        };
        let nanos = i128::try_from(now.saturating_duration_since(anchor).as_nanos())
            .map_err(|_| TimeError::Overflow)?;
        let ticks = nanos
            .checked_mul(i128::from(self.timebase.denominator()))
            .ok_or(TimeError::Overflow)?
            / (1_000_000_000 * i128::from(self.timebase.numerator()));
        Ok(TimelineTime(
            self.base
                .0
                .checked_add(i64::try_from(ticks).map_err(|_| TimeError::Overflow)?)
                .ok_or(TimeError::Overflow)?,
        ))
    }
    fn freeze(&mut self, position: TimelineTime) {
        self.base = position;
        self.anchor = None;
    }
    fn resume(&mut self, now: Instant) {
        if self.anchor.is_none() {
            self.anchor = Some(now);
        }
    }
}

struct TimedFrame {
    position: TimelineTime,
    frame: DecodedFrame,
}
struct Slot {
    worker: usize,
    clip: Clip,
    ticket: PreviewTicket,
    requested: SourceTime,
    bounds: SourceMapping,
    pending: VecDeque<TimedFrame>,
    finished: bool,
    eof: bool,
    error: Option<String>,
    decoded: u64,
    pid: Option<u32>,
}
impl Slot {
    fn ready(&self) -> bool {
        self.error.is_some() || self.eof || self.pending.len() >= 2
    }
    fn poll(&mut self, decoder: &PersistentDecoder, clock: Timebase) -> Result<(), MediaError> {
        if self.eof || self.error.is_some() {
            return Ok(());
        }
        while self.pending.len() < 2 {
            let Some(frame) = decoder.take()? else {
                break;
            };
            if frame.ticket != self.ticket
                || frame.requested != self.requested
                || frame.actual.timebase != self.bounds.requested.timebase
            {
                return Err(MediaError::Invalid(
                    "timeline frame identity/clock mismatch".into(),
                ));
            }
            if frame.actual.ticks >= self.bounds.end_exclusive.ticks {
                self.eof = true;
                decoder.cancel()?;
                break;
            }
            if !self
                .bounds
                .accepts(frame.actual)
                .map_err(|e| MediaError::Invalid(e.to_string()))?
            {
                return Err(MediaError::Invalid(
                    "timeline frame outside clip source range".into(),
                ));
            }
            let position = match frame_timeline_time(&self.clip, frame.actual, clock) {
                Ok(p) => p,
                // Rounding can put the final fractional source tick at clip end.
                Err(cutcap_core::EvaluationError::OutsideClip) => {
                    self.eof = true;
                    decoder.cancel()?;
                    break;
                }
                Err(e) => return Err(MediaError::Invalid(e.to_string())),
            };
            self.pending.push_back(TimedFrame { position, frame });
        }
        let progress = decoder.progress()?;
        self.decoded = self.decoded.max(progress.decoded);
        self.pid = progress.process_id.or(self.pid);
        // Errors are reliable even while paused with future pixels queued.
        // Successful completion becomes EOF only after the worker queue drains.
        if let Some(result) = decoder.terminal()? {
            match result {
                Ok(()) | Err(MediaError::NoFrame) => self.finished = true,
                Err(e) => self.error = Some(e.to_string()),
            }
        }
        // Sample after terminal publication: the producer may have pushed its
        // final frame between the earlier progress read and terminal read.
        if self.finished {
            self.eof = decoder.progress()?.buffered == 0;
        }
        Ok(())
    }
}

pub struct TimelinePlayback {
    workers: [PersistentDecoder; 2],
    plan: Option<TimelinePlan>,
    epoch: RequestId,
    active: Option<Slot>,
    prepared: Option<Slot>,
    clock: TimelineClock,
    position: TimelineTime,
    pub view: TimelinePlaybackView,
    pub frame: Option<PresentedFrame>,
    frame_clip: Option<ClipId>,
    copied_generation: Option<u64>,
    starting: bool,
    queued: Instant,
    completed_decoded: u64,
    /// Phase 4B audio master (None: no output device, or tests).
    audio: Option<AudioTransport>,
    /// Audio is clocking the current epoch.
    audio_master: bool,
    /// Why there is no audio master (no device, failed, disabled).
    audio_note: Option<String>,
    video_late_episode: bool,
}
impl TimelinePlayback {
    #[cfg(test)]
    pub fn start(backend: Arc<dyn MediaBackend>) -> Result<Self, std::io::Error> {
        Self::start_with_audio(backend, None, None)
    }
    /// Video transport plus an optional audio master. `note` explains a
    /// missing master in the view (for example "No audio output device").
    pub fn start_with_audio(
        backend: Arc<dyn MediaBackend>,
        audio: Option<AudioTransport>,
        note: Option<String>,
    ) -> Result<Self, std::io::Error> {
        Ok(Self {
            audio,
            audio_master: false,
            audio_note: note,
            video_late_episode: false,
            workers: [
                PersistentDecoder::start(backend.clone())?,
                PersistentDecoder::start(backend)?,
            ],
            plan: None,
            epoch: RequestId::new(),
            active: None,
            prepared: None,
            clock: TimelineClock::default(),
            position: TimelineTime::ZERO,
            view: TimelinePlaybackView::default(),
            frame: None,
            frame_clip: None,
            copied_generation: None,
            starting: false,
            queued: Instant::now(),
            completed_decoded: 0,
        })
    }
    pub fn active(&self) -> bool {
        self.plan.is_some()
    }
    pub fn position(&self) -> TimelineTime {
        self.position
    }
    pub fn timebase(&self) -> Timebase {
        self.clock.timebase
    }
    pub fn valid_scope(&self, project: ProjectId, generation: RequestId) -> bool {
        self.plan
            .as_ref()
            .is_some_and(|p| p.project == project && p.generation == generation)
    }
    pub fn restore_frame_generation(&mut self, media: &mut MediaHost) {
        if let Some(frame) = &mut self.frame {
            frame.generation = media.next_generation();
            self.copied_generation = None;
            self.view.offered += 1;
        }
    }
    pub fn begin(
        &mut self,
        plan: TimelinePlan,
        playing: bool,
        seeking: bool,
        now: Instant,
    ) -> Result<(), MediaError> {
        self.cancel()?;
        self.epoch = RequestId::new();
        self.position = plan.position.min(plan.extent);
        self.clock.timebase = plan.timebase;
        self.clock.freeze(self.position);
        self.view = TimelinePlaybackView {
            state: if seeking { "seeking" } else { "starting" },
            desired_playing: playing,
            epoch: Some(self.epoch.to_string()),
            requested_ticks: self.position.0,
            requested_text: exact_input(self.position, plan.timebase),
            ..Default::default()
        };
        self.starting = true;
        self.queued = now;
        self.video_late_episode = false;
        // The audio master follows every epoch; a failed device falls back to
        // the system clock (video only) until a new transport is opened.
        self.audio_master = false;
        if let Some(audio) = &mut self.audio {
            if let Some(error) = audio.failed() {
                self.audio_note = Some(format!("Audio output unavailable: {error}"));
            } else {
                audio.begin(crate::audio_transport::plan_for(&plan), self.position);
                self.audio_master = true;
            }
        }
        self.view.clock_source = if self.audio_master { "audio" } else { "system" };
        self.plan = Some(plan);
        Ok(())
    }

    /// The audio master's position (frozen at `position` until the device
    /// reports one, and while paused: a real device still plays the period in
    /// flight when it is stopped, which must not move a paused playhead);
    /// `None` when the system clock is in charge.
    fn audio_position(&self, now: Instant) -> Option<TimelineTime> {
        self.audio_master.then(|| {
            if !self.view.desired_playing {
                return self.position;
            }
            self.audio
                .as_ref()
                .and_then(|a| a.clock(now))
                .unwrap_or(self.position)
        })
    }

    pub fn audio_view(&self) -> Option<AudioView> {
        let mut view = self
            .audio
            .as_ref()
            .map(AudioTransport::view)
            .unwrap_or_else(|| AudioView {
                state: "unavailable",
                clock: "system",
                output: crate::audio_output::OutputStatus {
                    state: "unavailable",
                    ..Default::default()
                },
                ..Default::default()
            });
        if !self.audio_master {
            view.clock = "system";
        }
        if view.detail.is_none() {
            view.detail = self.audio_note.clone();
        }
        Some(view)
    }

    fn note_video_late(&mut self) {
        if !self.video_late_episode {
            self.video_late_episode = true;
            self.view.video_late += 1;
        }
    }
    pub fn pause(&mut self, now: Instant) -> Result<(), TimeError> {
        let at = match self.audio_position(now) {
            Some(position) => position,
            None => self.clock.at(now)?,
        };
        if self.audio_master
            && let Some(audio) = &mut self.audio
        {
            audio.pause();
        }
        self.position = at.min(self.plan.as_ref().map_or(self.position, |p| p.extent));
        // A control between pumps cannot authorize an image past an unprocessed cut.
        if let Some(slot) = &self.active {
            let end = slot.clip.end()?;
            self.position = self.position.min(end);
            if self.position == end && self.plan.as_ref().is_some_and(|p| end < p.extent) {
                self.clear_image();
            }
        }
        self.clock.freeze(self.position);
        self.view.desired_playing = false;
        if self.active() && !matches!(self.view.state, "ended" | "failed") {
            self.view.state = "paused";
        }
        self.update_position();
        Ok(())
    }
    pub fn resume(&mut self, now: Instant) {
        self.view.desired_playing = true;
        if self.starting {
            self.view.state = "starting";
        }
        if !self.starting && self.view.state == "paused" && self.audio_master {
            // The audio master resumes the device; late video catches up.
            if let Some(audio) = &mut self.audio {
                audio.play();
            }
            self.clock.resume(now);
            self.view.state = "playing";
            return;
        }
        if !self.starting && self.view.state == "paused" {
            if self
                .active
                .as_ref()
                .is_some_and(|s| s.pending.is_empty() && !s.eof)
            {
                self.view.state = "buffering";
                self.view.buffer_stalls += 1;
            } else {
                self.clock.resume(now);
                self.view.state = "playing";
            }
        }
    }
    pub fn cancel(&mut self) -> Result<(), MediaError> {
        for worker in &self.workers {
            worker.cancel()?;
        }
        if let Some(audio) = &mut self.audio {
            audio.cancel();
        }
        self.audio_master = false;
        self.plan = None;
        self.active = None;
        self.prepared = None;
        self.frame = None;
        self.frame_clip = None;
        self.copied_generation = None;
        self.completed_decoded = 0;
        self.clock.freeze(self.position);
        self.view.state = "stopped";
        self.view.desired_playing = false;
        self.view.epoch = None;
        self.view.error = None;
        self.view.buffered = 0;
        self.view.offered_ticks = None;
        self.view.offered_pts = None;
        self.view.process_id = None;
        self.view.prepared_process_id = None;
        Ok(())
    }
    pub fn stop(&mut self) -> Result<(), MediaError> {
        self.cancel()?;
        self.position = TimelineTime::ZERO;
        self.clock.freeze(self.position);
        self.view = TimelinePlaybackView::default();
        Ok(())
    }
    fn new_slot(
        &mut self,
        clip: Clip,
        position: TimelineTime,
        worker: usize,
    ) -> Result<Slot, MediaError> {
        let plan = self.plan.as_ref().ok_or(MediaError::Worker)?;
        let media = plan.media.get(&clip.media_id);
        let available = plan.availability.get(&clip.media_id);
        let ticket = PreviewTicket {
            project_id: plan.project,
            media_id: clip.media_id,
            request_id: RequestId::new(),
        };
        let mapped = media
            .ok_or_else(|| "Timeline source metadata is missing".to_string())
            .and_then(|media| {
                let bounds =
                    map_clip_source(&clip, clip.start, plan.timebase, media.metadata.timebase)
                        .map_err(|e| e.to_string())?;
                let requested =
                    map_clip_source(&clip, position, plan.timebase, media.metadata.timebase)
                        .map_err(|e| e.to_string())?
                        .requested;
                Ok((bounds, requested))
            });
        let (bounds, requested, mapping_error) = match mapped {
            Ok((bounds, requested)) => (bounds, requested, None),
            Err(error) => (
                SourceMapping {
                    requested: clip.source_in,
                    end_exclusive: clip.source_in,
                },
                clip.source_in,
                Some(error),
            ),
        };
        let error = (!available.is_some_and(|s| s.state == Availability::Ready))
            .then(|| available.map_or_else(|| "Source unavailable".into(), |s| s.detail.clone()));
        let mut slot = Slot {
            worker,
            clip,
            ticket,
            requested,
            bounds,
            pending: VecDeque::new(),
            finished: false,
            eof: false,
            error: mapping_error.or(error),
            decoded: 0,
            pid: None,
        };
        if slot.error.is_none() {
            if requested.ticks >= bounds.end_exclusive.ticks {
                slot.eof = true;
            } else {
                let result = self
                    .workers
                    .get(worker)
                    .ok_or(MediaError::Worker)?
                    .request_interval(
                        DecodeRequest {
                            ticket,
                            media: media.cloned().ok_or(MediaError::Worker)?,
                            position: requested,
                        },
                        bounds.requested,
                    );
                if let Err(error) = result {
                    slot.error = Some(error.to_string());
                }
                self.view.decoder_starts += 1;
            }
        }
        Ok(slot)
    }
    fn retire(&mut self, slot: Slot) -> Result<(), MediaError> {
        self.completed_decoded = self.completed_decoded.saturating_add(slot.decoded);
        self.workers
            .get(slot.worker)
            .ok_or(MediaError::Worker)?
            .cancel()
    }
    fn clear_image(&mut self) {
        self.frame = None;
        self.frame_clip = None;
        self.view.offered_ticks = None;
        self.view.offered_pts = None;
    }
    fn fail(&mut self, message: String) {
        if let Some(audio) = &mut self.audio {
            audio.cancel();
        }
        self.audio_master = false;
        self.clock.freeze(self.position);
        self.view.state = "failed";
        self.view.desired_playing = false;
        self.view.error = Some(message);
        self.clear_image();
        for worker in &self.workers {
            if let Err(e) = worker.cancel() {
                tracing::error!("timeline failure cleanup: {e}");
            }
        }
    }
    pub fn rejected(&mut self, message: String) {
        self.fail(message);
    }
    pub fn poll(&mut self, now: Instant, media: &mut MediaHost) -> bool {
        if !self.active() || matches!(self.view.state, "ended" | "failed") {
            return false;
        }
        let before = self.change_key();
        if let Err(e) = self.tick(now, media) {
            self.fail(e.to_string());
        }
        self.update_position();
        before != self.change_key()
    }
    fn change_key(
        &self,
    ) -> (
        TimelineTime,
        &'static str,
        u64,
        u64,
        u64,
        usize,
        Option<u32>,
        Option<u32>,
    ) {
        (
            self.position,
            self.view.state,
            self.view.offered,
            self.view.decoded,
            self.view.buffer_stalls,
            self.view.buffered,
            self.view.process_id,
            self.view.prepared_process_id,
        )
    }
    fn tick(&mut self, now: Instant, media: &mut MediaHost) -> Result<(), MediaError> {
        let time_error = |e: TimeError| MediaError::Invalid(e.to_string());
        let plan = self.plan.as_ref().ok_or(MediaError::Worker)?;
        let clock = plan.timebase;
        for slot in [&mut self.active, &mut self.prepared].into_iter().flatten() {
            let worker = self.workers.get(slot.worker).ok_or(MediaError::Worker)?;
            if let Err(e) = slot.poll(worker, clock) {
                slot.error = Some(e.to_string());
            }
        }
        if self.audio_master
            && let Some(error) = self.audio.as_ref().and_then(AudioTransport::failed)
        {
            self.audio_note = Some(format!("Audio output unavailable: {error}"));
            return Err(MediaError::Invalid(format!(
                "Audio output stopped ({error}). Play again to continue without sound."
            )));
        }
        let sampled = match self.audio_position(now) {
            Some(position) => position,
            None => self.clock.at(now).map_err(time_error)?,
        }
        .min(plan.extent);
        // Inspect every boundary even when a delayed pump would jump over a clip.
        let boundary = if let Some(slot) = &self.active {
            Some(slot.clip.end().map_err(time_error)?)
        } else {
            plan.track
                .as_ref()
                .and_then(|t| plan.index.next(t.id, self.position))
                .map(|c| c.start)
        };
        let target = boundary
            .filter(|b| *b > self.position)
            .map_or(sampled, |b| sampled.min(b));
        self.position = target;
        if target >= plan.extent {
            self.clock.freeze(plan.extent);
            self.view.state = "ended";
            self.view.desired_playing = false;
            for worker in &self.workers {
                worker.cancel()?;
            }
            if let Some(audio) = &mut self.audio {
                audio.cancel();
            }
            self.audio_master = false;
            return Ok(());
        }
        let clip = plan
            .track
            .as_ref()
            .map_or(Ok(None), |t| plan.index.active(plan.project, t.id, target))
            .map_err(|e| MediaError::Invalid(e.to_string()))?
            .cloned();
        if self.active.as_ref().map(|s| s.clip.id) != clip.as_ref().map(|c| c.id) {
            self.clear_image();
            if let Some(old) = self.active.take() {
                self.retire(old)?;
            }
            if let Some(clip) = clip {
                if self.prepared.as_ref().is_some_and(|s| s.clip.id == clip.id) {
                    self.active = self.prepared.take();
                    self.view.handoffs += 1;
                } else {
                    let worker = self.prepared.as_ref().map_or(0, |s| 1 - s.worker);
                    self.active = Some(self.new_slot(clip, target, worker)?);
                }
            }
        }
        if let Some(slot) = &self.active {
            if let Some(error) = &slot.error {
                return Err(MediaError::Invalid(error.clone()));
            }
            let blocked = (!slot.ready() && (self.starting || self.frame.is_none()))
                || (self.view.state == "buffering" && slot.pending.is_empty() && !slot.eof);
            if blocked && self.audio_master && !self.starting {
                // Audio is the master: it keeps playing; the picture catches up.
                self.note_video_late();
            } else if blocked {
                self.clock.freeze(target);
                if !self.starting && self.view.desired_playing && self.view.state != "buffering" {
                    self.view.buffer_stalls += 1;
                    self.view.state = "buffering";
                }
                self.prepare_next()?;
                return Ok(());
            }
        }
        if self.starting && self.audio_master && self.view.desired_playing {
            // Both must be ready: the first picture and enough queued audio.
            if !self.audio.as_ref().is_some_and(AudioTransport::ready) {
                self.prepare_next()?;
                return Ok(());
            }
        }
        if self.starting || self.view.state == "buffering" {
            self.starting = false;
            if self.audio_master
                && self.view.desired_playing
                && let Some(audio) = &mut self.audio
            {
                audio.play();
            }
            self.view.request_to_ready_us.get_or_insert(
                now.saturating_duration_since(self.queued)
                    .as_micros()
                    .min(u128::from(u64::MAX)) as u64,
            );
            if self.view.desired_playing {
                self.clock.resume(now);
                self.view.state = "playing";
            } else {
                self.view.state = "paused";
            }
        }
        let mut chosen = None;
        if let Some(slot) = &mut self.active {
            // Bounded draining, newest due frame wins; future PTS remains queued.
            for _ in 0..3 {
                while slot.pending.front().is_some_and(|f| f.position <= target) {
                    if chosen.is_some() {
                        self.view.dropped += 1;
                    }
                    chosen = slot.pending.pop_front();
                }
                slot.poll(
                    self.workers.get(slot.worker).ok_or(MediaError::Worker)?,
                    clock,
                )?;
                if !slot.pending.front().is_some_and(|f| f.position <= target) {
                    break;
                }
            }
            if let Some(error) = &slot.error {
                return Err(MediaError::Invalid(error.clone()));
            }
            if let Some(timed) = chosen {
                self.video_late_episode = false;
                self.view.offered_ticks = Some(timed.position.0);
                self.view.offered_pts = Some(timed.frame.actual);
                self.view.offered += 1;
                self.frame_clip = Some(slot.clip.id);
                self.frame = Some(PresentedFrame {
                    generation: media.next_generation(),
                    image: timed.frame.image,
                    ticket: timed.frame.ticket,
                    actual: timed.frame.actual,
                });
            }
            if slot.pending.is_empty()
                && !slot.eof
                && self.view.desired_playing
                && self.audio_master
            {
                self.note_video_late();
            } else if slot.pending.is_empty() && !slot.eof && self.view.desired_playing {
                self.clock.freeze(target);
                if self.view.state != "buffering" {
                    self.view.buffer_stalls += 1;
                }
                self.view.state = "buffering";
            }
        }
        self.prepare_next()?;
        Ok(())
    }
    fn prepare_next(&mut self) -> Result<(), MediaError> {
        let plan = self.plan.as_ref().ok_or(MediaError::Worker)?;
        let next = plan
            .track
            .as_ref()
            .and_then(|t| plan.index.next(t.id, self.position))
            .cloned();
        let next = next.filter(|c| {
            let delta = c.start.0.saturating_sub(self.position.0);
            i128::from(delta) * i128::from(plan.timebase.numerator())
                <= 2 * i128::from(plan.timebase.denominator())
        });
        if self.prepared.as_ref().map(|s| s.clip.id) != next.as_ref().map(|c| c.id) {
            if let Some(old) = self.prepared.take() {
                self.retire(old)?;
            }
            if let Some(clip) = next {
                let worker = self.active.as_ref().map_or(0, |s| 1 - s.worker);
                self.prepared = Some(self.new_slot(clip.clone(), clip.start, worker)?);
            }
        }
        self.view.decoded = self.completed_decoded
            + self.active.as_ref().map_or(0, |s| s.decoded)
            + self.prepared.as_ref().map_or(0, |s| s.decoded);
        self.view.process_id = self.active.as_ref().and_then(|s| s.pid);
        self.view.prepared_process_id = self.prepared.as_ref().and_then(|s| s.pid);
        self.view.buffered = self.active.as_ref().map_or(0, |s| s.pending.len())
            + self.prepared.as_ref().map_or(0, |s| s.pending.len());
        Ok(())
    }
    fn update_position(&mut self) {
        self.view.clock_ticks = self.position.0;
        self.view.clock_ms = SourceTime {
            ticks: self.position.0,
            timebase: self.clock.timebase,
        }
        .rescale(Timebase::new(1, 1000).expect("constant"), Rounding::Floor)
        .map_or(i64::MAX, |t| t.ticks);
        self.view.clock_text = view::precise_duration(self.view.clock_ms);
    }
    pub fn preview(&self) -> TimelinePreviewView {
        let mut v = TimelinePreviewView {
            position_ticks: self.position.0,
            timebase: self.clock.timebase,
            position_ms: self.view.clock_ms,
            position_text: self.view.clock_text.clone(),
            position_input: self.view.clock_text.clone(),
            state: self.view.state,
            busy: matches!(self.view.state, "starting" | "seeking" | "buffering"),
            has_frame: self.frame.is_some(),
            detail: self.view.error.clone().unwrap_or_else(|| {
                if self.audio_master {
                    "Video and audio · audio clock".into()
                } else {
                    "Video only · one monitored track".into()
                }
            }),
            actual: self.frame.as_ref().map(|f| f.actual),
            process_id: self.view.process_id,
            request_to_ready_us: self.view.request_to_ready_us,
            frames_copied: self.view.copied,
            ..Default::default()
        };
        if let Some(plan) = &self.plan {
            v.extent_ticks = plan.extent.0;
            v.extent_ms = SourceTime {
                ticks: plan.extent.0,
                timebase: plan.timebase,
            }
            .rescale(Timebase::new(1, 1000).expect("constant"), Rounding::Floor)
            .map_or(i64::MAX, |t| t.ticks);
            v.extent_text = view::precise_duration(v.extent_ms);
            v.seek_max_ms = v.extent_ms.max(1);
            v.track_id = plan.track.as_ref().map(|t| t.id.to_string());
            v.track_name = plan
                .track
                .as_ref()
                .map_or_else(|| "No video track".into(), |t| t.name.clone());
            v.title = format!("Timeline · {} · single video track", v.track_name);
            v.evaluation_generation = Some(plan.generation.to_string());
            if let Some(slot) = &self.active {
                v.clip_id = Some(slot.clip.id.to_string());
                v.media_id = Some(slot.clip.media_id.to_string());
                v.clip_start_ticks = Some(slot.clip.start.0);
                v.clip_end_ticks = slot.clip.end().ok().map(|t| t.0);
                v.requested = Some(slot.requested);
                v.source_name = plan
                    .media
                    .get(&slot.clip.media_id)
                    .map_or_else(String::new, view::file_name);
                if self.view.error.is_none() {
                    v.detail = if self.audio_master {
                        format!("{} · video and audio · audio clock", v.source_name)
                    } else if let Some(note) = &self.audio_note {
                        format!("{} · video only · {note}", v.source_name)
                    } else {
                        format!("{} · video only · one monitored track", v.source_name)
                    };
                }
            } else if self.view.state == "playing" {
                v.state = "blank";
                v.detail = "Timeline gap · clock advancing".into();
            }
            if self.frame.is_none() && self.active.as_ref().is_some_and(|s| s.eof) {
                v.detail = "No eligible frame inside this clip · clock advancing".into();
            }
        }
        if let Some(actual) = v.actual {
            v.caption = format!(
                "Source PTS {} @ {}/{}",
                actual.ticks,
                actual.timebase.numerator(),
                actual.timebase.denominator()
            );
        }
        v
    }
    pub fn copy_allowed(&self, generation: u64) -> bool {
        self.frame
            .as_ref()
            .is_some_and(|f| f.generation == generation)
            && self.active()
    }
    pub fn copied(&mut self, generation: u64) {
        if self.copy_allowed(generation) && self.copied_generation != Some(generation) {
            self.copied_generation = Some(generation);
            self.view.copied += 1;
            self.view.copied_ticks = self.view.offered_ticks;
            self.view.copied_pts = self.view.offered_pts;
            self.view.copied_clip_id = self.frame_clip.map(|id| id.to_string());
            // A/V offset at presentation: picture position minus audio clock.
            if self.audio_master
                && self.view.state == "playing"
                && let (Some(frame_ticks), Some(audio)) =
                    (self.view.copied_ticks, self.audio_position(Instant::now()))
            {
                let ticks = frame_ticks - audio.0;
                let ms = ticks as f64 * 1000.0 * f64::from(self.clock.timebase.numerator())
                    / f64::from(self.clock.timebase.denominator());
                self.view.av_offset_ms = Some(ms);
                self.view.av_offset_max_ms = self.view.av_offset_max_ms.max(ms.abs());
                self.view.av_samples += 1;
            }
        }
    }
    pub fn shutdown(&mut self) -> Result<(), MediaError> {
        self.cancel()?;
        for worker in &mut self.workers {
            worker.shutdown()?;
        }
        if let Some(mut audio) = self.audio.take() {
            audio.shutdown();
        }
        Ok(())
    }
}

fn exact_input(position: TimelineTime, clock: Timebase) -> String {
    let source = SourceTime {
        ticks: position.0,
        timebase: clock,
    };
    let millis = Timebase::new(1, 1000).expect("constant");
    match (
        source.rescale(millis, Rounding::Floor),
        source.rescale(millis, Rounding::Ceil),
    ) {
        (Ok(floor), Ok(ceil)) if floor == ceil => view::precise_duration(floor.ticks),
        _ => format!("{} ticks", position.0),
    }
}
