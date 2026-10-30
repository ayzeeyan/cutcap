use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
};

use cutcap_contracts::{
    ClipId, MediaId, ProjectId, SelectionState, SourceTime, TimeError, Timebase, TimelineDuration,
    TimelineTime, TrackId, TrackKind,
};
use serde::{Deserialize, Serialize};

pub const PROJECT_SCHEMA_VERSION: u32 = 1;
pub const MAX_TRACKS: usize = 128;
pub const MAX_MEDIA: usize = 10_000;
pub const MAX_CLIPS: usize = 100_000;
pub const MAX_NAME_CHARS: usize = 128;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Track {
    pub id: TrackId,
    pub name: String,
    pub kind: TrackKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MediaAsset {
    pub id: MediaId,
    /// A reference only. No method in this crate opens or modifies the file.
    pub path: PathBuf,
    pub kind: TrackKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Clip {
    pub id: ClipId,
    pub media_id: MediaId,
    pub track_id: TrackId,
    pub start: TimelineTime,
    pub duration: TimelineDuration,
    pub source_in: SourceTime,
}

impl Clip {
    pub fn end(&self) -> Result<TimelineTime, TimeError> {
        self.start.checked_add(self.duration)
    }
}

/// Deserialization validates the complete graph before exposing a Project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "ProjectData")]
pub struct Project {
    schema_version: u32,
    id: ProjectId,
    name: String,
    timebase: Timebase,
    tracks: Vec<Track>,
    media: Vec<MediaAsset>,
    clips: Vec<Clip>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProjectData {
    schema_version: u32,
    id: ProjectId,
    name: String,
    timebase: Timebase,
    tracks: Vec<Track>,
    media: Vec<MediaAsset>,
    clips: Vec<Clip>,
}

impl TryFrom<ProjectData> for Project {
    type Error = ProjectError;
    fn try_from(value: ProjectData) -> Result<Self, Self::Error> {
        let project = Self {
            schema_version: value.schema_version,
            id: value.id,
            name: value.name,
            timebase: value.timebase,
            tracks: value.tracks,
            media: value.media,
            clips: value.clips,
        };
        project.validate()?;
        Ok(project)
    }
}

impl Project {
    pub fn new(name: impl Into<String>, timebase: Timebase) -> Result<Self, ProjectError> {
        let name = name.into();
        validate_name(&name)?;
        Ok(Self {
            schema_version: PROJECT_SCHEMA_VERSION,
            id: ProjectId::new(),
            name,
            timebase,
            tracks: Vec::new(),
            media: Vec::new(),
            clips: Vec::new(),
        })
    }

    pub const fn id(&self) -> ProjectId {
        self.id
    }
    pub fn name(&self) -> &str {
        &self.name
    }
    pub const fn timebase(&self) -> Timebase {
        self.timebase
    }
    pub fn tracks(&self) -> &[Track] {
        &self.tracks
    }
    pub fn media(&self) -> &[MediaAsset] {
        &self.media
    }
    pub fn clips(&self) -> &[Clip] {
        &self.clips
    }

    pub fn add_track(&mut self, track: Track) -> Result<(), ProjectError> {
        if self.tracks.len() >= MAX_TRACKS {
            return Err(ProjectError::Limit("tracks"));
        }
        validate_name(&track.name)?;
        if self.tracks.iter().any(|item| item.id == track.id) {
            return Err(ProjectError::DuplicateId);
        }
        self.tracks.push(track);
        Ok(())
    }

    pub fn add_media(&mut self, asset: MediaAsset) -> Result<(), ProjectError> {
        if self.media.len() >= MAX_MEDIA {
            return Err(ProjectError::Limit("media assets"));
        }
        validate_path(&asset.path)?;
        if self.media.iter().any(|item| item.id == asset.id) {
            return Err(ProjectError::DuplicateId);
        }
        self.media.push(asset);
        Ok(())
    }

    pub fn insert_clip(&mut self, clip: Clip) -> Result<(), ProjectError> {
        if self.clips.len() >= MAX_CLIPS {
            return Err(ProjectError::Limit("clips"));
        }
        if self.clips.iter().any(|item| item.id == clip.id) {
            return Err(ProjectError::DuplicateId);
        }
        self.validate_clip(&clip)?;
        let end = clip.end()?;
        for other in self
            .clips
            .iter()
            .filter(|other| other.track_id == clip.track_id)
        {
            if clip.start < other.end()? && other.start < end {
                return Err(ProjectError::Overlap);
            }
        }
        self.clips.push(clip);
        Ok(())
    }

    pub fn validate_selection(&self, selection: SelectionState) -> Result<(), ProjectError> {
        match selection {
            SelectionState::None => Ok(()),
            SelectionState::Track(id) if self.tracks.iter().any(|track| track.id == id) => Ok(()),
            SelectionState::Clip(id) if self.clips.iter().any(|clip| clip.id == id) => Ok(()),
            _ => Err(ProjectError::MissingReference),
        }
    }

    pub fn validate(&self) -> Result<(), ProjectError> {
        if self.schema_version != PROJECT_SCHEMA_VERSION {
            return Err(ProjectError::Schema(self.schema_version));
        }
        validate_name(&self.name)?;
        if self.tracks.len() > MAX_TRACKS
            || self.media.len() > MAX_MEDIA
            || self.clips.len() > MAX_CLIPS
        {
            return Err(ProjectError::Limit("project collections"));
        }
        let mut tracks = HashMap::with_capacity(self.tracks.len());
        for track in &self.tracks {
            validate_name(&track.name)?;
            if tracks.insert(track.id, track.kind).is_some() {
                return Err(ProjectError::DuplicateId);
            }
        }
        let mut media = HashMap::with_capacity(self.media.len());
        for asset in &self.media {
            validate_path(&asset.path)?;
            if media.insert(asset.id, asset.kind).is_some() {
                return Err(ProjectError::DuplicateId);
            }
        }
        let mut clips = HashSet::new();
        let mut intervals = Vec::with_capacity(self.clips.len());
        for clip in &self.clips {
            if !clips.insert(clip.id) {
                return Err(ProjectError::DuplicateId);
            }
            validate_clip_values(
                clip,
                *tracks
                    .get(&clip.track_id)
                    .ok_or(ProjectError::MissingReference)?,
                *media
                    .get(&clip.media_id)
                    .ok_or(ProjectError::MissingReference)?,
            )?;
            intervals.push((clip.track_id, clip.start, clip.end()?));
        }
        intervals.sort_unstable();
        for pair in intervals.windows(2) {
            if pair[0].0 == pair[1].0 && pair[1].1 < pair[0].2 {
                return Err(ProjectError::Overlap);
            }
        }
        Ok(())
    }

    fn validate_clip(&self, clip: &Clip) -> Result<(), ProjectError> {
        let track = self
            .tracks
            .iter()
            .find(|track| track.id == clip.track_id)
            .ok_or(ProjectError::MissingReference)?;
        let asset = self
            .media
            .iter()
            .find(|asset| asset.id == clip.media_id)
            .ok_or(ProjectError::MissingReference)?;
        validate_clip_values(clip, track.kind, asset.kind)
    }
}

fn validate_clip_values(
    clip: &Clip,
    track_kind: TrackKind,
    media_kind: TrackKind,
) -> Result<(), ProjectError> {
    if clip.start < TimelineTime::ZERO {
        return Err(ProjectError::NegativeStart);
    }
    clip.end()?;
    if track_kind != media_kind {
        return Err(ProjectError::KindMismatch);
    }
    Ok(())
}

fn validate_name(name: &str) -> Result<(), ProjectError> {
    if name.trim().is_empty()
        || name.chars().count() > MAX_NAME_CHARS
        || name.chars().any(char::is_control)
    {
        return Err(ProjectError::InvalidName);
    }
    Ok(())
}

fn validate_path(path: &Path) -> Result<(), ProjectError> {
    if path.as_os_str().is_empty() {
        Err(ProjectError::EmptyPath)
    } else {
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProjectError {
    #[error("project schema {0} is unsupported; migrate explicitly before loading")]
    Schema(u32),
    #[error("names must contain 1–128 visible characters and no control characters")]
    InvalidName,
    #[error("media path is empty")]
    EmptyPath,
    #[error("duplicate stable identifier")]
    DuplicateId,
    #[error("referenced track, media or clip does not exist")]
    MissingReference,
    #[error("clip start cannot be negative in schema 1")]
    NegativeStart,
    #[error("track and media kinds differ")]
    KindMismatch,
    #[error("clips cannot overlap on the same track in schema 1")]
    Overlap,
    #[error("project limit reached for {0}")]
    Limit(&'static str),
    #[error(transparent)]
    Time(#[from] TimeError),
}
