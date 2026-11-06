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
