//! The immutable description of one export, built by the command owner.
//!
//! Everything a job needs is owned here: later edits, New and Open cannot
//! reach it (PHASE_5A_EXPORT_CONTRACT.md §3). It is not a second project
//! model: the clips stay in the engine's own immutable timeline index.
use crate::audio_mix::AudioPlan;
use cutcap_contracts::*;
use cutcap_core::{OutputGrid, TimelineIndex};
use serde::Serialize;
use std::{collections::BTreeMap, path::PathBuf, sync::Arc};

/// One source the export reads, and how it must still look.
#[derive(Debug, Clone)]
pub struct ExpectedSource {
    pub media: MediaSummary,
    pub fingerprint: Option<SourceFingerprint>,
}

pub struct ExportPlan {
    pub project: ProjectId,
    pub project_name: String,
    pub revision: u64,
    pub timebase: Timebase,
    /// The one video track rendered (none: black picture).
    pub video_track: Option<TrackId>,
    pub video_track_name: Option<String>,
    /// Occupied video tracks left out, which the user has acknowledged.
    pub omitted_tracks: Vec<String>,
    pub index: Arc<TimelineIndex>,
    pub sources: BTreeMap<MediaId, ExpectedSource>,
    /// The clips of every audible audio track.
    pub audio: AudioPlan,
    /// Committed controls of every audio track, in project order.
    pub controls: Vec<(TrackId, TrackControls)>,
    pub grid: OutputGrid,
    pub preset: ExportPreset,
}

/// Where the result goes and whether an existing file may be replaced.
#[derive(Debug, Clone)]
pub struct OutputRequest {
    pub path: PathBuf,
    pub allow_overwrite: bool,
}

/// One video track the dialog can offer.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ExportTrackChoice {
    pub id: String,
    pub name: String,
    pub clips: usize,
    pub duration_ms: i64,
    pub duration_text: String,
    pub frames: u64,
    /// Why this choice cannot be exported as things stand.
    pub blocked: Option<String>,
}

/// What Export would do with the project as it is now, per snapshot.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ExportSummary {
    pub preset_name: String,
    pub preset_text: String,
    pub video_tracks: Vec<ExportTrackChoice>,
    /// The offered default: the first video track holding clips, else the first.
    pub default_track: Option<String>,
    /// Audio tracks that will be heard, out of those holding clips.
    pub audio_tracks: usize,
    pub audio_tracks_with_clips: usize,
    pub revision: u64,
}
