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
    /// Phase 4A filmstrip thumbnails: presentation only, never project state.
    thumbnails: crate::thumbnails::ThumbnailService,
    /// The completed thumbnail announced by `next_thumbnail`, awaiting its copy.
    thumbnail_slot: Option<crate::thumbnails::Done>,
    /// Stats last published in the view (a change republishes it).
    thumbnail_stats_seen: crate::thumbnails::ThumbnailStats,
    /// Phase 4B waveform peaks: presentation only, never project state.
    waveforms: crate::waveforms::WaveformService,
    waveform_slot: Option<crate::waveforms::Done>,
    waveform_stats_seen: crate::waveforms::WaveformStats,
    /// A volume drag in progress: heard live, not a project change until committed.
    gain_preview: Option<(TrackId, i32)>,
    /// The commit that will end the preview once its snapshot arrives.
    gain_commit: Option<RequestId>,
    /// The exact partner the selected clip can be linked with (per snapshot).
    link_candidate: Option<ClipId>,
    /// Phase 5A: the one export job, independent of both monitors.
    export: crate::export::ExportHost,
    /// A start waiting for the command owner to capture its plan.
    export_pending: Option<PendingExport>,
    /// Built once per snapshot, so an idle or playing view never recomputes it.
    export_summary: crate::export::ExportSummary,
    closed: bool,
}

struct PendingExport {
    request_id: RequestId,
    job: String,
    output: crate::export::OutputRequest,
}

/// One waveform chunk the frontend wants (wire form of the demand list).
#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct WaveformWant {
    token: u64,
    media_id: MediaId,
    audio_key: String,
    stream: u32,
    chunk: i64,
}

/// One filmstrip sample the frontend wants (wire form of the demand list).
#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ThumbnailWant {
    token: u64,
    media_id: MediaId,
    source_key: String,
    /// Source time in microseconds on the source's own time axis.
    sample_us: i64,
    height: u32,
}

/// Opens the default Windows output for the timeline's audio master. Without
/// a device (or with `CUTCAP_AUDIO=off`) playback keeps the system clock and
/// the view explains why.
fn open_audio(
    backend: &Arc<dyn MediaBackend>,
) -> (
    Option<crate::audio_transport::AudioTransport>,
    Option<String>,
) {
    if std::env::var("CUTCAP_AUDIO").is_ok_and(|v| v.eq_ignore_ascii_case("off")) {
        return (
            None,
            Some("Audio output is disabled (CUTCAP_AUDIO=off).".into()),
        );
    }
    match crate::audio_output::AudioOutput::open_default() {
        Ok(output) => {
            match crate::audio_transport::AudioTransport::start(backend.clone(), output) {
                Ok(transport) => (Some(transport), None),
                Err(error) => (
                    None,
                    Some(format!("Audio playback could not start: {error}")),
                ),
            }
        }
        Err(error) => (None, Some(error)),
    }
}

impl Session {
    /// The production session: real FFmpeg and the default audio output.
    pub fn start() -> Result<Self, SessionError> {
        let backend: Arc<dyn MediaBackend> = Arc::new(FfmpegBackend::default());
        let (audio, note) = open_audio(&backend);
        Self::with_parts(backend, audio, note)
    }

    /// A session without audio output (tests and diagnostics).
    pub fn with_backend(backend: Arc<dyn MediaBackend>) -> Result<Self, SessionError> {
        Self::with_parts(
            backend,
            None,
            Some("No audio output in this session.".into()),
        )
    }

    fn with_parts(
        backend: Arc<dyn MediaBackend>,
        audio: Option<crate::audio_transport::AudioTransport>,
        audio_note: Option<String>,
    ) -> Result<Self, SessionError> {
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
            timeline_playback: crate::timeline_playback::TimelinePlayback::start_with_audio(
                backend.clone(),
                audio,
                audio_note,
            )?,
            timeline_request: RequestId::new(),
            timeline_desired: None,
            timeline_inflight: None,
            timeline_intent: false,
            timeline_seeking: false,
            timeline_waiting: false,
            clip_lookup: HashMap::new(),
            thumbnails: crate::thumbnails::ThumbnailService::start(backend.clone())?,
            thumbnail_slot: None,
            thumbnail_stats_seen: Default::default(),
            export: crate::export::ExportHost::start(backend.clone())?,
            export_pending: None,
            export_summary: crate::export::ExportSummary::default(),
            waveforms: crate::waveforms::WaveformService::start(backend)?,
            waveform_slot: None,
            waveform_stats_seen: Default::default(),
            gain_preview: None,
            gain_commit: None,
            link_candidate: None,
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
        // Track controls and their undo steps change only the mix: the
        // transport keeps playing. (If this snapshot is behind the owner, the
        // generation check in `pump` still stops playback for a real edit.)
        let history = self.requests.snapshot.as_ref().map(|s| &s.history);
        let live_undo = match command {
            Command::Undo => history.is_some_and(|h| h.undo_track),
            Command::Redo => history.is_some_and(|h| h.redo_track),
            _ => false,
        };
        let invalidate = !live_undo
            && !matches!(
                command,
                Command::GetSnapshot
                    | Command::SaveProject { .. }
                    | Command::SelectMedia { .. }
                    | Command::Select { .. }
                    | Command::SetTrackGain { .. }
                    | Command::SetTrackMute { .. }
                    | Command::SetTrackSolo { .. }
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

    /// Places one chosen audio stream as an independent clip (undoable).
    pub fn place_audio_stream(
        &mut self,
        media: MediaId,
        stream: u32,
        track: Option<TrackId>,
        position: TimelineSeek,
    ) -> Result<(), SessionError> {
        self.requests.inspecting_media = false;
        self.submit(Command::PlaceAudioStream {
            media_id: media,
            stream,
            track_id: track,
            position,
        })
        .map(|_| ())
    }

    pub fn unlink_clip(&mut self, id: ClipId) -> Result<(), SessionError> {
        self.submit(Command::UnlinkClip { clip_id: id }).map(|_| ())
    }

    pub fn link_clips(&mut self, id: ClipId, partner: ClipId) -> Result<(), SessionError> {
        self.submit(Command::LinkClips {
            clip_id: id,
            partner_id: partner,
        })
        .map(|_| ())
    }

    pub fn set_track_mute(&mut self, track: TrackId, mute: bool) -> Result<(), SessionError> {
        self.submit(Command::SetTrackMute {
            track_id: track,
            mute,
        })
        .map(|_| ())
    }

    pub fn set_track_solo(&mut self, track: TrackId, solo: bool) -> Result<(), SessionError> {
        self.submit(Command::SetTrackSolo {
            track_id: track,
            solo,
        })
        .map(|_| ())
    }

    fn audio_track(&self, track: TrackId) -> Result<&TrackSummary, SessionError> {
        self.requests
            .snapshot
            .as_ref()
            .and_then(|s| s.tracks.iter().find(|t| t.id == track))
            .filter(|t| t.kind == TrackKind::Audio)
            .ok_or(SessionError::Invalid("Choose an audio track."))
    }

    /// A volume drag: heard live and shown in the view, never in history.
    pub fn preview_track_gain(&mut self, track: TrackId, cdb: i32) -> Result<(), SessionError> {
        self.audio_track(track)?;
        if !(MIN_GAIN_CDB..=MAX_GAIN_CDB).contains(&cdb) {
            return Err(SessionError::Invalid("Track gain must be -60 to +6 dB."));
        }
        self.gain_preview = Some((track, cdb));
        self.gain_commit = None;
        self.push_mix();
        self.dirty = true;
        Ok(())
    }

    /// Ends a volume drag with one undoable change. The preview stays in force
    /// until the owner's snapshot confirms the new value, so nothing jumps.
    pub fn commit_track_gain(&mut self, track: TrackId, cdb: i32) -> Result<(), SessionError> {
        let stored = self.audio_track(track)?.controls.gain_cdb;
        if !(MIN_GAIN_CDB..=MAX_GAIN_CDB).contains(&cdb) {
            return Err(SessionError::Invalid("Track gain must be -60 to +6 dB."));
        }
        if stored == cdb {
            return self.cancel_track_gain();
        }
        self.gain_preview = Some((track, cdb));
        self.gain_commit = Some(self.submit(Command::SetTrackGain {
            track_id: track,
            gain_cdb: cdb,
        })?);
        self.push_mix();
        Ok(())
    }

    /// Abandons a volume drag: the stored value is heard and shown again.
    pub fn cancel_track_gain(&mut self) -> Result<(), SessionError> {
        self.gain_preview = None;
        self.gain_commit = None;
        self.push_mix();
        self.dirty = true;
        Ok(())
    }

    /// Hands the current controls (with any preview) to the mixer.
    fn push_mix(&self) {
        let Some(snapshot) = self.requests.snapshot.as_ref() else {
            return;
        };
        let tracks: Vec<(TrackId, TrackControls)> = snapshot
            .tracks
            .iter()
            .filter(|t| t.kind == TrackKind::Audio)
            .map(|t| {
                let mut controls = t.controls;
                if let Some((_, cdb)) = self.gain_preview.filter(|(id, _)| *id == t.id) {
                    controls.gain_cdb = cdb;
                }
                (t.id, controls)
            })
            .collect();
        self.timeline_playback.set_mix(&tracks);
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
                ..Default::default()
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
            stream: cutcap_core::ClipStream::Video,
            link: None,
            id: clip.id,
            media_id: clip.media_id,
            track_id: clip.track_id,
            start: clip.start,
            duration: clip.duration,
            source_in: clip.source_in,
        };
        let can_unlink = clip.link.is_some();
        let link_candidate = self.link_candidate.map(|c| c.to_string());
        match cutcap_core::edit::split_offset(&core, self.timeline_playhead(), snapshot.timebase) {
            Ok(_) => view::EditingView {
                split_enabled: true,
                split_detail: "Split the selected clip at the playhead (Ctrl+B)".into(),
                notice,
                can_unlink,
                link_candidate,
            },
            Err(reason) => view::EditingView {
                split_enabled: false,
                split_detail: reason.to_string(),
                notice,
                can_unlink,
                link_candidate,
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
        // The current source again only brings its inspector back: reloading
        // the monitor would lose its position.
        let current = self.requests.desired_media == Some(id)
            && self
                .requests
                .snapshot
                .as_ref()
                .is_some_and(|snapshot| snapshot.source_media == Some(id));
        self.submit(Command::SelectMedia { media_id: id })?;
        self.requests.desired_media = Some(id);
        self.requests.inspecting_media = true;
        if !current {
            self.media.pending_selection();
        }
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
    /// Phase 5A: accepts an export. Acceptance is not completion: the job
    /// reports every later step in the view. `track` of `None` means no
    /// video track. Works from a captured plan, so editing may continue.
    pub fn start_export(
        &mut self,
        output: PathBuf,
        track: Option<TrackId>,
        allow_overwrite: bool,
        acknowledge_omitted: bool,
    ) -> Result<(), SessionError> {
        if self.closed {
            return Err(SessionError::Invalid("The session is shut down."));
        }
        if self.export.running() || self.export_pending.is_some() {
            return Err(SessionError::Invalid("An export is already running."));
        }
        let Some(project) = self.current_project() else {
            return Err(SessionError::Invalid(
                "The project is not ready yet. Try again in a moment.",
            ));
        };
        let request_id = RequestId::new();
        self.bridge.submit(CommandEnvelope {
            request_id,
            project_id: Some(project),
            command: Command::PrepareExport {
                video_track: track,
                acknowledge_omitted,
            },
        })?;
        let job = self.export.announce(&output);
        self.export_pending = Some(PendingExport {
            request_id,
            job,
            output: crate::export::OutputRequest {
                path: output,
                allow_overwrite,
            },
        });
        self.dirty = true;
        Ok(())
    }

    /// Asks the named export job to stop. Never waits for it.
    pub fn cancel_export(&mut self, job: &str) -> Result<(), SessionError> {
        self.export.cancel(job).map_err(SessionError::Invalid)?;
        self.dirty = true;
        Ok(())
    }

    fn receive_export_plan(
        &mut self,
        request_id: RequestId,
        result: Result<crate::export::ExportPlan, EngineError>,
    ) {
        let Some(pending) = self
            .export_pending
            .take_if(|pending| pending.request_id == request_id)
        else {
            return;
        };
        match result {
            Ok(plan) => {
                if let Err(error) = self.export.begin(&pending.job, plan, pending.output) {
                    self.export.refuse(&pending.job, error.to_string());
                }
            }
            Err(error) => self.export.refuse(&pending.job, error.message),
        }
    }

    pub fn pump(&mut self) -> bool {
        if self.export.take_changed() {
            self.dirty = true;
        }
        // Playback has priority: thumbnails and waveforms take smaller batches.
        let playing = self.media.playback.active() || self.timeline_playback.active();
        self.thumbnails.set_throttled(playing);
        self.waveforms.set_throttled(playing);
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
                HostEvent::ExportPlan { request_id, result } => {
                    self.receive_export_plan(request_id, *result);
                }
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
        // Thumbnail progress is reported in the view: republish when it moves.
        let stats = self.thumbnails.stats();
        if stats != self.thumbnail_stats_seen {
            self.thumbnail_stats_seen = stats;
            self.dirty = true;
        }
        let stats = self.waveforms.stats();
        if stats != self.waveform_stats_seen {
            self.waveform_stats_seen = stats;
            self.dirty = true;
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
                if self.gain_commit == Some(request_id) {
                    self.gain_commit = None;
                    self.gain_preview = None;
                    self.push_mix();
                }
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
                if self.gain_commit == Some(request_id) {
                    self.gain_commit = None;
                    self.gain_preview = None;
                    self.push_mix();
                }
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
        self.export_summary = crate::export::summarize(&snapshot);
        if self.project_id() != Some(snapshot.id) {
            // Old asynchronous thumbnails must never reach a replacement project.
            self.thumbnails.clear();
            self.thumbnail_slot = None;
            self.waveforms.clear();
            self.waveform_slot = None;
            self.gain_preview = None;
            self.gain_commit = None;
        }
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
        self.link_candidate = match snapshot.selection {
            SelectionState::Clip(id) => self
                .clip_lookup
                .get(&id)
                .and_then(|i| snapshot.clips.get(*i))
                .and_then(|clip| crate::edits::link_candidate(&snapshot.clips, clip))
                .map(|c| c.id),
            _ => None,
        };
        self.requests.snapshot = Some(snapshot);
        self.timeline_generation += 1;
        self.timeline_cached = None;
        self.push_mix();
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
                badge: "",
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
                    let family = match clip.stream {
                        ClipStream::Video => "Video",
                        ClipStream::Audio { .. } => "Audio",
                    };
                    InspectorView {
                        kind: "clip",
                        badge: family,
                        title: view::clip_name(snapshot, clip.media_id),
                        detail: format!("{family} clip on {track_name}"),
                        sections: view::clip_sections(snapshot, clip),
                    }
                } else if let Some(asset) = source {
                    let audio_only = asset.metadata.is_audio_only();
                    InspectorView {
                        kind: "source",
                        badge: if audio_only { "Audio" } else { "Video" },
                        title: view::file_name(asset),
                        detail: if audio_only {
                            "Source audio, read only".into()
                        } else {
                            "Source video, read only".into()
                        },
                        sections: view::source_sections(asset),
                    }
                } else if let Some(track) = track {
                    InspectorView {
                        kind: "track",
                        badge: "",
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
                        badge: "",
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
            tracks: snapshot
                .map(|s| view::track_views(s, self.gain_preview))
                .unwrap_or_default(),
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
            thumbnails: self.thumbnail_stats_seen.clone(),
            waveforms: self.waveform_stats_seen.clone(),
            export: self.export.view(&self.export_summary),
            audio: self.timeline_playback.audio_view(),
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

    /// Replaces the filmstrip demand (visible samples first). Each want is
    /// checked against the current verified source: an unknown or unavailable
    /// media, or a stale source key, is dropped; a sample outside the source's
    /// range completes immediately as no-frame. Accepted means queued.
    pub fn request_thumbnails(&mut self, json: &str) -> Result<(), SessionError> {
        let wants: Vec<ThumbnailWant> = serde_json::from_str(json).map_err(|_| {
            SessionError::Invalid("Thumbnail requests must be a JSON array of wants.")
        })?;
        if wants.len() > crate::thumbnails::MAX_DEMAND {
            return Err(SessionError::Invalid(
                "Too many thumbnail requests at once.",
            ));
        }
        let Some(snapshot) = self.requests.snapshot.as_ref() else {
            return Ok(());
        };
        let micro = Timebase::new(1, 1_000_000).expect("constant");
        let mut jobs = Vec::with_capacity(wants.len());
        for want in wants {
            let Some(media) = snapshot.media.iter().find(|m| m.id == want.media_id) else {
                continue;
            };
            if view::source_key(snapshot, media) != want.source_key || want.source_key.is_empty() {
                continue;
            }
            let Some(meta) = media.metadata.video() else {
                continue;
            };
            let Ok(position) = (SourceTime {
                ticks: want.sample_us,
                timebase: micro,
            })
            .rescale(meta.timebase, Rounding::Ceil) else {
                self.thumbnails.complete_now(
                    want.token,
                    want.source_key,
                    crate::thumbnails::Status::Failed,
                );
                continue;
            };
            let position = SourceTime {
                ticks: position.ticks.max(meta.start_pts),
                timebase: meta.timebase,
            };
            let past_end = meta
                .duration_ticks
                .and_then(|d| meta.start_pts.checked_add(d))
                .is_some_and(|end| position.ticks >= end);
            if past_end {
                self.thumbnails.complete_now(
                    want.token,
                    want.source_key,
                    crate::thumbnails::Status::NoFrame,
                );
                continue;
            }
            if cutcap_media::validate_position(meta, position).is_err() {
                self.thumbnails.complete_now(
                    want.token,
                    want.source_key,
                    crate::thumbnails::Status::Failed,
                );
                continue;
            }
            jobs.push(crate::thumbnails::Job {
                token: want.token,
                media: media.clone(),
                source_key: want.source_key,
                position,
                height: want.height.clamp(
                    cutcap_media::MIN_THUMBNAIL_HEIGHT,
                    cutcap_media::MAX_THUMBNAIL_HEIGHT,
                ),
            });
        }
        self.thumbnails.set_demand(jobs);
        Ok(())
    }

    /// Announces the next completed thumbnail that still belongs to a current
    /// verified source (stale ones are discarded here), keeping it for `copy`.
    pub fn next_thumbnail(&mut self) -> Option<&crate::thumbnails::Done> {
        let current: HashSet<String> = self
            .requests
            .snapshot
            .as_ref()
            .map(|s| {
                s.media
                    .iter()
                    .map(|m| view::source_key(s, m))
                    .filter(|k| !k.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        self.thumbnail_slot = None;
        while let Some(done) = self.thumbnails.take() {
            if current.contains(&done.source_key) {
                self.thumbnail_slot = Some(done);
                break;
            }
        }
        self.thumbnail_slot.as_ref()
    }

    /// The announced thumbnail, if `token` names it (not released).
    pub fn announced_thumbnail(&self, token: u64) -> Option<&crate::thumbnails::Done> {
        self.thumbnail_slot.as_ref().filter(|d| d.token == token)
    }

    /// Releases the announced thumbnail for copying (token must match).
    pub fn take_thumbnail(&mut self, token: u64) -> Option<crate::thumbnails::Done> {
        if self
            .thumbnail_slot
            .as_ref()
            .is_some_and(|d| d.token == token)
        {
            self.thumbnail_slot.take()
        } else {
            None
        }
    }

    pub fn thumbnail_stats(&self) -> crate::thumbnails::ThumbnailStats {
        self.thumbnails.stats()
    }

    /// Replaces the waveform demand. Each want must name a current verified
    /// audio stream (`audio_key`); others are dropped. Accepted means queued.
    pub fn request_waveforms(&mut self, json: &str) -> Result<(), SessionError> {
        let wants: Vec<WaveformWant> = serde_json::from_str(json).map_err(|_| {
            SessionError::Invalid("Waveform requests must be a JSON array of wants.")
        })?;
        if wants.len() > crate::waveforms::MAX_DEMAND {
            return Err(SessionError::Invalid("Too many waveform requests at once."));
        }
        let Some(snapshot) = self.requests.snapshot.as_ref() else {
            return Ok(());
        };
        let mut jobs = Vec::with_capacity(wants.len());
        for want in wants {
            let Some(media) = snapshot.media.iter().find(|m| m.id == want.media_id) else {
                continue;
            };
            let key = view::audio_key(snapshot, media, want.stream);
            let Some(audio) = media.metadata.audio_stream(want.stream) else {
                continue;
            };
            let limit =
                i64::from(audio.sample_rate) * MAX_MEDIA_SECONDS / cutcap_media::PEAK_CHUNK_FRAMES;
            if key.is_empty() || key != want.audio_key || !(0..=limit).contains(&want.chunk) {
                continue;
            }
            jobs.push(crate::waveforms::Job {
                token: want.token,
                media: media.clone(),
                audio_key: want.audio_key,
                stream: want.stream,
                chunk: want.chunk,
            });
        }
        self.waveforms.set_demand(jobs);
        Ok(())
    }

    /// Announces the next completed waveform chunk whose audio key is still
    /// current (stale ones are discarded here), keeping it for `copy`.
    pub fn next_waveform(&mut self) -> Option<&crate::waveforms::Done> {
        let current: HashSet<String> = self
            .requests
            .snapshot
            .as_ref()
            .map(|s| {
                s.media
                    .iter()
                    .flat_map(|m| {
                        m.metadata
                            .audio()
                            .iter()
                            .map(|a| view::audio_key(s, m, a.stream_index))
                            .collect::<Vec<_>>()
                    })
                    .filter(|k| !k.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        self.waveform_slot = None;
        while let Some(done) = self.waveforms.take() {
            if current.contains(&done.audio_key) {
                self.waveform_slot = Some(done);
                break;
            }
        }
        self.waveform_slot.as_ref()
    }

    pub fn announced_waveform(&self, token: u64) -> Option<&crate::waveforms::Done> {
        self.waveform_slot.as_ref().filter(|d| d.token == token)
    }

    pub fn take_waveform(&mut self, token: u64) -> Option<crate::waveforms::Done> {
        if self
            .waveform_slot
            .as_ref()
            .is_some_and(|d| d.token == token)
        {
            self.waveform_slot.take()
        } else {
            None
        }
    }

    /// Stops media work, then the command worker. Idempotent.
    pub fn shutdown(&mut self) -> Result<(), SessionError> {
        if self.closed {
            return Ok(());
        }
        self.closed = true;
        self.export.shutdown();
        self.thumbnails.shutdown();
        self.waveforms.shutdown();
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
