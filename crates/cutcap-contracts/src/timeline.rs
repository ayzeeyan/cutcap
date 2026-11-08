use crate::*;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreviewMode {
    #[default]
    Source,
    Timeline,
}

/// Which stream of the clip's media a clip presents (schema 2). A source's
/// video stream is the one its probe selected; audio names its stream.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ClipStream {
    #[default]
    Video,
    Audio {
        index: u32,
    },
}

impl ClipStream {
    pub const fn track_kind(self) -> TrackKind {
        match self {
            Self::Video => TrackKind::Video,
            Self::Audio { .. } => TrackKind::Audio,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TimelineSeek {
    Ticks(TimelineTime),
    Milliseconds(i64),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MonitoredTrack {
    pub id: TrackId,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineEvaluation {
    pub project_id: ProjectId,
    pub generation: RequestId,
    pub document_revision: u64,
    pub position: TimelineTime,
    pub timebase: Timebase,
    pub track: Option<MonitoredTrack>,
    pub extent: TimelineTime,
    pub target: TimelineTarget,
    pub evaluation_us: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TimelineTarget {
    Blank,
    Frame {
        clip: ClipSummary,
        media: Box<MediaSummary>,
        requested: SourceTime,
        end_exclusive: SourceTime,
    },
    Unavailable {
        clip: ClipSummary,
        path: PathBuf,
        availability: Availability,
        detail: String,
    },
    NoFrame {
        clip: ClipSummary,
        media: Box<MediaSummary>,
        requested: SourceTime,
        end_exclusive: SourceTime,
        detail: String,
    },
}
