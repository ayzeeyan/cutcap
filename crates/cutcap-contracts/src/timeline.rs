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

/// Quietest stored track gain: -60.00 dB (a real gain; mute is separate).
pub const MIN_GAIN_CDB: i32 = -6000;
/// Loudest stored track gain: +6.00 dB.
pub const MAX_GAIN_CDB: i32 = 600;

/// Persistent controls of an audio track (schema 3). `gain_cdb` is hundredths
/// of a decibel, an exact integer so documents compare and round-trip exactly.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct TrackControls {
    pub gain_cdb: i32,
    pub mute: bool,
    pub solo: bool,
}

impl TrackControls {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
    pub fn gain_in_range(&self) -> bool {
        (MIN_GAIN_CDB..=MAX_GAIN_CDB).contains(&self.gain_cdb)
    }
    /// Mute wins over solo: a muted, soloed track is silent.
    pub fn audible(&self, any_solo: bool) -> bool {
        !self.mute && (!any_solo || self.solo)
    }
    /// Amplitude gain: `10^(dB / 20)`. Never zero; inaudibility is `audible`.
    pub fn linear_gain(&self) -> f32 {
        10f64.powf(f64::from(self.gain_cdb) / 2000.0) as f32
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
