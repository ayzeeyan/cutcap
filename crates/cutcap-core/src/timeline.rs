//! Deterministic, codec/UI-independent single-track lookup and exact source mapping.
use crate::{Clip, Project, Track};
use cutcap_contracts::*;
use std::collections::{BTreeMap, HashMap};

pub struct TimelineIndex {
    project: ProjectId,
    tracks: BTreeMap<TrackId, (Track, Vec<Clip>)>,
    clip_tracks: HashMap<ClipId, TrackId>,
    default: Option<TrackId>,
}
impl TimelineIndex {
    /// Rebuild only after geometry changes; Project already enforces graph invariants.
    pub fn new(project: &Project) -> Self {
        let mut tracks: BTreeMap<_, _> = project
            .tracks()
            .iter()
            .filter(|t| t.kind == TrackKind::Video)
            .map(|t| (t.id, (t.clone(), Vec::new())))
            .collect();
        let mut clip_tracks = HashMap::with_capacity(project.clips().len());
        for clip in project.clips() {
            if let Some((_, clips)) = tracks.get_mut(&clip.track_id) {
                clips.push(clip.clone());
                clip_tracks.insert(clip.id, clip.track_id);
            }
        }
        for (_, clips) in tracks.values_mut() {
            clips.sort_unstable_by_key(|c| c.start);
        }
        Self {
            project: project.id(),
            tracks,
            clip_tracks,
            default: project
                .tracks()
                .iter()
                .find(|t| t.kind == TrackKind::Video)
                .map(|t| t.id),
        }
    }
    pub fn monitor(&self, selection: SelectionState) -> Option<&Track> {
        let id = match selection {
            SelectionState::Track(id) if self.tracks.contains_key(&id) => Some(id),
            SelectionState::Clip(id) => self.clip_tracks.get(&id).copied(),
            _ => None,
        }
        .or(self.default)?;
        self.tracks.get(&id).map(|(track, _)| track)
    }
    pub fn track(&self, id: TrackId) -> Option<&Track> {
        self.tracks.get(&id).map(|(t, _)| t)
    }
    pub fn end(&self, id: TrackId) -> Result<TimelineTime, TimeError> {
        self.tracks
            .get(&id)
            .and_then(|(_, clips)| clips.last())
            .map_or(Ok(TimelineTime::ZERO), Clip::end)
    }
    /// First clip starting strictly after position (also works inside a gap).
    pub fn next(&self, track: TrackId, position: TimelineTime) -> Option<&Clip> {
        let (_, clips) = self.tracks.get(&track)?;
        clips.get(clips.partition_point(|c| c.start <= position))
    }
    pub fn active(
        &self,
        project: ProjectId,
        track: TrackId,
        position: TimelineTime,
    ) -> Result<Option<&Clip>, EvaluationError> {
        if project != self.project {
            return Err(EvaluationError::StaleProject);
        }
        if position.0 < 0 {
            return Err(EvaluationError::NegativePosition);
        }
        let (_, clips) = self
            .tracks
            .get(&track)
            .ok_or(EvaluationError::NotVideoTrack)?;
        let insertion = clips.partition_point(|c| c.start <= position);
        let Some(clip) = insertion.checked_sub(1).and_then(|i| clips.get(i)) else {
            return Ok(None);
        };
        Ok((position < clip.end()?).then_some(clip))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EvaluationError {
    #[error("timeline position must be nonnegative")]
    NegativePosition,
    #[error("timeline evaluation belongs to another project")]
    StaleProject,
    #[error("monitor a video track; audio is not evaluated")]
    NotVideoTrack,
    #[error("position is outside this clip's half-open interval")]
    OutsideClip,
    #[error(transparent)]
    Time(#[from] TimeError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceMapping {
    pub requested: SourceTime,
    pub end_exclusive: SourceTime,
}
impl SourceMapping {
    /// Require original stream ticks; a finer/mismatched clock is not silently rounded.
    pub fn accepts(self, actual: SourceTime) -> Result<bool, TimeError> {
        let floor = actual.rescale(self.requested.timebase, Rounding::Floor)?;
        let ceil = actual.rescale(self.requested.timebase, Rounding::Ceil)?;
        Ok(floor == ceil
            && floor.ticks >= self.requested.ticks
            && floor.ticks < self.end_exclusive.ticks)
    }
}

pub fn map_clip_source(
    clip: &Clip,
    position: TimelineTime,
    project_clock: Timebase,
    source_clock: Timebase,
) -> Result<SourceMapping, EvaluationError> {
    if position < clip.start || position >= clip.end()? {
        return Err(EvaluationError::OutsideClip);
    }
    let delta = position
        .0
        .checked_sub(clip.start.0)
        .ok_or(TimeError::Overflow)?;
    Ok(SourceMapping {
        requested: sum_rescale_ceil(clip.source_in, delta, project_clock, source_clock)?,
        end_exclusive: sum_rescale_ceil(
            clip.source_in,
            i64::try_from(clip.duration.ticks()).map_err(|_| TimeError::Overflow)?,
            project_clock,
            source_clock,
        )?,
    })
}

/// Actual source PTS -> first project tick at which it may be presented.
/// Round the rational difference once; never infer time from a frame rate.
pub fn frame_timeline_time(
    clip: &Clip,
    actual: SourceTime,
    project_clock: Timebase,
) -> Result<TimelineTime, EvaluationError> {
    let bounds = map_clip_source(clip, clip.start, project_clock, actual.timebase)?;
    if !bounds.accepts(actual)? {
        return Err(EvaluationError::OutsideClip);
    }
    let offset = sum_parts_ceil(
        i128::from(actual.ticks),
        actual.timebase,
        -i128::from(clip.source_in.ticks),
        clip.source_in.timebase,
        project_clock,
    )?;
    let position = TimelineTime(
        clip.start
            .0
            .checked_add(offset)
            .ok_or(TimeError::Overflow)?,
    );
    if position >= clip.end()? {
        return Err(EvaluationError::OutsideClip);
    }
    Ok(position)
}

/// Sum rational times before rounding. Splitting quotient/remainder avoids the
/// much larger numerator of a naive common-denominator source+timeline product.
fn sum_rescale_ceil(
    source: SourceTime,
    offset: i64,
    offset_clock: Timebase,
    target: Timebase,
) -> Result<SourceTime, TimeError> {
    Ok(SourceTime {
        ticks: sum_parts_ceil(
            i128::from(source.ticks),
            source.timebase,
            i128::from(offset),
            offset_clock,
            target,
        )?,
        timebase: target,
    })
}

fn sum_parts_ceil(
    first: i128,
    first_clock: Timebase,
    second: i128,
    second_clock: Timebase,
    target: Timebase,
) -> Result<i64, TimeError> {
    fn parts(ticks: i128, from: Timebase, to: Timebase) -> Result<(i128, i128, i128), TimeError> {
        let n = ticks
            .checked_mul(i128::from(from.numerator()))
            .and_then(|n| n.checked_mul(i128::from(to.denominator())))
            .ok_or(TimeError::Overflow)?;
        let d = i128::from(from.denominator()) * i128::from(to.numerator());
        Ok((n.div_euclid(d), n.rem_euclid(d), d))
    }
    fn gcd(mut a: i128, mut b: i128) -> i128 {
        while b != 0 {
            (a, b) = (b, a % b);
        }
        a
    }
    let (a, ar, ad) = parts(first, first_clock, target)?;
    let (b, br, bd) = parts(second, second_clock, target)?;
    let g = gcd(ad, bd);
    let common = (ad / g).checked_mul(bd).ok_or(TimeError::Overflow)?;
    let remainder = ar
        .checked_mul(bd / g)
        .and_then(|n| br.checked_mul(ad / g).and_then(|m| n.checked_add(m)))
        .ok_or(TimeError::Overflow)?;
    let ticks = a
        .checked_add(b)
        .and_then(|n| n.checked_add(remainder / common))
        .and_then(|n| n.checked_add(i128::from(remainder % common != 0)))
        .ok_or(TimeError::Overflow)?;
    i64::try_from(ticks).map_err(|_| TimeError::Overflow)
}
