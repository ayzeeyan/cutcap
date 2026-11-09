use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::{
    ClipId, ClipStream, DocumentStatus, LinkId, MediaAvailability, MediaId, MediaSummary,
    ProjectId, RequestId, SourceFingerprint, SourceTime, Timebase, TimelineDuration, TimelineTime,
    TrackControls, TrackId, VerifiedMedia,
};

pub const CONTRACT_VERSION: u32 = 11;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrackKind {
    Video,
    Audio,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SelectionState {
    #[default]
    None,
    Track(TrackId),
    Clip(ClipId),
}

/// One request, one terminal Completed/Failed event. No fire-and-forget writes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandEnvelope {
    pub request_id: RequestId,
    /// Required for every command except NewProject; rejects stale UI actions.
    pub project_id: Option<ProjectId>,
    pub command: Command,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum Command {
    NewProject {
        name: String,
    },
    GetSnapshot,
    /// Ephemeral evaluation: responds without cloning/serializing a project snapshot.
    EvaluateTimeline {
        position: crate::TimelineSeek,
        track_id: Option<TrackId>,
    },
    /// Private host response carries a shared read-only core index, never pixels.
    /// Phase 5A: captures an immutable export plan. Changes nothing, and never
    /// goes through the edit path. `acknowledge_omitted`: the user knows other
    /// occupied video tracks will not be rendered.
    PrepareExport {
        video_track: Option<TrackId>,
        acknowledge_omitted: bool,
    },
    PrepareTimelinePlayback {
        position: crate::TimelineSeek,
    },
    AddTrack {
        kind: TrackKind,
    },
    Select {
        selection: SelectionState,
    },
    /// Only a validated probe result can be committed by the project owner.
    RegisterMedia {
        media: Box<MediaSummary>,
        fingerprint: Option<SourceFingerprint>,
    },
    SelectMedia {
        media_id: MediaId,
    },
    // Reserved request shapes. Currently Unsupported, with no side effects.
    OpenProject {
        path: PathBuf,
    },
    SaveProject {
        path: Option<PathBuf>,
    },
    /// Internal result from the single cancellable probe. Relink commits only
    /// after matching the original content and saved metadata.
    VerifyMedia {
        media_id: MediaId,
        result: Result<Box<VerifiedMedia>, EngineError>,
        relink: bool,
    },
    ImportMedia {
        paths: Vec<PathBuf>,
    },
    /// Appends a registered, ready video source to the end of a video track,
    /// using the source's full reported duration. Undoable.
    InsertClip {
        media_id: MediaId,
        track_id: TrackId,
    },
    /// Removes a clip; its media asset and source file are untouched. Undoable.
    RemoveClip {
        clip_id: ClipId,
    },
    /// Places the whole Ready video source on a video track at `position`
    /// (drag-and-drop). Rejected, never shifted, when it would overlap.
    PlaceClip {
        media_id: MediaId,
        track_id: TrackId,
        position: crate::TimelineSeek,
    },
    /// Places one chosen audio stream of a Ready source as an independent clip
    /// at `position`, on the given audio track or, with none, on the first
    /// audio track free for the whole interval. Undoable.
    PlaceAudioStream {
        media_id: MediaId,
        stream: u32,
        track_id: Option<TrackId>,
        position: crate::TimelineSeek,
    },
    /// Removes the link between the members of a linked pair. Undoable; no
    /// timing changes.
    UnlinkClip {
        clip_id: ClipId,
    },
    /// Links one unlinked video clip and one unlinked audio clip with the same
    /// media, start, duration and source in-point. Undoable.
    LinkClips {
        clip_id: ClipId,
        partner_id: ClipId,
    },
    /// Audio track controls. Each is one undo step and never stops playback.
    SetTrackGain {
        track_id: TrackId,
        gain_cdb: i32,
    },
    SetTrackMute {
        track_id: TrackId,
        mute: bool,
    },
    SetTrackSolo {
        track_id: TrackId,
        solo: bool,
    },
    /// Moves a clip to `position` on a video track, keeping identity, media,
    /// source in-point and duration. `anchor` says which edge `position` names.
    MoveClip {
        clip_id: ClipId,
        track_id: TrackId,
        position: crate::TimelineSeek,
        anchor: ClipEdge,
    },
    /// Moves one boundary of a clip within its source range. A start trim keeps
    /// the retained frames' source mapping by moving source_in by the same
    /// amount, aligned to the source tick grid (see `cutcap_core::edit`).
    TrimClip {
        clip_id: ClipId,
        edge: ClipEdge,
        position: crate::TimelineSeek,
    },
    /// Splits a clip at an interior position aligned to the source tick grid.
    /// The left part keeps the clip identity; the right part is a new clip.
    SplitClip {
        clip_id: ClipId,
        position: crate::TimelineSeek,
    },
    Seek {
        position: TimelineTime,
    },
    SetPlayback {
        playing: bool,
    },
    /// Reverses the newest clip edit. Selection is never an undo step.
    Undo,
    Redo,
    Cancel {
        target: RequestId,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrackSummary {
    pub id: TrackId,
    pub name: String,
    pub kind: TrackKind,
    pub clip_count: usize,
    pub controls: TrackControls,
}

/// One timeline clip as stored in the project (exact ticks, never pixels).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClipSummary {
    pub id: ClipId,
    pub media_id: MediaId,
    pub track_id: TrackId,
    pub start: TimelineTime,
    pub duration: TimelineDuration,
    pub source_in: SourceTime,
    pub stream: ClipStream,
    pub link: Option<LinkId>,
}

/// Which boundary of a clip a move anchor or trim refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClipEdge {
    Start,
    End,
}

/// Editing history availability. Labels describe the edit that would be undone/redone.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HistoryStatus {
    pub can_undo: bool,
    pub can_redo: bool,
    pub undo: Option<String>,
    pub redo: Option<String>,
    pub depth: usize,
    /// The next undo/redo only changes track controls, so playback continues.
    pub undo_track: bool,
    pub redo_track: bool,
}

/// Small view DTO, never the mutable project or a decoded frame buffer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectSnapshot {
    pub id: ProjectId,
    /// Transient identity, refreshed independently of undoable document revisions.
    pub evaluation_generation: RequestId,
    pub name: String,
    pub timebase: Timebase,
    pub tracks: Vec<TrackSummary>,
    pub media_count: usize,
    pub clip_count: usize,
    pub selection: SelectionState,
    pub media: Vec<MediaSummary>,
    /// Source viewer selection is independent of timeline selection/playhead.
    pub source_media: Option<MediaId>,
    pub media_state: Vec<MediaAvailability>,
    pub document: DocumentStatus,
    /// Clips in project order.
    pub clips: Vec<ClipSummary>,
    pub history: HistoryStatus,
    /// How the last successful edit adjusted the requested position (source
    /// tick alignment), when it had to. Cleared by the next command.
    pub edit_notice: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum EngineEvent {
    TimelineEvaluated {
        request_id: RequestId,
        result: Result<Box<crate::TimelineEvaluation>, EngineError>,
    },
    /// Nonterminal status update; accepted file operations remain pending.
    Updated { snapshot: ProjectSnapshot },
    FileFailed {
        request_id: RequestId,
        error: EngineError,
        snapshot: Option<ProjectSnapshot>,
    },
    Completed {
        request_id: RequestId,
        snapshot: ProjectSnapshot,
    },
    Failed {
        request_id: RequestId,
        error: EngineError,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ErrorCode {
    Unsupported,
    InvalidInput,
    NotFound,
    StaleProject,
    Busy,
    Internal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[error("{message}")]
#[serde(deny_unknown_fields)]
pub struct EngineError {
    pub code: ErrorCode,
    pub message: String,
}

impl EngineError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_are_serializable_without_engine_or_ui_types() {
        let envelope = CommandEnvelope {
            request_id: RequestId::new(),
            project_id: Some(ProjectId::new()),
            command: Command::Seek {
                position: TimelineTime(24_024),
            },
        };
        let json = serde_json::to_string(&envelope).unwrap();
        assert_eq!(
            serde_json::from_str::<CommandEnvelope>(&json).unwrap(),
            envelope
        );
    }
}
