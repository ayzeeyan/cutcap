//! UI-independent source transport and monotonic, PTS-driven scheduling.
use cutcap_contracts::*;
use cutcap_media::*;
use serde::Serialize;
use std::{collections::VecDeque, sync::Arc, time::Instant};

#[derive(Debug, Clone, Serialize)]
pub struct PlaybackView {
    pub state: &'static str,
    pub desired_playing: bool,
    pub epoch: Option<String>,
    pub requested_ms: i64,
    pub clock_ms: u64,
    pub offered_ms: Option<u64>,
    pub copied_ms: Option<u64>,
    pub offered_pts: Option<SourceTime>,
    pub copied_pts: Option<SourceTime>,
    pub decoded: u64,
    pub offered: u64,
    pub copied: u64,
    pub dropped: u64,
    pub buffer_stalls: u64,
    pub buffered: usize,
    pub process_id: Option<u32>,
    pub error: Option<String>,
    pub requested_text: String,
    pub copied_text: String,
    pub clock_text: String,
    pub request_to_ready_us: Option<u64>,
}
impl Default for PlaybackView {
    fn default() -> Self {
        Self {
            state: "stopped",
            desired_playing: false,
            epoch: None,
            requested_ms: 0,
            clock_ms: 0,
            offered_ms: None,
            copied_ms: None,
            offered_pts: None,
            copied_pts: None,
            decoded: 0,
            offered: 0,
            copied: 0,
            dropped: 0,
            buffer_stalls: 0,
            buffered: 0,
            process_id: None,
            error: None,
            requested_text: "00:00.000".into(),
            copied_text: "--:--.---".into(),
            clock_text: "00:00.000".into(),
            request_to_ready_us: None,
        }
    }
}
pub struct Playback {
    decoder: PersistentDecoder,
    clock: PresentationClock,
    pub view: PlaybackView,
    request: Option<DecodeRequest>,
    pending: VecDeque<DecodedFrame>,
    first: bool,
    eof: bool,
    offered_generation: Option<u64>,
    copied_generation: Option<u64>,
    queued: Instant,
}
impl Playback {
    pub fn start(backend: Arc<dyn MediaBackend>) -> Result<Self, std::io::Error> {
        Ok(Self {
            decoder: PersistentDecoder::start(backend)?,
            clock: PresentationClock::default(),
            view: PlaybackView::default(),
            request: None,
            pending: VecDeque::new(),
            first: true,
            eof: false,
            offered_generation: None,
            copied_generation: None,
            queued: Instant::now(),
        })
    }
    pub fn active(&self) -> bool {
        self.request.is_some()
    }
    pub fn request(
        &mut self,
        request: DecodeRequest,
        ms: i64,
        playing: bool,
        seeking: bool,
    ) -> Result<(), MediaError> {
        self.decoder.request(request.clone())?;
        self.pending.clear();
        self.first = true;
        self.eof = false;
        self.clock.freeze_at(0);
        self.offered_generation = None;
        self.copied_generation = None;
        self.view = PlaybackView {
            state: if seeking { "seeking" } else { "starting" },
            desired_playing: playing,
            epoch: Some(request.ticket.request_id.to_string()),
            requested_ms: ms,
            requested_text: crate::view::precise_duration(ms),
            ..PlaybackView::default()
        };
        self.request = Some(request);
        self.queued = Instant::now();
        Ok(())
    }
    pub fn pause(&mut self, now: Instant) {
        if self.active() && !matches!(self.view.state, "ended" | "failed") {
            self.clock.pause(now);
            self.view.desired_playing = false;
            if !self.first {
                self.view.state = "paused";
            }
        }
    }
    pub fn resume(&mut self, now: Instant) {
        if self.active() {
            self.view.desired_playing = true;
            if self.first {
                self.view.state = "starting";
            } else {
                self.clock.resume(now);
                self.view.state = "playing";
            }
        }
    }
    pub fn reset(&mut self) -> Result<(), MediaError> {
        self.decoder.cancel()?;
        self.pending.clear();
        self.request = None;
        self.clock.freeze_at(0);
        self.view = PlaybackView::default();
        self.offered_generation = None;
        self.copied_generation = None;
        Ok(())
    }
    pub fn ticket(&self) -> Option<PreviewTicket> {
        self.request.as_ref().map(|r| r.ticket)
    }
    fn relative_ns(&self, time: SourceTime) -> Result<u64, MediaError> {
        let request = self.request.as_ref().ok_or(MediaError::Worker)?;
        let origin = request
            .media
            .metadata
            .start()
            .rescale(time.timebase, Rounding::Floor)
            .map_err(|_| MediaError::Invalid("source clock overflow".into()))?;
        let ticks = time
            .ticks
            .checked_sub(origin.ticks)
            .ok_or_else(|| MediaError::Invalid("source clock overflow".into()))?;
        let ns = SourceTime {
            ticks,
            timebase: time.timebase,
        }
        .rescale(
            Timebase::new(1, 1_000_000_000).expect("constant"),
            Rounding::Floor,
        )
        .map_err(|_| MediaError::Invalid("source clock overflow".into()))?;
        u64::try_from(ns.ticks)
            .map_err(|_| MediaError::Invalid("frame precedes source start".into()))
            .and_then(|value| {
                if value > MAX_MEDIA_SECONDS as u64 * 1_000_000_000 {
                    Err(MediaError::Budget("continuous source timestamp"))
                } else {
                    Ok(value)
                }
            })
    }
    pub fn offered(&mut self, generation: u64, time: SourceTime) -> Result<(), MediaError> {
        self.offered_generation = Some(generation);
        self.view.offered = self.view.offered.saturating_add(1);
        self.view.offered_ms = Some(self.relative_ns(time)? / 1_000_000);
        self.view.offered_pts = Some(time);
        Ok(())
    }
    pub fn copied(&mut self, generation: u64, time: SourceTime) -> Result<(), MediaError> {
        if self.offered_generation == Some(generation) && self.copied_generation != Some(generation)
        {
            self.copied_generation = Some(generation);
            self.view.copied = self.view.copied.saturating_add(1);
            self.view.copied_ms = Some(self.relative_ns(time)? / 1_000_000);
            self.view.copied_pts = Some(time);
            self.view.copied_text =
                crate::view::precise_duration(self.view.copied_ms.unwrap_or(0) as i64);
        }
        Ok(())
    }
    pub fn fail(&mut self, error: &MediaError, now: Instant) {
        let _cancel = self.decoder.cancel();
        self.clock.pause(now);
        self.view.state = "failed";
        self.view.desired_playing = false;
        self.view.error = Some(error.to_string());
        self.pending.clear();
    }
    /// At most two lookahead frames plus the decoder's three-frame queue.
    /// Clock time is sampled once; no sleeps, filesystem or process work here.
    pub fn tick(&mut self, now: Instant) -> Result<(Option<DecodedFrame>, bool), MediaError> {
        if !self.active() || matches!(self.view.state, "failed" | "ended") {
            return Ok((None, false));
        }
        let before = self.view.state;
        while self.pending.len() < 2 {
            let Some(frame) = self.decoder.take()? else {
                break;
            };
            if Some(frame.ticket) == self.ticket() {
                self.relative_ns(frame.actual)?;
                self.pending.push_back(frame);
            }
        }
        if let Some(terminal) = self.decoder.terminal()? {
            terminal?;
            self.eof = true;
        }
        let progress = self.decoder.progress()?;
        self.view.decoded = progress.decoded;
        self.view.process_id = progress.process_id;
        self.view.buffered = self.pending.len() + progress.buffered;
        if self.first {
            let Some(frame) = self.pending.pop_front() else {
                return Ok((None, false));
            };
            let ns = self.relative_ns(frame.actual)?;
            self.view.request_to_ready_us = Some(
                now.saturating_duration_since(self.queued)
                    .as_micros()
                    .min(u128::from(u64::MAX)) as u64,
            );
            self.first = false;
            if self.view.desired_playing {
                self.clock.start(ns, now);
                self.view.state = "playing";
            } else {
                self.clock.freeze_at(ns);
                self.view.state = "paused";
            }
            self.view.clock_ms = ns / 1_000_000;
            self.view.clock_text = crate::view::precise_duration(self.view.clock_ms as i64);
            return Ok((Some(frame), true));
        }
        if !self.view.desired_playing {
            return Ok((None, before != self.view.state));
        }
        if self.view.state == "buffering" && !self.pending.is_empty() {
            self.clock.resume(now);
            self.view.state = "playing";
        }
        let ns = self.clock.position(now);
        self.view.clock_ms = ns / 1_000_000;
        self.view.clock_text = crate::view::precise_duration(self.view.clock_ms as i64);
        let mut chosen = None;
        while self
            .pending
            .front()
            .is_some_and(|f| self.relative_ns(f.actual).is_ok_and(|pts| pts <= ns))
        {
            if chosen.is_some() {
                self.view.dropped = self.view.dropped.saturating_add(1);
            }
            chosen = self.pending.pop_front();
        }
        if chosen.is_some() {
            return Ok((chosen, true));
        }
        if self.pending.is_empty() {
            if self.eof
                && self.offered_generation.is_some()
                && self.offered_generation == self.copied_generation
            {
                self.clock.pause(now);
                self.view.state = "ended";
                self.view.desired_playing = false;
            } else if !self.eof && self.view.state != "buffering" {
                self.clock.pause(now);
                self.view.state = "buffering";
                self.view.buffer_stalls = self.view.buffer_stalls.saturating_add(1);
            }
        }
        Ok((None, before != self.view.state))
    }
    pub fn shutdown(&mut self) -> Result<(), MediaError> {
        self.decoder.shutdown()
    }
    pub fn requested_at(&self) -> Instant {
        self.queued
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{path::Path, time::Duration};
    struct Idle;
    impl MediaBackend for Idle {
        fn probe(&self, _: &Path, _: &Cancellation) -> Result<ProbeResult, MediaError> {
            Err(MediaError::Worker)
        }
        fn decode(&self, _: &DecodeRequest, _: &Cancellation) -> Result<DecodedFrame, MediaError> {
            Err(MediaError::Worker)
        }
    }
    fn scheduler(now: Instant) -> Playback {
        let metadata=parse_probe(br#"{"streams":[{"index":0,"codec_type":"video","codec_name":"ffv1","width":320,"height":180,"time_base":"1/1000","start_pts":0,"duration_ts":3000}],"format":{"format_name":"matroska"}}"#).unwrap();
        let media = MediaSummary {
            id: MediaId::new(),
            path: "fixture.mkv".into(),
            metadata,
        };
        let request = DecodeRequest {
            ticket: PreviewTicket {
                project_id: ProjectId::new(),
                media_id: media.id,
                request_id: RequestId::new(),
            },
            position: media.metadata.start(),
            media,
        };
        let mut p = Playback::start(Arc::new(Idle)).unwrap();
        p.request = Some(request);
        p.first = false;
        p.view.desired_playing = true;
        p.view.state = "playing";
        p.clock.start(0, now);
        p
    }
    fn frame(p: &Playback, pts: i64) -> DecodedFrame {
        let request = p.request.as_ref().unwrap();
        DecodedFrame {
            ticket: request.ticket,
            requested: request.position,
            actual: SourceTime {
                ticks: pts,
                timebase: request.position.timebase,
            },
            image: RgbaFrame::new(1, 1, 4, vec![0, 0, 0, 255]).unwrap(),
            decode_us: 1,
            process_id: 0,
        }
    }
    #[test]
    fn newest_due_frame_wins_and_eof_waits_for_delivery() {
        let now = Instant::now();
        let mut p = scheduler(now);
        p.pending.push_back(frame(&p, 100));
        p.pending.push_back(frame(&p, 200));
        let (chosen, _) = p.tick(now + Duration::from_millis(250)).unwrap();
        let chosen = chosen.unwrap();
        assert_eq!(chosen.actual.ticks, 200);
        assert_eq!(p.view.dropped, 1);
        p.offered(4, chosen.actual).unwrap();
        p.eof = true;
        p.tick(now + Duration::from_millis(251)).unwrap();
        assert_ne!(p.view.state, "ended");
        p.copied(4, chosen.actual).unwrap();
        p.copied(4, chosen.actual).unwrap();
        assert_eq!(p.view.copied, 1);
        p.tick(now + Duration::from_millis(252)).unwrap();
        assert_eq!(p.view.state, "ended");
    }
    #[test]
    fn starvation_freezes_clock_and_recovery_preserves_future_pts() {
        let now = Instant::now();
        let mut p = scheduler(now);
        p.tick(now + Duration::from_millis(300)).unwrap();
        assert_eq!(p.view.state, "buffering");
        p.tick(now + Duration::from_secs(8)).unwrap();
        assert_eq!(p.view.clock_ms, 300);
        p.pending.push_back(frame(&p, 400));
        assert!(p.tick(now + Duration::from_secs(9)).unwrap().0.is_none());
        assert_eq!(p.view.state, "playing");
        assert_eq!(
            p.tick(now + Duration::from_millis(9100))
                .unwrap()
                .0
                .unwrap()
                .actual
                .ticks,
            400
        );
    }
}
