//! One editor session: the command owner, the media host and the derived view.
//!
//! Ported from the Slint coordinator. The frontend calls these methods from its
//! UI thread, then calls `pump` on a timer to drain bounded worker results. No
//! callbacks cross into the frontend: it pulls the coalesced view and frame.
use crate::{
    bridge::EngineBridge,
    media::{MediaHost, PresentedFrame},
    view::{self, InspectorView, ProjectView, SessionView, StatusView},
};
use cutcap_contracts::*;
use cutcap_media::{FfmpegBackend, MediaBackend, MediaError};
use std::{collections::HashSet, path::PathBuf, sync::Arc};

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
    desired_media: Option<MediaId>,
    waiting_project: Option<RequestId>,
    registering: Option<RequestId>,
    inspecting_media: bool,
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
    closed: bool,
}

impl Session {
    pub fn start() -> Result<Self, SessionError> {
        Self::with_backend(Arc::new(FfmpegBackend::default()))
    }

    pub fn with_backend(backend: Arc<dyn MediaBackend>) -> Result<Self, SessionError> {
        let mut session = Self {
            bridge: EngineBridge::start()?,
            media: MediaHost::start(backend)?,
            requests: Requests::default(),
            status: StatusView {
                text: "Starting command worker…".into(),
                error: false,
            },
            engine_ready: false,
            dirty: true,
            revision: 0,
            cached: None,
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
        let request_id = self.submit(Command::NewProject {
            name: DEFAULT_PROJECT_NAME.into(),
        })?;
        self.requests.waiting_project = Some(request_id);
        self.requests.desired_media = None;
        self.requests.inspecting_media = false;
        self.media.reset();
        self.engine_ready = false;
        Ok(())
    }

    pub fn add_track(&mut self, kind: TrackKind) -> Result<(), SessionError> {
        self.submit(Command::AddTrack { kind }).map(|_| ())
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
        if self.media.import_busy() {
            return Err(SessionError::Invalid(
                "Another import is in progress. Cancel it or wait for it to finish.",
            ));
        }
        self.dirty = true;
        self.media.start_probe(project, path, &mut self.status);
        Ok(())
    }

    pub fn cancel_import(&mut self) {
        self.dirty = true;
        self.media.cancel_import();
    }

    pub fn seek(&mut self, milliseconds: i64) {
        self.dirty = true;
        self.media.seek(milliseconds, &mut self.status);
    }

    /// Parses the viewer's go-to text with the tested source-time grammar.
    pub fn seek_text(&mut self, text: &str) -> Result<(), SessionError> {
        self.dirty = true;
        match view::parse_source_time(text) {
            Ok(ms) => {
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
            self.engine_ready = false;
            self.status = StatusView {
                text: error.to_string(),
                error: true,
            };
            self.dirty = true;
        }
        for event in events {
            self.dirty = true;
            self.apply(event);
        }
        let current = self.current_project();
        let (registration, changed) = self.media.poll(current, &mut self.status);
        self.dirty |= changed;
        if let Some(registration) = registration
            && current == Some(registration.project_id)
        {
            let id = registration.media.id;
            match self.submit(Command::RegisterMedia {
                media: Box::new(registration.media),
            }) {
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
        self.dirty
    }

    fn apply(&mut self, event: EngineEvent) {
        match event {
            EngineEvent::Completed {
                request_id,
                snapshot,
            } => {
                self.requests.pending.remove(&request_id);
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
                self.engine_ready = true;
                if snapshot.source_media == self.requests.desired_media
                    && let Some(selected) = snapshot
                        .media
                        .iter()
                        .find(|m| Some(m.id) == snapshot.source_media)
                {
                    self.media
                        .select(snapshot.id, selected.clone(), &mut self.status);
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
                self.requests.snapshot = Some(snapshot);
                self.status = StatusView {
                    text: "Ready".into(),
                    error: false,
                };
            }
            EngineEvent::Failed { request_id, error } => {
                self.requests.pending.remove(&request_id);
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
                if let Some(asset) = source {
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
                        detail: match track.kind {
                            TrackKind::Video => "Empty video track",
                            TrackKind::Audio => "Empty audio track",
                        }
                        .into(),
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
        SessionView {
            revision: self.revision,
            engine_ready: self.engine_ready,
            project: snapshot.map(|snapshot| ProjectView {
                id: snapshot.id.to_string(),
                name: snapshot.name.clone(),
                saved: false,
                clip_count: snapshot.clip_count,
            }),
            status: self.status.clone(),
            pending: u32::try_from(self.requests.pending.len()).unwrap_or(u32::MAX),
            tracks: snapshot.map(view::track_views).unwrap_or_default(),
            media: snapshot
                .map(|snapshot| {
                    snapshot
                        .media
                        .iter()
                        .map(|asset| {
                            view::media_view(asset, snapshot.source_media == Some(asset.id))
                        })
                        .collect()
                })
                .unwrap_or_default(),
            import: self.media.import.clone(),
            inspector,
            source: self.media.source.clone(),
            frame: self.media.frame_view(),
            metrics: self.media.metrics,
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

    pub fn frame(&self) -> Option<&PresentedFrame> {
        self.media.frame()
    }

    /// Stops media work, then the command worker. Idempotent.
    pub fn shutdown(&mut self) -> Result<(), SessionError> {
        if self.closed {
            return Ok(());
        }
        self.closed = true;
        let media = self.media.shutdown();
        let bridge = self.bridge.shutdown();
        media?;
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
