//! Bounded transient timeline coordination. The project owner evaluates; the
//! existing isolated decoder owns media work; this object owns one latest frame.
use crate::{
    media::{MediaHost, PresentedFrame},
    view,
};
use cutcap_contracts::*;
use cutcap_core::SourceMapping;
use cutcap_media::{DecodeOutcome, DecodeRequest, MediaError, PreviewTicket};
use serde::Serialize;
use std::time::Instant;

#[derive(Debug, Clone, Serialize)]
pub struct TimelinePreviewView {
    pub position_ticks: i64,
    pub timebase: Timebase,
    pub position_ms: i64,
    pub position_text: String,
    pub position_input: String,
    pub seek_max_ms: i64,
    pub extent_ms: i64,
    pub extent_ticks: i64,
    pub extent_text: String,
    pub track_id: Option<String>,
    pub track_name: String,
    pub title: String,
    pub state: &'static str,
    pub detail: String,
    pub busy: bool,
    pub has_frame: bool,
    pub clip_id: Option<String>,
    pub media_id: Option<String>,
    pub source_name: String,
    pub clip_start_ticks: Option<i64>,
    pub clip_end_ticks: Option<i64>,
    pub requested: Option<SourceTime>,
    pub actual: Option<SourceTime>,
    pub caption: String,
    pub evaluation_generation: Option<String>,
    pub evaluation_us: Option<u64>,
    pub decode_us: Option<u64>,
    pub request_to_ready_us: Option<u64>,
    pub process_id: Option<u32>,
    pub frames_copied: u64,
}
impl Default for TimelinePreviewView {
    fn default() -> Self {
        Self {
            position_ticks: 0,
            timebase: Timebase::DEFAULT_TIMELINE,
            position_ms: 0,
            position_text: "00:00.000".into(),
            position_input: "00:00.000".into(),
            seek_max_ms: 60_000,
            extent_ms: 0,
            extent_ticks: 0,
            extent_text: "00:00.000".into(),
            track_id: None,
            track_name: "No video track".into(),
            title: "Timeline".into(),
            state: "blank",
            detail: "Select Timeline to monitor one video track.".into(),
            busy: false,
            has_frame: false,
            clip_id: None,
            media_id: None,
            source_name: String::new(),
            clip_start_ticks: None,
            clip_end_ticks: None,
            requested: None,
            actual: None,
            caption: String::new(),
            evaluation_generation: None,
            evaluation_us: None,
            decode_us: None,
            request_to_ready_us: None,
            process_id: None,
            frames_copied: 0,
        }
    }
}
pub struct TimelinePreview {
    pub view: TimelinePreviewView,
    pub frame: Option<PresentedFrame>,
    desired: Option<TimelineSeek>,
    last_input: TimelineSeek,
    key: RequestId,
    inflight: Option<(RequestId, RequestId)>,
    expected: Option<(PreviewTicket, SourceMapping)>,
    queued: Instant,
    copied: Option<u64>,
}
impl Default for TimelinePreview {
    fn default() -> Self {
        Self {
            view: TimelinePreviewView::default(),
            frame: None,
            desired: None,
            last_input: TimelineSeek::Ticks(TimelineTime::ZERO),
            key: RequestId::new(),
            inflight: None,
            expected: None,
            queued: Instant::now(),
            copied: None,
        }
    }
}
impl TimelinePreview {
    pub fn invalidate(&mut self, media: &mut MediaHost) -> Result<(), MediaError> {
        self.queue(self.last_input, media)
    }
    pub fn queue(&mut self, input: TimelineSeek, media: &mut MediaHost) -> Result<(), MediaError> {
        media.cancel_timeline()?;
        self.key = RequestId::new();
        self.last_input = input;
        self.desired = Some(input);
        self.expected = None;
        self.frame = None;
        self.copied = None;
        self.view.busy = true;
        self.view.has_frame = false;
        self.view.actual = None;
        self.view.caption.clear();
        self.view.decode_us = None;
        self.view.request_to_ready_us = None;
        self.view.process_id = None;
        self.view.state = "evaluating";
        self.view.detail = "Evaluating the monitored video track…".into();
        self.queued = Instant::now();
        Ok(())
    }
    pub fn reset_position(&mut self) {
        self.last_input = TimelineSeek::Ticks(TimelineTime::ZERO);
        self.view = TimelinePreviewView::default();
    }
    pub fn cancel(&mut self, media: &mut MediaHost) -> Result<(), MediaError> {
        media.cancel_timeline()?;
        self.key = RequestId::new();
        self.desired = None;
        self.expected = None;
        self.frame = None;
        self.view.busy = false;
        self.view.has_frame = false;
        self.view.state = "cancelled";
        self.view.detail = "Timeline preview cancelled. Seek again to request a frame.".into();
        Ok(())
    }
    /// One reliable evaluation in flight; rapid UI seeks replace one desired slot.
    pub fn next_query(&self, project_id: ProjectId) -> Option<CommandEnvelope> {
        if self.inflight.is_some() {
            return None;
        }
        Some(CommandEnvelope {
            request_id: self.key,
            project_id: Some(project_id),
            command: Command::EvaluateTimeline {
                position: self.desired?,
                track_id: None,
            },
        })
    }
    pub fn submitted(&mut self, request: RequestId) {
        self.inflight = Some((request, self.key));
        self.desired = None;
    }
    pub fn evaluated(
        &mut self,
        request: RequestId,
        result: Result<Box<TimelineEvaluation>, EngineError>,
        active: bool,
        scope: Option<(ProjectId, RequestId)>,
        media: &mut MediaHost,
    ) {
        let Some((id, key)) = self.inflight else {
            return;
        };
        if id != request {
            return;
        }
        self.inflight = None;
        if !active || key != self.key {
            return;
        }
        let result = match result {
            Ok(r) => r,
            Err(e) => {
                self.last_input = TimelineSeek::Ticks(TimelineTime(self.view.position_ticks));
                self.fail("failed", e.to_string());
                return;
            }
        };
        if scope != Some((result.project_id, result.generation)) {
            return;
        }
        self.last_input = TimelineSeek::Ticks(result.position);
        let ms = |ticks| {
            SourceTime {
                ticks,
                timebase: result.timebase,
            }
            .rescale(Timebase::new(1, 1000).expect("constant"), Rounding::Floor)
            .map(|t| t.ticks)
        };
        let position_ms = match ms(result.position.0) {
            Ok(ms) => ms,
            Err(e) => {
                self.fail("failed", e.to_string());
                return;
            }
        };
        self.view.position_ticks = result.position.0;
        self.view.timebase = result.timebase;
        self.view.position_ms = position_ms;
        self.view.position_text = view::precise_duration(position_ms);
        let ceiling_ms = SourceTime {
            ticks: result.position.0,
            timebase: result.timebase,
        }
        .rescale(Timebase::new(1, 1000).expect("constant"), Rounding::Ceil)
        .ok()
        .map(|t| t.ticks);
        self.view.position_input = if ceiling_ms == Some(position_ms) {
            self.view.position_text.clone()
        } else {
            format!("{} ticks", result.position.0)
        };
        self.view.extent_ms = ms(result.extent.0).unwrap_or(i64::MAX);
        self.view.extent_ticks = result.extent.0;
        self.view.extent_text = view::precise_duration(self.view.extent_ms);
        self.view.seek_max_ms = 60_000
            .max(position_ms)
            .max(self.view.extent_ms)
            .saturating_add(5000);
        self.view.track_id = result.track.as_ref().map(|t| t.id.to_string());
        self.view.track_name = result
            .track
            .as_ref()
            .map_or_else(|| "No video track".into(), |t| t.name.clone());
        self.view.title = format!("Timeline · {} · single video track", self.view.track_name);
        self.view.evaluation_generation = Some(result.generation.to_string());
        self.view.evaluation_us = Some(result.evaluation_us);
        self.view.requested = None;
        self.view.clip_id = None;
        self.view.media_id = None;
        self.view.source_name.clear();
        self.view.clip_start_ticks = None;
        self.view.clip_end_ticks = None;
        match result.target {
            TimelineTarget::Blank => self.fail(
                "blank",
                "No clip at this timeline position. This is a blank frame.".into(),
            ),
            TimelineTarget::Unavailable {
                clip,
                path,
                availability,
                detail,
            } => {
                self.clip(&clip);
                self.view.source_name = path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned();
                self.fail(
                    "unavailable",
                    format!(
                        "{} · source {availability:?}: {detail}",
                        self.view.source_name
                    ),
                );
            }
            TimelineTarget::NoFrame {
                clip,
                media,
                requested,
                detail,
                ..
            } => {
                self.clip(&clip);
                self.view.source_name = view::file_name(&media);
                self.view.requested = Some(requested);
                self.fail("no_frame", detail);
            }
            TimelineTarget::Frame {
                clip,
                media: source,
                requested,
                end_exclusive,
            } => {
                self.clip(&clip);
                self.view.source_name = view::file_name(&source);
                self.view.requested = Some(requested);
                let ticket = PreviewTicket {
                    project_id: result.project_id,
                    media_id: source.id,
                    request_id: self.key,
                };
                self.expected = Some((
                    ticket,
                    SourceMapping {
                        requested,
                        end_exclusive,
                    },
                ));
                match media.request_timeline(DecodeRequest {
                    ticket,
                    media: *source,
                    position: requested,
                }) {
                    Ok(()) => {
                        self.view.state = "loading";
                        self.view.detail = "Decoding the evaluated clip frame…".into();
                    }
                    Err(e) => self.fail("failed", e.to_string()),
                }
            }
        }
    }
    fn clip(&mut self, clip: &ClipSummary) {
        self.view.clip_id = Some(clip.id.to_string());
        self.view.media_id = Some(clip.media_id.to_string());
        self.view.clip_start_ticks = Some(clip.start.0);
        self.view.clip_end_ticks = clip.start.checked_add(clip.duration).ok().map(|t| t.0);
    }
    fn fail(&mut self, state: &'static str, detail: String) {
        self.view.state = state;
        self.view.detail = detail;
        self.view.busy = false;
        self.view.has_frame = false;
        self.frame = None;
    }
    pub fn decoded(&mut self, outcome: DecodeOutcome, media: &mut MediaHost) {
        let Some((ticket, mapping)) = self.expected else {
            return;
        };
        if ticket != outcome.ticket || ticket.request_id != self.key {
            return;
        }
        match outcome.result {
            Ok(frame) => {
                if frame.ticket != ticket || frame.requested != mapping.requested {
                    self.fail(
                        "failed",
                        "Decoder returned a frame for a different timeline request.".into(),
                    );
                    return;
                }
                match mapping.accepts(frame.actual) {
                    Ok(true) => {}
                    Ok(false) => {
                        self.fail("no_frame", "The next source frame is outside this clip's exclusive source endpoint. No frame is substituted.".into());
                        return;
                    }
                    Err(e) => {
                        self.fail("failed", e.to_string());
                        return;
                    }
                }
                self.view.state = "ready";
                self.view.busy = false;
                self.view.has_frame = true;
                self.view.actual = Some(frame.actual);
                self.view.caption = format!(
                    "Source PTS {} @ {}/{}",
                    frame.actual.ticks,
                    frame.actual.timebase.numerator(),
                    frame.actual.timebase.denominator()
                );
                self.view.detail = format!(
                    "{} · video only · one monitored track · timeline playback unavailable",
                    self.view.source_name
                );
                self.view.decode_us = Some(frame.decode_us);
                self.view.process_id = Some(frame.process_id);
                self.view.request_to_ready_us =
                    Some(self.queued.elapsed().as_micros().min(u128::from(u64::MAX)) as u64);
                self.frame = Some(PresentedFrame {
                    generation: media.next_generation(),
                    image: frame.image,
                    ticket,
                    actual: frame.actual,
                });
            }
            Err(MediaError::NoFrame) => self.fail(
                "no_frame",
                "No valid source frame exists at/after this request inside the clip.".into(),
            ),
            Err(e) => self.fail("failed", e.to_string()),
        }
    }
    pub fn copy_allowed(&self, generation: u64) -> bool {
        self.frame
            .as_ref()
            .is_some_and(|f| f.generation == generation && f.ticket.request_id == self.key)
    }
    pub fn copied(&mut self, generation: u64) {
        if self.copy_allowed(generation) && self.copied != Some(generation) {
            self.copied = Some(generation);
            self.view.frames_copied += 1;
        }
    }
}
