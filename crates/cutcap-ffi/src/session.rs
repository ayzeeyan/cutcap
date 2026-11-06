//! One editor session: the command owner, the media host and the derived view.
//!
//! Ported from the Slint coordinator. The frontend calls these methods from its
//! UI thread, then calls `pump` on a timer to drain bounded worker results. No
//! callbacks cross into the frontend: it pulls the coalesced view and frame.
use crate::{
    bridge::{EngineBridge, HostEvent},
    media::{MediaHost, PresentedFrame},
    view::{self, InspectorView, ProjectView, SessionView, StatusView},
};
use cutcap_contracts::*;
use cutcap_media::{FfmpegBackend, MediaBackend, MediaError};
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::Arc,
};

pub const DEFAULT_PROJECT_NAME: &str = "Untitled project";

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error(transparent)]
    Engine(#[from] EngineError),
    #[error(transparent)]
    Media(#[from] MediaError),
    #[error("could not start a worker: {0}")]
    Worker(#[from] std::io::Error),
    #[error("{0}")]
    Invalid(&'static str),
}

#[derive(Default)]
struct Requests {
    snapshot: Option<ProjectSnapshot>,
    pending: HashSet<RequestId>,
    file_pending: HashSet<RequestId>,
    desired_media: Option<MediaId>,
    waiting_project: Option<RequestId>,
    registering: Option<RequestId>,
    inspecting_media: bool,
    verifying: Option<MediaId>,
    verification_request: Option<RequestId>,
    verified: Option<Command>,
    checks: std::collections::VecDeque<(MediaId, PathBuf)>,
}

pub struct Session {
    bridge: EngineBridge,
    media: MediaHost,
    requests: Requests,
    status: StatusView,
    engine_ready: bool,
    dirty: bool,
    revision: u64,
    cached: Option<String>,
    /// Bumped with every new project snapshot; the timeline document is
    /// rebuilt lazily from it (commands only, never per playback frame).
    timeline_generation: u64,
    timeline_cached: Option<String>,
    preview_mode: PreviewMode,
    timeline_preview: crate::timeline_preview::TimelinePreview,
    timeline_playback: crate::timeline_playback::TimelinePlayback,
    timeline_request: RequestId,
    timeline_desired: Option<TimelineSeek>,
    timeline_inflight: Option<RequestId>,
    timeline_intent: bool,
    timeline_seeking: bool,
    timeline_waiting: bool,
    clip_lookup: HashMap<ClipId, usize>,
    closed: bool,
}

impl Session {
    pub fn start() -> Result<Self, SessionError> {
        Self::with_backend(Arc::new(FfmpegBackend::default()))
    }

    pub fn with_backend(backend: Arc<dyn MediaBackend>) -> Result<Self, SessionError> {
        let mut session = Self {
            bridge: EngineBridge::start()?,
            media: MediaHost::start(backend.clone())?,
            requests: Requests::default(),
            status: StatusView {
                text: "Starting command worker…".into(),
                error: false,
            },
            engine_ready: false,
            dirty: true,
            revision: 0,
            cached: None,
            timeline_generation: 0,
            timeline_cached: None,
            preview_mode: PreviewMode::Source,
            timeline_preview: crate::timeline_preview::TimelinePreview::default(),
            timeline_playback: crate::timeline_playback::TimelinePlayback::start(backend)?,
            timeline_request: RequestId::new(),
            timeline_desired: None,
            timeline_inflight: None,
            timeline_intent: false,
            timeline_seeking: false,
            timeline_waiting: false,
            clip_lookup: HashMap::new(),
            closed: false,
        };
        session.new_project()?;
        Ok(session)
    }

    fn project_id(&self) -> Option<ProjectId> {
        self.requests.snapshot.as_ref().map(|snapshot| snapshot.id)
    }

    fn submit(&mut self, command: Command) -> Result<RequestId, SessionError> {
        if self.closed {
            return Err(SessionError::Invalid("The session is shut down."));
        }
        let invalidate = !matches!(
            command,
            Command::GetSnapshot
                | Command::SaveProject { .. }
                | Command::SelectMedia { .. }
                | Command::Select { .. }
        );
        let request_id = RequestId::new();
        let envelope = CommandEnvelope {
            request_id,
            project_id: self.project_id(),
            command,
        };
        self.dirty = true;
        match self.bridge.submit(envelope) {
            Ok(()) => {
                self.requests.pending.insert(request_id);
                if invalidate {
                    self.invalidate_timeline();
                }
                Ok(request_id)
            }
            Err(error) => {
                tracing::warn!(code = ?error.code, "{error}");
                self.status = StatusView {
                    text: error.to_string(),
                    error: true,
                };
                Err(error.into())
            }
        }
    }

    pub fn new_project(&mut self) -> Result<(), SessionError> {
        self.require_no_file_operation()?;
        let request_id = self.submit(Command::NewProject {
            name: DEFAULT_PROJECT_NAME.into(),
        })?;
        self.requests.waiting_project = Some(request_id);
        self.requests.desired_media = None;
        self.requests.inspecting_media = false;
        self.requests.checks.clear();
        self.requests.verified = None;
        self.requests.verifying = None;
        self.requests.verification_request = None;
        self.media.reset();
        self.timeline_preview.reset_position();
        self.invalidate_timeline();
        self.engine_ready = false;
        Ok(())
    }

    pub fn add_track(&mut self, kind: TrackKind) -> Result<(), SessionError> {
        self.submit(Command::AddTrack { kind }).map(|_| ())
    }

    pub fn save_project(&mut self, path: Option<PathBuf>) -> Result<(), SessionError> {
        self.require_no_file_operation()?;
        let id = self.submit(Command::SaveProject { path })?;
        self.requests.file_pending.insert(id);
        Ok(())
    }
    pub fn open_project(&mut self, path: PathBuf) -> Result<(), SessionError> {
        self.require_no_file_operation()?;
        // The old project/source remains intact until a validated replacement
        // returns from the owner. Frontend confirmation precedes this command.
        let id = self.submit(Command::OpenProject { path })?;
        self.requests.file_pending.insert(id);
        Ok(())
    }
    fn require_no_file_operation(&self) -> Result<(), SessionError> {
        if self.requests.file_pending.is_empty() {
            Ok(())
        } else {
            Err(EngineError::new(
                ErrorCode::Busy,
                "Wait for the current project file operation to finish.",
            )
            .into())
        }
    }
    pub fn relink_media(&mut self, id: MediaId, path: PathBuf) -> Result<(), SessionError> {
        if self.requests.verifying.is_some() {
            return Err(SessionError::Invalid(
                "Wait for the current source inspection to finish.",
            ));
        }
        let project = self
            .current_project()
            .ok_or(SessionError::Invalid("No project is ready."))?;
        if !self
            .requests
            .snapshot
            .as_ref()
            .is_some_and(|s| s.media_state.iter().any(|m| m.id == id))
        {
            return Err(SessionError::Invalid(
                "Select a registered media asset to relink.",
            ));
        }
        self.media
            .start_probe(project, id, path, true, true, &mut self.status)?;
        self.requests.verifying = Some(id);
        self.dirty = true;
        Ok(())
    }

    /// Appends a ready video source to the end of a video track (undoable).
    pub fn insert_clip(&mut self, media: MediaId, track: TrackId) -> Result<(), SessionError> {
        // The new clip becomes the selection, so the inspector follows it.
        self.requests.inspecting_media = false;
        self.submit(Command::InsertClip {
            media_id: media,
            track_id: track,
        })
        .map(|_| ())
    }

    pub fn remove_clip(&mut self, id: ClipId) -> Result<(), SessionError> {
        self.submit(Command::RemoveClip { clip_id: id }).map(|_| ())
    }

    /// Drop placement: the whole Ready source at `position` (undoable).
    pub fn place_clip(
        &mut self,
        media: MediaId,
        track: TrackId,
        position: TimelineSeek,
    ) -> Result<(), SessionError> {
        self.requests.inspecting_media = false;
        self.submit(Command::PlaceClip {
            media_id: media,
            track_id: track,
            position,
        })
        .map(|_| ())
    }

    pub fn move_clip(
        &mut self,
        id: ClipId,
        track: TrackId,
        position: TimelineSeek,
        anchor: ClipEdge,
    ) -> Result<(), SessionError> {
        self.submit(Command::MoveClip {
            clip_id: id,
            track_id: track,
            position,
            anchor,
        })
        .map(|_| ())
    }

    pub fn trim_clip(
        &mut self,
        id: ClipId,
        edge: ClipEdge,
        position: TimelineSeek,
    ) -> Result<(), SessionError> {
        self.submit(Command::TrimClip {
            clip_id: id,
            edge,
            position,
        })
        .map(|_| ())
    }

    pub fn split_clip(&mut self, id: ClipId, position: TimelineSeek) -> Result<(), SessionError> {
        self.submit(Command::SplitClip {
            clip_id: id,
            position,
        })
        .map(|_| ())
    }

    /// The Rust-owned timeline playhead: the transport clock while timeline
    /// playback is active, otherwise the still-preview position.
    pub fn timeline_playhead(&self) -> TimelineTime {
        if self.timeline_playback.active() {
            self.timeline_playback.position()
        } else {
            TimelineTime(self.timeline_preview.view.position_ticks)
        }
    }

    /// Splits at the playhead. Like every structural edit this stops timeline
    /// playback (the plan is invalidated) and does not resume it.
    pub fn split_clip_at_playhead(&mut self, id: ClipId) -> Result<(), SessionError> {
        let position = self.timeline_playhead();
        self.split_clip(id, TimelineSeek::Ticks(position))
    }

    fn editing_view(&self) -> view::EditingView {
        let Some(snapshot) = self.requests.snapshot.as_ref() else {
            return view::EditingView::default();
        };
        let notice = snapshot.edit_notice.clone();
        let SelectionState::Clip(id) = snapshot.selection else {
            return view::EditingView {
                split_enabled: false,
                split_detail: "Select a clip, then place the playhead inside it to split (Ctrl+B)."
                    .into(),
                notice,
            };
        };
        let Some(clip) = self
            .clip_lookup
            .get(&id)
            .and_then(|i| snapshot.clips.get(*i))
        else {
            return view::EditingView {
                notice,
                ..Default::default()
            };
        };
        let core = cutcap_core::Clip {
            id: clip.id,
            media_id: clip.media_id,
            track_id: clip.track_id,
            start: clip.start,
            duration: clip.duration,
            source_in: clip.source_in,
        };
        match cutcap_core::edit::split_offset(&core, self.timeline_playhead(), snapshot.timebase) {
            Ok(_) => view::EditingView {
                split_enabled: true,
                split_detail: "Split the selected clip at the playhead (Ctrl+B)".into(),
                notice,
            },
            Err(reason) => view::EditingView {
                split_enabled: false,
                split_detail: reason.to_string(),
                notice,
            },
        }
    }

    /// Timeline selection only: never dirties the document or enters history.
    pub fn select_clip(&mut self, id: ClipId) -> Result<(), SessionError> {
        self.requests.inspecting_media = false;
        self.submit(Command::Select {
            selection: SelectionState::Clip(id),
        })
        .map(|_| ())
    }

    pub fn undo(&mut self) -> Result<(), SessionError> {
        self.submit(Command::Undo).map(|_| ())
    }

    pub fn redo(&mut self) -> Result<(), SessionError> {
        self.submit(Command::Redo).map(|_| ())
    }

    pub fn select_track(&mut self, id: TrackId) -> Result<(), SessionError> {
        self.requests.inspecting_media = false;
        self.submit(Command::Select {
            selection: SelectionState::Track(id),
        })
        .map(|_| ())
    }

    pub fn clear_selection(&mut self) -> Result<(), SessionError> {
        self.requests.inspecting_media = false;
        self.submit(Command::Select {
            selection: SelectionState::None,
        })
        .map(|_| ())
    }

    pub fn select_media(&mut self, id: MediaId) -> Result<(), SessionError> {
        self.submit(Command::SelectMedia { media_id: id })?;
        self.requests.desired_media = Some(id);
        self.requests.inspecting_media = true;
        self.media.pending_selection();
        Ok(())
    }

    /// Starts a probe for a path the frontend's native picker returned.
    pub fn import(&mut self, path: PathBuf) -> Result<(), SessionError> {
        let Some(project) = self.current_project() else {
            return Err(SessionError::Invalid(
                "The project is not ready yet. Try again in a moment.",
            ));
        };
        if self.media.import_busy() || self.requests.verifying.is_some() {
            return Err(SessionError::Invalid(
                "Another import is in progress. Cancel it or wait for it to finish.",
            ));
        }
        self.dirty = true;
        self.media.start_probe(
            project,
            MediaId::new(),
            path,
            false,
            false,
            &mut self.status,
        )?;
        Ok(())
    }

    pub fn cancel_import(&mut self) {
        self.dirty = true;
        self.media.cancel_import();
    }

    fn preserve_timeline_position(&mut self) -> Result<(), SessionError> {
        if self.timeline_playback.active() {
            self.timeline_preview
                .set_position(
                    self.timeline_playback.position(),
                    self.timeline_playback.timebase(),
                )
                .map_err(|e| EngineError::new(ErrorCode::InvalidInput, e.to_string()))?;
        }
        Ok(())
    }
    fn invalidate_timeline(&mut self) {
        if let Err(e) = self.preserve_timeline_position() {
            self.status = StatusView {
                text: e.to_string(),
                error: true,
            };
        }
        self.timeline_request = RequestId::new();
        self.timeline_desired = None;
        self.timeline_waiting = false;
        self.timeline_intent = false;
        if let Err(e) = self.timeline_playback.cancel() {
            self.status = StatusView {
                text: e.to_string(),
                error: true,
            };
        }
        if self.preview_mode == PreviewMode::Timeline {
            if let Err(error) = self.timeline_preview.invalidate(&mut self.media) {
                self.status = StatusView {
                    text: error.to_string(),
                    error: true,
                };
            }
            self.dirty = true;
        }
    }
    pub fn set_preview_mode(&mut self, mode: PreviewMode) -> Result<(), SessionError> {
        if self.closed {
            return Err(SessionError::Invalid("The session is shut down."));
        }
        if mode == self.preview_mode {
            return Ok(());
        }
        if self.current_project().is_none() {
            return Err(SessionError::Invalid("Wait for the project to be ready."));
        }
        self.media
            .show_source(mode == PreviewMode::Source, &mut self.status)?;
        self.preview_mode = mode;
        if mode == PreviewMode::Timeline {
            if !self.timeline_playback.active() {
                self.invalidate_timeline();
            } else {
                self.timeline_playback
                    .restore_frame_generation(&mut self.media);
            }
        } else {
            self.pause_timeline()?;
            self.timeline_request = RequestId::new();
            self.timeline_desired = None;
            self.timeline_waiting = false;
            self.timeline_preview.cancel(&mut self.media)?;
        }
        self.dirty = true;
        Ok(())
    }
    pub fn seek_timeline(&mut self, position: TimelineSeek) -> Result<(), SessionError> {
        let value = match position {
            TimelineSeek::Ticks(t) => t.0,
            TimelineSeek::Milliseconds(ms) => ms,
        };
        if value < 0 {
            return Err(SessionError::Invalid(
                "Timeline position must be nonnegative.",
            ));
        }
        self.set_preview_mode(PreviewMode::Timeline)?;
        if self.timeline_playback.active() || self.timeline_waiting {
            let playing = if self.timeline_waiting {
                self.timeline_intent
            } else {
                self.timeline_playback.view.desired_playing
            };
            self.queue_timeline_transport(position, playing, true)?;
        } else {
            self.timeline_preview.queue(position, &mut self.media)?;
        }
        self.dirty = true;
        Ok(())
    }
    pub fn seek_timeline_text(&mut self, text: &str) -> Result<(), SessionError> {
        let input = if let Some(ticks) = text.trim().strip_suffix("ticks") {
            TimelineSeek::Ticks(TimelineTime(ticks.trim().parse().map_err(|_| {
                SessionError::Invalid("Enter an integer project-tick position followed by 'ticks'.")
            })?))
        } else {
            TimelineSeek::Milliseconds(i64::from(
                view::parse_source_time(text).map_err(SessionError::Invalid)?,
            ))
        };
        self.seek_timeline(input)
    }
    pub fn cancel_timeline_preview(&mut self) -> Result<(), SessionError> {
        self.preserve_timeline_position()?;
        self.timeline_playback.cancel()?;
        self.timeline_request = RequestId::new();
        self.timeline_desired = None;
        self.timeline_waiting = false;
        self.timeline_intent = false;
        self.timeline_preview.cancel(&mut self.media)?;
        self.dirty = true;
        Ok(())
    }

    pub fn seek(&mut self, milliseconds: i64) {
        if self.preview_mode != PreviewMode::Source {
            return;
        }
        self.dirty = true;
        self.media.seek(milliseconds, &mut self.status);
    }

    /// Parses the viewer's go-to text with the tested source-time grammar.
    pub fn seek_text(&mut self, text: &str) -> Result<(), SessionError> {
        self.dirty = true;
        match view::parse_source_time(text) {
            Ok(ms) => {
                if !self.source_ready() {
                    return Err(SessionError::Invalid("Select a source before seeking."));
                }
                self.media.seek(i64::from(ms), &mut self.status);
                Ok(())
            }
            Err(message) => {
                self.status = StatusView {
                    text: message.into(),
                    error: true,
                };
                Err(SessionError::Invalid(message))
            }
        }
    }

    pub fn cancel_preview(&mut self) {
        self.dirty = true;
        self.media.cancel_preview(&mut self.status);
    }
    pub fn play(&mut self) -> Result<(), SessionError> {
        if self.preview_mode != PreviewMode::Source {
            return Err(SessionError::Invalid(
                "Timeline playback is unavailable. Select Source to play source media.",
            ));
        }
        if self.current_project().is_none() || self.media.selected_media().is_none() {
            return Err(SessionError::Invalid("Select a source before playback."));
        }
        self.media.play()?;
        self.dirty = true;
        Ok(())
    }
    fn queue_timeline_transport(
        &mut self,
        position: TimelineSeek,
        playing: bool,
        seeking: bool,
    ) -> Result<(), SessionError> {
        self.preserve_timeline_position()?;
        self.timeline_preview.cancel(&mut self.media)?;
        self.timeline_playback.cancel()?;
        self.timeline_request = RequestId::new();
        self.timeline_desired = Some(position);
        self.timeline_intent = playing;
        self.timeline_seeking = seeking;
        self.timeline_waiting = true;
        self.dirty = true;
        Ok(())
    }
    pub fn play_timeline(&mut self) -> Result<(), SessionError> {
        if self.preview_mode != PreviewMode::Timeline || self.current_project().is_none() {
            return Err(SessionError::Invalid(
                "Select Timeline before timeline playback.",
            ));
        }
        if self.timeline_waiting {
            self.timeline_intent = true;
            self.dirty = true;
            return Ok(());
        }
        if self.timeline_playback.active()
            && !matches!(self.timeline_playback.view.state, "ended" | "failed")
        {
            self.timeline_playback.resume(std::time::Instant::now());
            self.timeline_intent = true;
            self.dirty = true;
            return Ok(());
        }
        let position = if self.timeline_playback.view.state == "ended" {
            TimelineTime::ZERO
        } else if self.timeline_playback.active() {
            self.timeline_playback.position()
        } else {
            TimelineTime(self.timeline_preview.view.position_ticks)
        };
        self.queue_timeline_transport(TimelineSeek::Ticks(position), true, false)
    }
    pub fn pause_timeline(&mut self) -> Result<(), SessionError> {
        self.timeline_intent = false;
        self.timeline_playback
            .pause(std::time::Instant::now())
            .map_err(|_| SessionError::Invalid("Timeline clock overflow."))?;
        self.dirty = true;
        Ok(())
    }
    pub fn stop_timeline(&mut self) -> Result<(), SessionError> {
        self.timeline_playback.stop()?;
        self.timeline_preview
            .set_position(
                TimelineTime::ZERO,
                self.requests
                    .snapshot
                    .as_ref()
                    .map_or(Timebase::DEFAULT_TIMELINE, |s| s.timebase),
            )
            .map_err(|e| EngineError::new(ErrorCode::InvalidInput, e.to_string()))?;
        self.timeline_request = RequestId::new();
        self.timeline_desired = None;
        self.timeline_waiting = false;
        self.timeline_intent = false;
        self.timeline_preview
            .queue(TimelineSeek::Ticks(TimelineTime::ZERO), &mut self.media)?;
        self.dirty = true;
        Ok(())
    }
    pub fn pause(&mut self) {
        self.media.pause();
        self.dirty = true;
    }
    pub fn stop(&mut self) -> Result<(), SessionError> {
        if self.preview_mode != PreviewMode::Source {
            return Err(SessionError::Invalid("Timeline playback is unavailable."));
        }
        self.media.stop(&mut self.status)?;
        self.dirty = true;
        Ok(())
    }
    pub fn copy_allowed(&self, generation: u64) -> bool {
        match self.preview_mode {
            PreviewMode::Source => self.media.copy_allowed(generation),
            PreviewMode::Timeline => {
                if self.timeline_playback.active() {
                    self.timeline_playback.copy_allowed(generation)
                } else {
                    self.timeline_preview.copy_allowed(generation)
                }
            }
        }
    }
    pub fn source_ready(&self) -> bool {
        self.preview_mode == PreviewMode::Source
            && self.current_project().is_some()
            && self.media.selected_media().is_some()
    }
    pub fn frame_copied(&mut self, generation: u64) -> Result<(), SessionError> {
        match self.preview_mode {
            PreviewMode::Source => self.media.frame_copied(generation)?,
            PreviewMode::Timeline => {
                if self.timeline_playback.active() {
                    self.timeline_playback.copied(generation);
                } else {
                    self.timeline_preview.copied(generation);
                }
            }
        }
        self.dirty = true;
        Ok(())
    }

    /// The project new work may target: none while a replacement is pending.
    fn current_project(&self) -> Option<ProjectId> {
        if self.requests.waiting_project.is_none() {
            self.project_id()
        } else {
            None
        }
    }

    /// Drains bounded worker results. Returns true when the view changed.
    pub fn pump(&mut self) -> bool {
        if self.closed {
            return false;
        }
        let mut events = Vec::new();
        if let Err(error) = self.bridge.drain(|event| events.push(event)) {
            tracing::error!("{error}");
            self.requests.pending.clear();
            self.requests.file_pending.clear();
            self.engine_ready = false;
            self.status = StatusView {
                text: error.to_string(),
                error: true,
            };
            self.dirty = true;
        }
        for event in events {
            self.dirty = true;
            match event {
                HostEvent::Engine(event) => self.apply(*event),
                HostEvent::TimelinePlan { request_id, result } => {
                    if self.timeline_inflight == Some(request_id) {
                        self.timeline_inflight = None;
                    }
                    if request_id == self.timeline_request
                        && self.preview_mode == PreviewMode::Timeline
                    {
                        self.timeline_waiting = false;
                        match result {
                            Ok(plan)
                                if self.requests.snapshot.as_ref().is_some_and(|s| {
                                    s.id == plan.project
                                        && s.evaluation_generation == plan.generation
                                }) =>
                            {
                                if let Err(e) = self.timeline_playback.begin(
                                    plan,
                                    self.timeline_intent,
                                    self.timeline_seeking,
                                    std::time::Instant::now(),
                                ) {
                                    self.status = StatusView {
                                        text: e.to_string(),
                                        error: true,
                                    };
                                }
                            }
                            Ok(_) => {
                                self.timeline_intent = false;
                            }
                            Err(e) => {
                                self.timeline_intent = false;
                                self.timeline_playback.rejected(e.to_string());
                                self.status = StatusView {
                                    text: e.to_string(),
                                    error: true,
                                };
                            }
                        }
                    }
                }
            }
        }
        let current = self.current_project();
        let (registration, changed) = self.media.poll(current, &mut self.status);
        self.dirty |= changed;
        if let Some(outcome) = self.media.take_timeline() {
            if self.preview_mode == PreviewMode::Timeline
                && !self.timeline_playback.active()
                && !self.timeline_waiting
            {
                self.timeline_preview.decoded(outcome, &mut self.media);
            }
            self.dirty = true;
        }
        if let Some(registration) = registration
            && current == Some(registration.project_id)
        {
            let id = registration.media_id;
            let checking = registration.checking;
            let command = if checking {
                Command::VerifyMedia {
                    media_id: id,
                    result: registration.result,
                    relink: registration.relink,
                }
            } else {
                match registration.result {
                    Ok(verified) => Command::RegisterMedia {
                        media: Box::new(verified.media),
                        fingerprint: Some(verified.fingerprint),
                    },
                    Err(error) => {
                        self.media
                            .import_error(&error.to_string(), &mut self.status);
                        return self.dirty;
                    }
                }
            };
            if checking {
                self.requests.verified = Some(command);
            } else {
                match self.submit(command) {
                    Ok(request) => {
                        self.requests.registering = Some(request);
                        self.requests.desired_media = Some(id);
                        self.requests.inspecting_media = true;
                    }
                    Err(_) => self.media.import_error(
                        "The project queue is busy. Try importing again.",
                        &mut self.status,
                    ),
                }
            }
        }
        if let Some(command) = self.requests.verified.clone()
            && let Ok(request) = self.submit(command)
        {
            self.requests.verified = None;
            self.requests.verification_request = Some(request);
        }
        if !self.media.import_busy()
            && self.requests.verifying.is_none()
            && let Some((id, path)) = self.requests.checks.pop_front()
            && let Some(project) = current
        {
            if let Err(error) =
                self.media
                    .start_probe(project, id, path, true, false, &mut self.status)
            {
                self.requests.verified = Some(Command::VerifyMedia {
                    media_id: id,
                    result: Err(EngineError::new(ErrorCode::Internal, error.to_string())),
                    relink: false,
                });
            }
            self.requests.verifying = Some(id);
            self.dirty = true;
        }
        if self.preview_mode == PreviewMode::Timeline
            && !self.timeline_playback.active()
            && !self.timeline_waiting
            && self.engine_ready
            && let Some(project) = self.current_project()
            && let Some(query) = self.timeline_preview.next_query(project)
        {
            let request = query.request_id;
            match self.bridge.submit(query) {
                Ok(()) => self.timeline_preview.submitted(request),
                Err(error) => {
                    self.status = StatusView {
                        text: error.to_string(),
                        error: true,
                    };
                    self.dirty = true;
                }
            }
        }
        if self.preview_mode == PreviewMode::Timeline && self.engine_ready {
            if self.timeline_playback.active()
                && !self.requests.snapshot.as_ref().is_some_and(|s| {
                    self.timeline_playback
                        .valid_scope(s.id, s.evaluation_generation)
                })
            {
                self.invalidate_timeline();
            }
            if self.timeline_inflight.is_none()
                && let Some(position) = self.timeline_desired
                && let Some(project) = self.current_project()
            {
                let request_id = self.timeline_request;
                match self.bridge.submit(CommandEnvelope {
                    request_id,
                    project_id: Some(project),
                    command: Command::PrepareTimelinePlayback { position },
                }) {
                    Ok(()) => {
                        self.timeline_desired = None;
                        self.timeline_inflight = Some(request_id);
                    }
                    Err(e) => {
                        self.status = StatusView {
                            text: e.to_string(),
                            error: true,
                        };
                    }
                }
            }
            self.dirty |= self
                .timeline_playback
                .poll(std::time::Instant::now(), &mut self.media);
        }
        self.dirty
    }

    fn apply(&mut self, event: EngineEvent) {
        match event {
            EngineEvent::TimelineEvaluated { request_id, result } => {
                let scope = self
                    .requests
                    .snapshot
                    .as_ref()
                    .map(|s| (s.id, s.evaluation_generation));
                self.timeline_preview.evaluated(
                    request_id,
                    result,
                    self.preview_mode == PreviewMode::Timeline
                        && !self.timeline_playback.active()
                        && !self.timeline_waiting,
                    scope,
                    &mut self.media,
                );
            }
            EngineEvent::Updated { snapshot } => {
                self.engine_ready = snapshot.document.state != "opening";
                self.set_snapshot(snapshot);
            }
            EngineEvent::FileFailed {
                request_id,
                error,
                snapshot,
            } => {
                let verifying = self.requests.verification_request == Some(request_id);
                if let Some(snapshot) = snapshot {
                    self.apply(EngineEvent::Completed {
                        request_id,
                        snapshot,
                    });
                }
                if verifying {
                    self.media
                        .import_error(&error.to_string(), &mut self.status);
                }
                self.apply(EngineEvent::Failed { request_id, error });
            }
            EngineEvent::Completed {
                request_id,
                snapshot,
            } => {
                self.requests.pending.remove(&request_id);
                self.requests.file_pending.remove(&request_id);
                if self
                    .requests
                    .waiting_project
                    .is_some_and(|waiting| waiting != request_id)
                {
                    // A result for the project being replaced.
                    return;
                }
                if self.requests.waiting_project == Some(request_id) {
                    self.requests.waiting_project = None;
                }
                self.engine_ready = snapshot.document.state != "opening";
                if self.requests.verification_request == Some(request_id) {
                    if let Some(state) = snapshot
                        .media_state
                        .iter()
                        .find(|m| Some(m.id) == self.requests.verifying)
                    {
                        self.media.import = view::ImportView {
                            busy: false,
                            error: state.state != Availability::Ready,
                            message: state.detail.clone(),
                        };
                    }
                    self.requests.verification_request = None;
                    self.requests.verifying = None;
                }
                let replaced = self.project_id() != Some(snapshot.id);
                let opened = snapshot.document.state == "opened"
                    && self.requests.snapshot.as_ref().is_none_or(|s| {
                        s.document.state != "opened"
                            || s.document.operation_id != snapshot.document.operation_id
                    });
                if opened || replaced {
                    self.requests.checks.clear();
                    self.requests.verifying = None;
                    self.requests.verification_request = None;
                    self.requests.verified = None;
                    if opened {
                        self.media.reset();
                        self.timeline_preview.reset_position();
                        self.requests.desired_media = None;
                        self.requests.inspecting_media = false;
                        self.requests.checks.extend(
                            snapshot
                                .media_state
                                .iter()
                                .filter(|m| m.state == Availability::Checking)
                                .map(|m| (m.id, m.path.clone())),
                        );
                    }
                }
                if snapshot.source_media == self.requests.desired_media
                    && let Some(selected) = snapshot
                        .media
                        .iter()
                        .find(|m| Some(m.id) == snapshot.source_media)
                {
                    if snapshot
                        .media_state
                        .iter()
                        .any(|m| m.id == selected.id && m.state == Availability::Ready)
                    {
                        self.media
                            .select(snapshot.id, selected.clone(), &mut self.status);
                    } else if let Some(state) =
                        snapshot.media_state.iter().find(|m| m.id == selected.id)
                    {
                        self.media.pending_selection();
                        self.media.source.title = view::file_name(selected);
                        self.media.source.status = "Source unavailable".into();
                        self.media.source.detail = state.detail.clone();
                    }
                }
                tracing::info!(project = %snapshot.id, tracks = snapshot.tracks.len(),
                    selection = ?snapshot.selection, "command completed");
                if self.requests.registering == Some(request_id) {
                    self.requests.registering = None;
                    let imported = snapshot
                        .media
                        .iter()
                        .find(|m| Some(m.id) == snapshot.source_media)
                        .map(view::file_name);
                    self.media.import_finished(imported);
                }
                self.set_snapshot(snapshot);
                self.status = StatusView {
                    text: "Ready".into(),
                    error: false,
                };
                // An edit aligned to the source tick grid says so; otherwise Ready.
                if let Some(notice) = self
                    .requests
                    .snapshot
                    .as_ref()
                    .and_then(|s| s.edit_notice.clone())
                {
                    self.status.text = notice;
                }
            }
            EngineEvent::Failed { request_id, error } => {
                self.requests.pending.remove(&request_id);
                self.requests.file_pending.remove(&request_id);
                tracing::warn!(code = ?error.code, "command failed: {error}");
                self.status = StatusView {
                    text: error.to_string(),
                    error: true,
                };
                if self.requests.registering == Some(request_id) {
                    self.requests.registering = None;
                    self.media
                        .import_error(&error.to_string(), &mut self.status);
                }
                if self.requests.waiting_project == Some(request_id) {
                    self.requests.waiting_project = None;
                    self.engine_ready = true;
                }
            }
        }
    }

    fn set_snapshot(&mut self, snapshot: ProjectSnapshot) {
        let changed = self
            .requests
            .snapshot
            .as_ref()
            .is_none_or(|s| s.evaluation_generation != snapshot.evaluation_generation);
        self.clip_lookup = snapshot
            .clips
            .iter()
            .enumerate()
            .map(|(i, c)| (c.id, i))
            .collect();
        self.requests.snapshot = Some(snapshot);
        self.timeline_generation += 1;
        self.timeline_cached = None;
        if changed {
            self.invalidate_timeline();
        }
    }

    pub fn view(&self) -> SessionView {
        let snapshot = self.requests.snapshot.as_ref();
        let source = snapshot.and_then(|snapshot| {
            snapshot
                .media
                .iter()
                .find(|m| Some(m.id) == snapshot.source_media)
                .filter(|_| self.requests.inspecting_media)
        });
        let inspector = match snapshot {
            None => InspectorView {
                kind: "project",
                title: "Starting…".into(),
                detail: String::new(),
                sections: Vec::new(),
            },
            Some(snapshot) => {
                let track = snapshot
                    .tracks
                    .iter()
                    .find(|track| snapshot.selection == SelectionState::Track(track.id));
                let clip = match snapshot.selection {
                    SelectionState::Clip(id) => self
                        .clip_lookup
                        .get(&id)
                        .and_then(|i| snapshot.clips.get(*i)),
                    _ => None,
                };
                if let Some(clip) = clip.filter(|_| source.is_none()) {
                    let track_name = snapshot
                        .tracks
                        .iter()
                        .find(|t| t.id == clip.track_id)
                        .map_or_else(String::new, |t| t.name.clone());
                    InspectorView {
                        kind: "clip",
                        title: view::clip_name(snapshot, clip.media_id),
                        detail: format!("Video clip on {track_name}"),
                        sections: view::clip_sections(snapshot, clip),
                    }
                } else if let Some(asset) = source {
                    InspectorView {
                        kind: "source",
                        title: view::file_name(asset),
                        detail: "Source video, read only".into(),
                        sections: view::source_sections(asset),
                    }
                } else if let Some(track) = track {
                    InspectorView {
                        kind: "track",
                        title: track.name.clone(),
                        detail: match (track.kind, track.clip_count) {
                            (TrackKind::Video, 0) => "Empty video track".into(),
                            (TrackKind::Audio, 0) => "Empty audio track".into(),
                            (TrackKind::Video, 1) => "Video track · 1 clip".into(),
                            (TrackKind::Video, n) => format!("Video track · {n} clips"),
                            (TrackKind::Audio, 1) => "Audio track · 1 clip".into(),
                            (TrackKind::Audio, n) => format!("Audio track · {n} clips"),
                        },
                        sections: view::track_sections(snapshot, track),
                    }
                } else {
                    InspectorView {
                        kind: "project",
                        title: snapshot.name.clone(),
                        detail: "Project overview".into(),
                        sections: view::project_sections(snapshot),
                    }
                }
            }
        };
        let mut timeline_preview = if self.timeline_playback.active() {
            self.timeline_playback.preview()
        } else {
            self.timeline_preview.view.clone()
        };
        let mut timeline_playback = self.timeline_playback.view.clone();
        if self.timeline_waiting {
            timeline_playback.state = if self.timeline_seeking {
                "seeking"
            } else {
                "starting"
            };
            timeline_playback.desired_playing = self.timeline_intent;
            timeline_playback.epoch = Some(self.timeline_request.to_string());
            timeline_preview.state = timeline_playback.state;
            timeline_preview.busy = true;
            timeline_preview.detail = "Preparing the exact timeline transport request…".into();
        }
        SessionView {
            revision: self.revision,
            engine_ready: self.engine_ready,
            project: snapshot.map(|snapshot| ProjectView {
                id: snapshot.id.to_string(),
                name: snapshot.name.clone(),
                saved: !snapshot.document.dirty && snapshot.document.path.is_some(),
                clip_count: snapshot.clip_count,
                document: snapshot.document.clone(),
            }),
            status: self.status.clone(),
            pending: u32::try_from(self.requests.pending.len()).unwrap_or(u32::MAX),
            tracks: snapshot.map(view::track_views).unwrap_or_default(),
            media: snapshot
                .map(|snapshot| {
                    snapshot
                        .media_state
                        .iter()
                        .map(|state| {
                            view::media_view_with_state(
                                snapshot.media.iter().find(|m| m.id == state.id),
                                state,
                                snapshot.source_media == Some(state.id),
                            )
                        })
                        .collect()
                })
                .unwrap_or_default(),
            import: crate::view::ImportView {
                busy: self.media.import.busy || self.requests.verifying.is_some(),
                ..self.media.import.clone()
            },
            inspector,
            source: self.media.source.clone(),
            frame: self.frame().map(|f| view::FrameView {
                generation: f.generation,
                width: f.image.width(),
                height: f.image.height(),
                actual: f.actual,
                epoch: f.ticket.request_id.to_string(),
            }),
            metrics: self.media.metrics,
            playback: self.media.playback.view.clone(),
            timeline_generation: self.timeline_generation,
            history: snapshot.map(|s| s.history.clone()).unwrap_or_default(),
            editing: self.editing_view(),
            preview_mode: self.preview_mode,
            timeline_preview,
            timeline_playback,
        }
    }

    /// The serialized view. Serializes at most once per change.
    pub fn view_json(&mut self) -> Result<&str, SessionError> {
        if self.dirty || self.cached.is_none() {
            self.revision += 1;
            self.dirty = false;
            let json = serde_json::to_string(&self.view())
                .map_err(|_| SessionError::Invalid("The view could not be serialized."))?;
            self.cached = Some(json);
        }
        Ok(self.cached.as_deref().unwrap_or_default())
    }

    /// The serialized clip list. Rebuilt at most once per project snapshot.
    pub fn timeline_json(&mut self) -> Result<&str, SessionError> {
        if self.timeline_cached.is_none() {
            let timeline = view::TimelineView {
                generation: self.timeline_generation,
                clips: self
                    .requests
                    .snapshot
                    .as_ref()
                    .map(view::clip_views)
                    .unwrap_or_default(),
            };
            let json = serde_json::to_string(&timeline)
                .map_err(|_| SessionError::Invalid("The timeline could not be serialized."))?;
            self.timeline_cached = Some(json);
        }
        Ok(self.timeline_cached.as_deref().unwrap_or_default())
    }

    pub fn frame(&self) -> Option<&PresentedFrame> {
        match self.preview_mode {
            PreviewMode::Source => self.media.frame(),
            PreviewMode::Timeline => {
                if self.timeline_playback.active() {
                    self.timeline_playback.frame.as_ref()
                } else {
                    self.timeline_preview.frame.as_ref()
                }
            }
        }
    }

    /// Stops media work, then the command worker. Idempotent.
    pub fn shutdown(&mut self) -> Result<(), SessionError> {
        if self.closed {
            return Ok(());
        }
        self.closed = true;
        let timeline = self.timeline_playback.shutdown();
        let media = self.media.shutdown();
        let bridge = self.bridge.shutdown();
        media?;
        timeline?;
        bridge?;
        Ok(())
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if let Err(error) = self.shutdown() {
            tracing::error!("session shutdown: {error}");
        }
    }
}
