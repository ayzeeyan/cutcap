//! Exact clip-edit arithmetic shared by the command owner and its tests.
//!
//! A clip maps timeline position `t` to source time
//! `source_in + (t - start) * project_tick` (seconds), rounded up once to the
//! stream's tick clock (`map_clip_source`). `source_in` is an integer number
//! of stream ticks, so a start trim or a split that moves `source_in` by `d`
//! project ticks is exact only when `d * project_tick` is a whole number of
//! stream ticks. Those offsets form a grid: `d` must be a multiple of
//! [`source_step`]. Edits that move `source_in` are aligned to that grid (the
//! requested position is rounded to the nearest grid point, and the owner
//! reports the adjustment); they are never rounded silently per frame, so the
//! retained frames keep the exact mapping they had before the edit.
//!
//! Example: project 1/720000 s, stream 1/15360 s -> one stream tick is 46.875
//! project ticks, the grid step is 375 project ticks (0.52 ms, 8 stream ticks).
use crate::{Clip, Project, ProjectError};
use cutcap_contracts::{ClipId, SourceTime, TimeError, Timebase, TimelineTime};

fn gcd(mut a: i128, mut b: i128) -> i128 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a.abs()
}

/// `stream ticks per project tick` as a reduced fraction `(num, den)`.
fn ratio(project: Timebase, source: Timebase) -> (i128, i128) {
    let num = i128::from(project.numerator()) * i128::from(source.denominator());
    let den = i128::from(project.denominator()) * i128::from(source.numerator());
    let g = gcd(num, den);
    (num / g, den / g)
}

/// The smallest positive project-tick offset that is a whole number of stream
/// ticks. Every start trim/split offset is a multiple of this.
pub fn source_step(project: Timebase, source: Timebase) -> i64 {
    // den <= u32::MAX * u32::MAX / gcd, but realistic clocks keep it tiny; saturate.
    i64::try_from(ratio(project, source).1).unwrap_or(i64::MAX)
}

/// Exact stream-tick offset for a project-tick offset on the grid.
pub fn source_offset(delta: i64, project: Timebase, source: Timebase) -> Result<i64, EditError> {
    let (num, den) = ratio(project, source);
    let delta = i128::from(delta);
    if delta % den != 0 {
        return Err(EditError::OffGrid);
    }
    i64::try_from(delta / den * num).map_err(|_| EditError::Time(TimeError::Overflow))
}

/// Rounds a project-tick offset to the nearest grid multiple (ties away from zero).
pub fn align_to_step(delta: i64, step: i64) -> Result<i64, EditError> {
    if step <= 0 {
        return Err(EditError::Time(TimeError::Overflow));
    }
    let (delta, step) = (i128::from(delta), i128::from(step));
    let q = delta.div_euclid(step);
    let r = delta.rem_euclid(step);
    let q = if r * 2 >= step { q + 1 } else { q };
    i64::try_from(q * step).map_err(|_| EditError::Time(TimeError::Overflow))
}

/// Rounds a project-tick offset up to the next grid multiple. Start trims use
/// this: the new start moves toward the retained part of the clip, so it can
/// never land on a preceding clip or before a snap target.
pub fn align_up(delta: i64, step: i64) -> Result<i64, EditError> {
    if step <= 0 {
        return Err(EditError::Time(TimeError::Overflow));
    }
    let (delta, step) = (i128::from(delta), i128::from(step));
    let q = delta.div_euclid(step) + i128::from(delta.rem_euclid(step) != 0);
    i64::try_from(q * step).map_err(|_| EditError::Time(TimeError::Overflow))
}
/// Source time of the clip's exclusive end, as an exact rational compared in
/// i128: returns `(numerator, denominator)` in stream-tick units.
fn source_end(clip: &Clip, duration: i64, project: Timebase) -> (i128, i128) {
    let (num, den) = ratio(project, clip.source_in.timebase);
    (
        i128::from(clip.source_in.ticks) * den + i128::from(duration) * num,
        den,
    )
}

/// The available range of the clip's original stream, in its own ticks:
/// `[first, end)` where `end` excludes the last tick. `None` end = unknown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceRange {
    pub first: i64,
    pub end: Option<i64>,
}

/// Checks that `[source_in, source_in + duration)` stays inside `range`.
pub fn check_source_range(
    clip: &Clip,
    project: Timebase,
    range: SourceRange,
) -> Result<(), EditError> {
    if clip.source_in.ticks < range.first {
        return Err(EditError::BeforeSourceStart);
    }
    if let Some(end) = range.end {
        let duration = i64::try_from(clip.duration.ticks()).map_err(|_| TimeError::Overflow)?;
        let (n, d) = source_end(clip, duration, project);
        if n > i128::from(end) * d {
            return Err(EditError::AfterSourceEnd);
        }
    }
    Ok(())
}

/// A source in-point moved by an exact grid offset.
pub fn shifted_source(
    source_in: SourceTime,
    delta: i64,
    project: Timebase,
) -> Result<SourceTime, EditError> {
    let offset = source_offset(delta, project, source_in.timebase)?;
    Ok(SourceTime {
        ticks: source_in
            .ticks
            .checked_add(offset)
            .ok_or(TimeError::Overflow)?,
        timebase: source_in.timebase,
    })
}

/// The grid-aligned split offset (project ticks from the clip start) for a cut
/// requested at `position`, or why there is none.
pub fn split_offset(
    clip: &Clip,
    position: TimelineTime,
    project: Timebase,
) -> Result<i64, SplitError> {
    let length = i64::try_from(clip.duration.ticks()).map_err(|_| SplitError::Outside)?;
    let offset = position
        .0
        .checked_sub(clip.start.0)
        .filter(|d| *d > 0 && *d < length)
        .ok_or(SplitError::Outside)?;
    let aligned = align_to_step(offset, source_step(project, clip.source_in.timebase))
        .map_err(|_| SplitError::Outside)?;
    if aligned <= 0 || aligned >= length {
        return Err(SplitError::NearEdge);
    }
    Ok(aligned)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SplitError {
    #[error("Place the playhead inside the clip to split it.")]
    Outside,
    #[error("The split point is too close to the clip edge to fall on a source tick.")]
    NearEdge,
}

/// How far each boundary of `clip` may move within its source range (track
/// neighbours are not considered): the earliest start a start trim may reach
/// (on the source grid, never before timeline zero) and the latest end an end
/// trim may reach. `None` means the limit is unknown, so no extension.
pub fn trim_limits(
    clip: &Clip,
    project: Timebase,
    range: SourceRange,
) -> (TimelineTime, Option<TimelineTime>) {
    let (num, den) = ratio(project, clip.source_in.timebase);
    // Left: k whole grid steps, each `num` stream ticks and `den` project ticks.
    let available = i128::from(clip.source_in.ticks) - i128::from(range.first);
    let k = (available.max(0) / num).min(i128::from(clip.start.0) / den);
    let min_start = i64::try_from(i128::from(clip.start.0) - k * den).unwrap_or(clip.start.0);
    let max_end = range.end.and_then(|end| {
        let ticks = (i128::from(end) - i128::from(clip.source_in.ticks)) * den / num;
        i64::try_from(i128::from(clip.start.0) + ticks)
            .ok()
            .map(TimelineTime)
    });
    (TimelineTime(min_start), max_end)
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EditError {
    #[error("the edit offset is not a whole number of source ticks")]
    OffGrid,
    #[error("the clip would start before the beginning of its source")]
    BeforeSourceStart,
    #[error("the clip would extend past the end of its source")]
    AfterSourceEnd,
    #[error(transparent)]
    Time(#[from] TimeError),
}

/// One atomic clip transaction, as recorded for exact undo/redo. `removed`
/// lists clips in removal order with their index at removal time; `inserted`
/// lists clips in insertion order with their index at insertion time. Applying
/// the inverse in reverse order reproduces the exact previous clip order.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ClipChange {
    pub removed: Vec<(usize, Clip)>,
    pub inserted: Vec<(usize, Clip)>,
}

impl Project {
    /// Removes `remove` (by identity) and inserts `insert` (each at its index,
    /// in order), validating every inserted clip against the resulting
    /// project. All or nothing: on any failure the clip list is restored
    /// exactly and the error returned.
    pub fn replace_clips(
        &mut self,
        remove: &[ClipId],
        insert: &[(usize, Clip)],
    ) -> Result<ClipChange, ProjectError> {
        let mut change = ClipChange::default();
        let result = (|| {
            for id in remove {
                change.removed.push(self.remove_clip(*id)?);
            }
            for (index, clip) in insert {
                self.insert_clip_at(*index, clip.clone())?;
                let at = self
                    .clips()
                    .iter()
                    .position(|c| c.id == clip.id)
                    .ok_or(ProjectError::MissingReference)?;
                change.inserted.push((at, clip.clone()));
            }
            Ok(())
        })();
        match result {
            Ok(()) => Ok(change),
            Err(error) => {
                self.revert(&change);
                Err(error)
            }
        }
    }

    /// Re-applies a recorded change (redo). Atomic like [`Project::replace_clips`].
    pub fn apply_change(&mut self, change: &ClipChange) -> Result<(), ProjectError> {
        let remove: Vec<ClipId> = change.removed.iter().map(|(_, c)| c.id).collect();
        self.replace_clips(&remove, &change.inserted).map(|_| ())
    }

    /// Reverts a recorded change (undo). Atomic like [`Project::replace_clips`].
    pub fn revert_change(&mut self, change: &ClipChange) -> Result<(), ProjectError> {
        let remove: Vec<ClipId> = change.inserted.iter().rev().map(|(_, c)| c.id).collect();
        let insert: Vec<(usize, Clip)> = change.removed.iter().rev().cloned().collect();
        self.replace_clips(&remove, &insert).map(|_| ())
    }

    /// Restores the exact state before a partially applied change. Every step
    /// re-creates a previously valid arrangement, so it cannot fail validation.
    fn revert(&mut self, change: &ClipChange) {
        for (_, clip) in change.inserted.iter().rev() {
            let _ = self.remove_clip(clip.id);
        }
        for (index, clip) in change.removed.iter().rev() {
            let _ = self.insert_clip_at(*index, clip.clone());
        }
    }
}
