use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::{ClipId, MediaId, MediaSummary, ProjectId, RequestId, Timebase, TimelineTime, TrackId};

pub const CONTRACT_VERSION: u32 = 2;

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
    AddTrack {
        kind: TrackKind,
    },
    Select {
        selection: SelectionState,
    },
    /// Only a validated probe result can be committed by the project owner.
    RegisterMedia {
        media: Box<MediaSummary>,
    },
    SelectMedia {
        media_id: MediaId,
    },
    // Reserved request shapes. Currently Unsupported, with no side effects.
    OpenProject {
        path: PathBuf,
    },
    ImportMedia {
        paths: Vec<PathBuf>,
    },
    RemoveClip {
        clip_id: ClipId,
    },
    Seek {
        position: TimelineTime,
    },
    SetPlayback {
        playing: bool,
    },
    Undo,
    Redo,
    StartExport {
        destination: PathBuf,
    },
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
}

/// Small view DTO, never the mutable project or a decoded frame buffer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectSnapshot {
    pub id: ProjectId,
    pub name: String,
    pub timebase: Timebase,
    pub tracks: Vec<TrackSummary>,
    pub media_count: usize,
    pub clip_count: usize,
    pub selection: SelectionState,
    pub media: Vec<MediaSummary>,
    /// Source viewer selection is independent of timeline selection/playhead.
    pub source_media: Option<MediaId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum EngineEvent {
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
