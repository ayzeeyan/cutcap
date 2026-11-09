use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
};

use cutcap_contracts::{
    ClipId, LinkId, MediaId, ProjectId, SelectionState, SourceTime, TimeError, Timebase,
    TimelineDuration, TimelineTime, TrackControls, TrackId, TrackKind,
};
use serde::{Deserialize, Serialize};

/// Schema 2 (Phase 4B) adds an explicit stream per clip and linked
/// audio/video pairs. Schema 3 (Phase 4C) adds persistent audio track
/// controls. Older documents are migrated on load (every schema 1 clip is a
/// video clip; older tracks have default controls); nothing is written back
/// unless the user saves.
pub const PROJECT_SCHEMA_VERSION: u32 = 3;
/// The oldest schema this build migrates.
pub const OLDEST_SCHEMA_VERSION: u32 = 1;
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
    /// Volume, mute and solo of an audio track. Absent means defaults; video
    /// tracks always hold defaults.
    #[serde(default, skip_serializing_if = "TrackControls::is_default")]
    pub controls: TrackControls,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MediaAsset {
    pub id: MediaId,
    /// A reference only. No method in this crate opens or modifies the file.
    pub path: PathBuf,
    pub kind: TrackKind,
}

pub use cutcap_contracts::ClipStream;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Clip {
    pub id: ClipId,
    pub media_id: MediaId,
    pub track_id: TrackId,
    pub start: TimelineTime,
    pub duration: TimelineDuration,
    /// The exact instant on the file's container clock at `start`; `timebase`
    /// is only its representation. Linked clips share the same value, so both
    /// streams map a timeline position with one rounding. Unlink keeps it
    /// verbatim; independently placed audio uses `1/sample_rate`.
    pub source_in: SourceTime,
    /// Schema 1 documents have no field: every schema 1 clip is video.
    #[serde(default)]
    pub stream: ClipStream,
    /// Linked audio/video pair identity: both members share start, duration,
    /// media and `source_in`, and every edit changes them together.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<LinkId>,
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
        let controls_are_new = |tracks: &[Track]| {
            if tracks.iter().any(|t| !t.controls.is_default()) {
                Err(ProjectError::Migration(
                    "documents before schema 3 cannot contain track controls",
                ))
            } else {
                Ok(())
            }
        };
        let schema_version = match value.schema_version {
            PROJECT_SCHEMA_VERSION => PROJECT_SCHEMA_VERSION,
            2 => {
                controls_are_new(&value.tracks)?;
                PROJECT_SCHEMA_VERSION
            }
            1 => {
                controls_are_new(&value.tracks)?;
                // Schema 1 has no stream identity: its clips are video clips on
                // video tracks. Anything else cannot be migrated faithfully.
                let audio: HashSet<TrackId> = value
                    .tracks
                    .iter()
                    .filter(|t| t.kind == TrackKind::Audio)
                    .map(|t| t.id)
                    .collect();
                if value.clips.iter().any(|c| audio.contains(&c.track_id)) {
                    return Err(ProjectError::Migration(
                        "a schema 1 clip on an audio track has no stream identity",
                    ));
                }
                if value
                    .clips
                    .iter()
                    .any(|c| c.stream != ClipStream::Video || c.link.is_some())
                {
                    return Err(ProjectError::Migration(
                        "schema 1 documents cannot contain stream or link fields",
                    ));
                }
                PROJECT_SCHEMA_VERSION
            }
            other => return Err(ProjectError::Schema(other)),
        };
        let project = Self {
            schema_version,
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
        validate_controls(&track)?;
        if self.tracks.iter().any(|item| item.id == track.id) {
            return Err(ProjectError::DuplicateId);
        }
        self.tracks.push(track);
        Ok(())
    }

    /// Replaces an audio track's controls, returning the previous ones.
    pub fn set_track_controls(
        &mut self,
        id: TrackId,
        controls: TrackControls,
    ) -> Result<TrackControls, ProjectError> {
        if !controls.gain_in_range() {
            return Err(ProjectError::InvalidControls);
        }
        let track = self
            .tracks
            .iter_mut()
            .find(|t| t.id == id)
            .ok_or(ProjectError::MissingReference)?;
        if track.kind != TrackKind::Audio {
            return Err(ProjectError::InvalidControls);
        }
        Ok(std::mem::replace(&mut track.controls, controls))
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

    /// Inserts a validated clip at `index` in the clip order (clamped to the end).
    /// Used to restore an exact earlier state (undo of a removal); validation
    /// is identical to [`Project::insert_clip`].
    pub fn insert_clip_at(&mut self, index: usize, clip: Clip) -> Result<(), ProjectError> {
        self.insert_clip(clip)?;
        let last = self.clips.len() - 1;
        let index = index.min(last);
        self.clips[index..].rotate_right(1);
        Ok(())
    }

    /// Removes a clip, returning its former position and value. The media asset
    /// and the source file are untouched; nothing else references a clip.
    pub fn remove_clip(&mut self, id: ClipId) -> Result<(usize, Clip), ProjectError> {
        let index = self
            .clips
            .iter()
            .position(|clip| clip.id == id)
            .ok_or(ProjectError::MissingReference)?;
        Ok((index, self.clips.remove(index)))
    }

    pub fn clip(&self, id: ClipId) -> Option<&Clip> {
        self.clips.iter().find(|clip| clip.id == id)
    }

    /// The other member of a linked pair, if `id` is linked.
    pub fn linked_partner(&self, id: ClipId) -> Option<&Clip> {
        let link = self.clip(id)?.link?;
        self.clips
            .iter()
            .find(|c| c.link == Some(link) && c.id != id)
    }

    /// Checks the named link groups: each has exactly one video and one audio
    /// clip with the same media, start, duration and source in-point.
    pub fn validate_links(
        &self,
        links: impl IntoIterator<Item = LinkId>,
    ) -> Result<(), ProjectError> {
        let wanted: HashSet<LinkId> = links.into_iter().collect();
        if wanted.is_empty() {
            return Ok(());
        }
        let mut groups: HashMap<LinkId, Vec<&Clip>> = HashMap::with_capacity(wanted.len());
        for clip in &self.clips {
            if let Some(link) = clip.link.filter(|l| wanted.contains(l)) {
                groups.entry(link).or_default().push(clip);
            }
        }
        for link in &wanted {
            let Some([a, b]) = groups.get(link).map(Vec::as_slice) else {
                return Err(ProjectError::InvalidLink);
            };
            let kinds = (a.stream.track_kind(), b.stream.track_kind());
            if !matches!(
                kinds,
                (TrackKind::Video, TrackKind::Audio) | (TrackKind::Audio, TrackKind::Video)
            ) || a.media_id != b.media_id
                || a.start != b.start
                || a.duration != b.duration
                || a.source_in != b.source_in
            {
                return Err(ProjectError::InvalidLink);
            }
        }
        Ok(())
    }

    /// Timeline position just after the last clip on a track (zero when empty):
    /// the append-to-end insertion point.
    pub fn track_end(&self, track: TrackId) -> Result<TimelineTime, ProjectError> {
        if !self.tracks.iter().any(|item| item.id == track) {
            return Err(ProjectError::MissingReference);
        }
        let mut end = TimelineTime::ZERO;
        for clip in self.clips.iter().filter(|clip| clip.track_id == track) {
            end = end.max(clip.end()?);
        }
        Ok(end)
    }

    /// Change only a source reference, preserving identity, ordering and clips.
    /// The host validates the replacement file; core never touches the filesystem.
    pub fn relink_media(&mut self, id: MediaId, path: PathBuf) -> Result<(), ProjectError> {
        validate_path(&path)?;
        let asset = self
            .media
            .iter_mut()
            .find(|asset| asset.id == id)
            .ok_or(ProjectError::MissingReference)?;
        asset.path = path;
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
            validate_controls(track)?;
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
        let mut links = HashSet::new();
        let mut intervals = Vec::with_capacity(self.clips.len());
        for clip in &self.clips {
            if !clips.insert(clip.id) {
                return Err(ProjectError::DuplicateId);
            }
            links.extend(clip.link);
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
        self.validate_links(links)
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
    // A clip's stream decides its track family. Video clips need a video
    // source; audio clips come from an audiovisual (or audio) source.
    if clip.stream.track_kind() != track_kind {
        return Err(ProjectError::KindMismatch);
    }
    if clip.stream == ClipStream::Video && media_kind != TrackKind::Video {
        return Err(ProjectError::KindMismatch);
    }
    if let ClipStream::Audio { index } = clip.stream
        && index >= 64
    {
        return Err(ProjectError::KindMismatch);
    }
    Ok(())
}

fn validate_controls(track: &Track) -> Result<(), ProjectError> {
    if !track.controls.gain_in_range()
        || (track.kind != TrackKind::Audio && !track.controls.is_default())
    {
        return Err(ProjectError::InvalidControls);
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
    #[error("clip start cannot be negative")]
    NegativeStart,
    #[error("the clip's stream does not match its track or media kind")]
    KindMismatch,
    #[error("clips cannot overlap on the same track")]
    Overlap,
    #[error("project limit reached for {0}")]
    Limit(&'static str),
    #[error(
        "a linked group must be one video and one audio clip with the same media, timing and source in-point"
    )]
    InvalidLink,
    #[error("track gain is outside -60 to +6 dB, or controls are on a video track")]
    InvalidControls,
    #[error("the older project document cannot be migrated: {0}")]
    Migration(&'static str),
    #[error(transparent)]
    Time(#[from] TimeError),
}
