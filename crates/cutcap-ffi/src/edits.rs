//! Phase 3E timeline edits: place, move, trim and split. Each builds the exact
//! new clip values, then commits them through one atomic core transaction
//! (`Project::replace_clips`), so a rejected edit changes nothing and a
//! successful one becomes exactly one history entry.
//!
//! Timing policy (see `cutcap_core::edit`): moves and end trims keep
//! `source_in` and accept any project tick. Start trims and splits move
//! `source_in`, so their offset is aligned to the source tick grid (the
//! nearest grid point); the owner reports the adjustment in the snapshot's
//! `edit_notice` when one was needed. Retained frames keep their exact
//! mapping; no rounding accumulates across repeated edits.
use cutcap_contracts::*;
use cutcap_core::edit::{check_source_range, shifted_source, source_step};
use cutcap_core::{Clip, ClipChange, EditError, Project, SourceRange};
use std::collections::BTreeMap;

pub struct Outcome {
    pub change: ClipChange,
    pub label: String,
    pub notice: Option<String>,
    pub selection: SelectionState,
}

fn invalid(message: impl Into<String>) -> EngineError {
    EngineError::new(ErrorCode::InvalidInput, message)
}

fn edit_error(error: EditError) -> EngineError {
    invalid(match error {
        EditError::OffGrid => "That position does not fall on a source tick.".to_owned(),
        EditError::BeforeSourceStart => {
            "The clip cannot start before the first frame of its source.".to_owned()
        }
        EditError::AfterSourceEnd => {
            "The clip cannot extend past the end of its source.".to_owned()
        }
        EditError::Time(e) => format!("Clip timing is out of range: {e}."),
    })
}

fn project_error(error: cutcap_core::ProjectError) -> EngineError {
    use cutcap_core::ProjectError as E;
    invalid(match error {
        E::Overlap => "Clips cannot overlap on the same track.".to_owned(),
        E::KindMismatch => "Video clips can only be placed on a video track.".to_owned(),
        E::NegativeStart => "Clips cannot start before the beginning of the timeline.".to_owned(),
        E::MissingReference => {
            "That clip, track or source is no longer in this project.".to_owned()
        }
        E::Limit(what) => format!("The project limit for {what} is reached."),
        other => other.to_string(),
    })
}

/// Resolves a UI position: exact ticks, or whole milliseconds floored to ticks.
pub fn resolve(project: &Project, position: TimelineSeek) -> Result<TimelineTime, EngineError> {
    let time = match position {
        TimelineSeek::Ticks(time) => time,
        TimelineSeek::Milliseconds(ms) => TimelineTime(
            SourceTime {
                ticks: ms,
                timebase: Timebase::new(1, 1000).expect("constant"),
            }
            .rescale(project.timebase(), Rounding::Floor)
            .map_err(|e| invalid(e.to_string()))?
            .ticks,
        ),
    };
    if time.0 < 0 {
        return Err(invalid(
            "Clips cannot start before the beginning of the timeline.",
        ));
    }
    Ok(time)
}

fn video_track(project: &Project, id: TrackId) -> Result<(), EngineError> {
    match project.tracks().iter().find(|t| t.id == id) {
        None => Err(EngineError::new(
            ErrorCode::NotFound,
            "That track no longer exists.",
        )),
        Some(track) if track.kind != TrackKind::Video => {
            Err(invalid("Video clips can only be placed on a video track."))
        }
        Some(_) => Ok(()),
    }
}

fn find(project: &Project, id: ClipId) -> Result<(usize, Clip), EngineError> {
    project
        .clips()
        .iter()
        .position(|c| c.id == id)
        .map(|i| (i, project.clips()[i].clone()))
        .ok_or_else(|| {
            EngineError::new(
                ErrorCode::NotFound,
                "That clip is no longer in this project.",
            )
        })
}

fn ticks(duration: TimelineDuration) -> Result<i64, EngineError> {
    i64::try_from(duration.ticks()).map_err(|_| invalid("Clip duration is out of range."))
}

fn duration(value: i64) -> Result<TimelineDuration, EngineError> {
    u64::try_from(value)
        .ok()
        .and_then(|v| TimelineDuration::new(v).ok())
        .ok_or_else(|| invalid("A clip must keep at least one timeline tick."))
}

/// The original stream's available range for a clip's media. Unknown metadata
/// (never verified) allows shrinking but no extension past the current range.
pub fn source_range(media: Option<&MediaSummary>, clip: &Clip, project: Timebase) -> SourceRange {
    match media.filter(|m| m.metadata.timebase == clip.source_in.timebase) {
        Some(m) => SourceRange {
            first: m.metadata.start_pts,
            end: m
                .metadata
                .duration_ticks
                .and_then(|d| m.metadata.start_pts.checked_add(d)),
        },
        None => {
            // Current exclusive end, rounded up to whole ticks: shrinking stays valid.
            let end =
                cutcap_core::map_clip_source(clip, clip.start, project, clip.source_in.timebase)
                    .ok()
                    .map(|m| m.end_exclusive.ticks);
            SourceRange {
                first: clip.source_in.ticks,
                end,
            }
        }
    }
}

fn name(media: &BTreeMap<MediaId, MediaSummary>, clip: &Clip) -> String {
    media
        .get(&clip.media_id)
        .map_or_else(|| "clip".to_owned(), crate::view::file_name)
}

fn adjustment(project: Timebase, requested: TimelineTime, applied: TimelineTime) -> Option<String> {
    (requested != applied).then(|| {
        let ns = |t: i64| {
            SourceTime {
                ticks: t,
                timebase: project,
            }
            .rescale(
                Timebase::new(1, 1_000_000_000).expect("constant"),
                Rounding::Floor,
            )
            .map_or(0, |s| s.ticks)
        };
        format!(
            "Aligned to the nearest source tick ({:+.3} ms) so the source frames stay exact.",
            (ns(applied.0) - ns(requested.0)) as f64 / 1e6
        )
    })
}

pub fn apply(
    project: &mut Project,
    command: Command,
    media: &BTreeMap<MediaId, MediaSummary>,
    availability: &BTreeMap<MediaId, MediaAvailability>,
) -> Result<Outcome, EngineError> {
    let tb = project.timebase();
    match command {
        Command::PlaceClip {
            media_id,
            track_id,
            position,
        } => {
            let summary = media
                .get(&media_id)
                .filter(|_| {
                    availability
                        .get(&media_id)
                        .is_some_and(|a| a.state == Availability::Ready)
                })
                .ok_or_else(|| {
                    invalid("Only a verified, ready source can be added to the timeline.")
                })?;
            video_track(project, track_id)?;
            let start = resolve(project, position)?;
            let length = whole_source(summary, tb)?;
            start
                .checked_add(length)
                .map_err(|_| invalid("Clip timing overflows the timeline clock."))?;
            let clip = Clip {
                id: ClipId::new(),
                media_id,
                track_id,
                start,
                duration: length,
                source_in: SourceTime {
                    ticks: summary.metadata.start_pts,
                    timebase: summary.metadata.timebase,
                },
            };
            let id = clip.id;
            let change = project
                .replace_clips(&[], &[(project.clips().len(), clip)])
                .map_err(project_error)?;
            Ok(Outcome {
                change,
                label: format!("add {}", crate::view::file_name(summary)),
                notice: None,
                selection: SelectionState::Clip(id),
            })
        }
        Command::MoveClip {
            clip_id,
            track_id,
            position,
            anchor,
        } => {
            let (index, clip) = find(project, clip_id)?;
            video_track(project, track_id)?;
            let at = resolve(project, position)?;
            let length = ticks(clip.duration)?;
            let start =
                match anchor {
                    ClipEdge::Start => at,
                    ClipEdge::End => {
                        TimelineTime(at.0.checked_sub(length).filter(|s| *s >= 0).ok_or_else(
                            || invalid("Clips cannot start before the beginning of the timeline."),
                        )?)
                    }
                };
            if start == clip.start && track_id == clip.track_id {
                return Err(invalid("The clip is already at that position."));
            }
            start
                .checked_add(clip.duration)
                .map_err(|_| invalid("Clip timing overflows the timeline clock."))?;
            let moved = Clip {
                start,
                track_id,
                ..clip.clone()
            };
            let change = project
                .replace_clips(&[clip_id], &[(index, moved)])
                .map_err(project_error)?;
            Ok(Outcome {
                change,
                label: format!("move {}", name(media, &clip)),
                notice: None,
                selection: SelectionState::Clip(clip_id),
            })
        }
        Command::TrimClip {
            clip_id,
            edge,
            position,
        } => {
            let (index, clip) = find(project, clip_id)?;
            let requested = resolve(project, position)?;
            let length = ticks(clip.duration)?;
            let (trimmed, applied) = match edge {
                ClipEdge::Start => {
                    let step = source_step(tb, clip.source_in.timebase);
                    let delta = requested
                        .0
                        .checked_sub(clip.start.0)
                        .ok_or_else(|| invalid("Clip timing is out of range."))?;
                    let delta = cutcap_core::edit::align_up(delta, step).map_err(edit_error)?;
                    let start = TimelineTime(clip.start.0 + delta);
                    if start.0 < 0 {
                        return Err(invalid(
                            "Clips cannot start before the beginning of the timeline.",
                        ));
                    }
                    let clip = Clip {
                        start,
                        duration: duration(length - delta)?,
                        source_in: shifted_source(clip.source_in, delta, tb).map_err(edit_error)?,
                        ..clip.clone()
                    };
                    (clip, start)
                }
                ClipEdge::End => {
                    let new_length = requested
                        .0
                        .checked_sub(clip.start.0)
                        .ok_or_else(|| invalid("Clip timing is out of range."))?;
                    (
                        Clip {
                            duration: duration(new_length)?,
                            ..clip.clone()
                        },
                        requested,
                    )
                }
            };
            if trimmed == clip {
                return Err(invalid("The clip already ends there."));
            }
            check_source_range(
                &trimmed,
                tb,
                source_range(media.get(&clip.media_id), &clip, tb),
            )
            .map_err(edit_error)?;
            let change = project
                .replace_clips(&[clip_id], &[(index, trimmed)])
                .map_err(project_error)?;
            Ok(Outcome {
                change,
                label: format!("trim {}", name(media, &clip)),
                notice: adjustment(tb, requested, applied),
                selection: SelectionState::Clip(clip_id),
            })
        }
        Command::SplitClip { clip_id, position } => {
            let (index, clip) = find(project, clip_id)?;
            let requested = resolve(project, position)?;
            let length = ticks(clip.duration)?;
            let offset = cutcap_core::edit::split_offset(&clip, requested, tb)
                .map_err(|e| invalid(e.to_string()))?;
            let left = Clip {
                duration: duration(offset)?,
                ..clip.clone()
            };
            let right = Clip {
                id: ClipId::new(),
                start: TimelineTime(clip.start.0 + offset),
                duration: duration(length - offset)?,
                source_in: shifted_source(clip.source_in, offset, tb).map_err(edit_error)?,
                ..clip.clone()
            };
            let applied = right.start;
            let change = project
                .replace_clips(&[clip_id], &[(index, left), (index + 1, right)])
                .map_err(project_error)?;
            Ok(Outcome {
                change,
                label: format!("split {}", name(media, &clip)),
                notice: adjustment(tb, requested, applied),
                selection: SelectionState::Clip(clip_id),
            })
        }
        _ => Err(EngineError::new(ErrorCode::Internal, "Not a clip edit.")),
    }
}

/// The whole source's duration on the project clock (floor), as for append.
pub fn whole_source(
    source: &MediaSummary,
    project: Timebase,
) -> Result<TimelineDuration, EngineError> {
    let ticks = source
        .metadata
        .duration_ticks
        .filter(|t| *t > 0)
        .ok_or_else(|| invalid("This source has no known duration, so it cannot be placed yet."))?;
    let length = SourceTime {
        ticks,
        timebase: source.metadata.timebase,
    }
    .rescale(project, Rounding::Floor)
    .map_err(|_| invalid("Clip timing overflows the timeline clock."))?;
    u64::try_from(length.ticks)
        .ok()
        .and_then(|t| TimelineDuration::new(t).ok())
        .ok_or_else(|| invalid("This source is shorter than one timeline tick."))
}
