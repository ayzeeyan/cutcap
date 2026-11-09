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
//!
//! Linked audio (Phase 4B): a linked pair shares start, duration and the very
//! same `source_in` value (in the video stream's clock), so the existing grid
//! keeps both streams exact: the audio decoder maps that one rational instant
//! to its own samples with a single rounding, exactly as video maps it to
//! frames. Every edit of either member builds both new values and commits
//! them in one transaction; placement adds the source's primary audio stream
//! to the first free audio track or is rejected as a whole.
//!
//! Independent audio (Phase 4C): a clip of an audio-only source or a chosen
//! stream has no partner and uses the stream's sample clock for `source_in`.
//! Unlink only clears the link; every other value stays exact, so playback of
//! the audio is unchanged. See `docs/PHASE_4C_AUDIO_MIXING_CONTRACT.md`.
use cutcap_contracts::*;
use cutcap_core::edit::{check_source_range, shifted_source, source_step};
use cutcap_core::{Clip, ClipChange, ClipStream, EditError, Project, SourceRange};
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
        E::KindMismatch => {
            "Video clips go on video tracks and audio clips on audio tracks.".to_owned()
        }
        E::InvalidLink => "Linked video and audio must change together.".to_owned(),
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
    track_for(project, id, ClipStream::Video)
}

/// The track must exist and belong to the clip's stream family.
fn track_for(project: &Project, id: TrackId, stream: ClipStream) -> Result<(), EngineError> {
    match project.tracks().iter().find(|t| t.id == id) {
        None => Err(EngineError::new(
            ErrorCode::NotFound,
            "That track no longer exists.",
        )),
        Some(track) if track.kind != stream.track_kind() => Err(invalid(match stream {
            ClipStream::Video => "Video clips can only be placed on a video track.",
            ClipStream::Audio { .. } => "Audio clips can only be placed on an audio track.",
        })),
        Some(_) => Ok(()),
    }
}

/// A new video clip plus, when its source has a decodable audio stream, the
/// linked audio clip on the first audio track (project order) that is free
/// for the whole interval. No free audio track rejects the placement: the
/// video is never added while silently dropping its audio.
pub fn with_linked_audio(
    project: &Project,
    summary: &MediaSummary,
    mut video: Clip,
) -> Result<Vec<Clip>, EngineError> {
    let Some(audio) = summary.metadata.primary_audio() else {
        return Ok(vec![video]);
    };
    let end = video
        .end()
        .map_err(|_| invalid("Clip timing overflows the timeline clock."))?;
    let free = |track: TrackId| {
        project
            .clips()
            .iter()
            .filter(|c| c.track_id == track)
            .all(|c| c.end().is_ok_and(|e| e <= video.start) || c.start >= end)
    };
    let track = project
        .tracks()
        .iter()
        .find(|t| t.kind == TrackKind::Audio && free(t.id))
        .ok_or_else(|| {
            invalid(format!(
                "No audio track is free at this position for the audio of {}. \
                 Add an audio track or choose another position.",
                crate::view::file_name(summary)
            ))
        })?;
    let link = Some(LinkId::new());
    video.link = link;
    let audio = Clip {
        id: ClipId::new(),
        track_id: track.id,
        stream: ClipStream::Audio {
            index: audio.stream_index,
        },
        ..video.clone()
    };
    Ok(vec![video, audio])
}

/// The clip and, if linked, its partner, each with its index in clip order.
fn group(project: &Project, id: ClipId) -> Result<Vec<(usize, Clip)>, EngineError> {
    let mut members = vec![find(project, id)?];
    if let Some(partner) = project.linked_partner(id) {
        members.push(find(project, partner.id)?);
    }
    Ok(members)
}

/// Replaces each listed clip by its new values (zero, one or two clips each)
/// in one atomic transaction, keeping each replacement at its original place
/// in clip order. Undo/redo use the recorded change, so the exact order of
/// insertion only affects presentation, never correctness.
pub fn replace_group(
    project: &mut Project,
    mut replacements: Vec<(usize, ClipId, Vec<Clip>)>,
) -> Result<ClipChange, EngineError> {
    replacements.sort_by_key(|(index, _, _)| *index);
    let remove: Vec<ClipId> = replacements.iter().map(|(_, id, _)| *id).collect();
    let mut insert = Vec::new();
    let mut added = 0;
    for (k, (index, _, clips)) in replacements.into_iter().enumerate() {
        let base = index - k + added;
        added += clips.len();
        insert.extend(clips.into_iter().enumerate().map(|(m, c)| (base + m, c)));
    }
    project
        .replace_clips(&remove, &insert)
        .map_err(project_error)
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

/// The original stream's available range for a clip's media, in the clip's
/// own clock. Unknown metadata (never verified) allows shrinking but no
/// extension past the current range.
pub fn source_range(media: Option<&MediaSummary>, clip: &Clip, project: Timebase) -> SourceRange {
    // Current exclusive end, rounded up to whole ticks: shrinking stays valid.
    let current_end =
        cutcap_core::map_clip_source(clip, clip.start, project, clip.source_in.timebase)
            .ok()
            .map(|m| m.end_exclusive.ticks);
    let known = media.and_then(|m| match (clip.stream, clip.link) {
        // An independent audio clip is bounded by its own stream. A clip that
        // already reaches outside it (audio that starts after the picture,
        // kept through Unlink) stays valid; it just cannot grow further.
        (ClipStream::Audio { index }, None) => {
            let audio = m.metadata.audio_stream(index)?;
            let tb = clip.source_in.timebase;
            let first = audio.start().rescale(tb, Rounding::Ceil).ok()?.ticks;
            let end = audio.duration_ticks.and_then(|d| {
                let end = SourceTime {
                    ticks: audio.start_pts.checked_add(d)?,
                    timebase: audio.timebase,
                };
                end.rescale(tb, Rounding::Floor).ok().map(|t| t.ticks)
            });
            Some(SourceRange {
                first: first.min(clip.source_in.ticks),
                end: match (end, current_end) {
                    (Some(e), Some(c)) => Some(e.max(c)),
                    (e, c) => e.or(c),
                },
            })
        }
        // Video clips and linked pairs live on the video stream's clock.
        _ => {
            let video = m.metadata.video()?;
            (video.timebase == clip.source_in.timebase).then(|| SourceRange {
                first: video.start_pts,
                end: video
                    .duration_ticks
                    .and_then(|d| video.start_pts.checked_add(d)),
            })
        }
    });
    known.unwrap_or(SourceRange {
        first: clip.source_in.ticks,
        end: current_end,
    })
}

/// A stream's length on the project clock (floor). A stream that reports no
/// duration of its own uses its source's.
fn stream_length(
    summary: &MediaSummary,
    stream: &AudioStreamMetadata,
    project: Timebase,
) -> Result<TimelineDuration, EngineError> {
    let (ticks, timebase) = stream
        .duration_ticks
        .filter(|t| *t > 0)
        .map(|t| (t, stream.timebase))
        .or_else(|| {
            let video = summary.metadata.video()?;
            Some((video.duration_ticks.filter(|t| *t > 0)?, video.timebase))
        })
        .ok_or_else(|| invalid("This audio has no known duration, so it cannot be placed yet."))?;
    let length = SourceTime { ticks, timebase }
        .rescale(project, Rounding::Floor)
        .map_err(|_| invalid("Clip timing overflows the timeline clock."))?;
    u64::try_from(length.ticks)
        .ok()
        .and_then(|t| TimelineDuration::new(t).ok())
        .ok_or_else(|| invalid("This audio is shorter than one timeline tick."))
}

/// An independent clip of the whole stream starting at `start`. Its in-point
/// is the stream's first sample on the `1/sample_rate` clock.
pub fn independent_audio(
    project: &Project,
    summary: &MediaSummary,
    stream: &AudioStreamMetadata,
    track_id: TrackId,
    start: TimelineTime,
) -> Result<Clip, EngineError> {
    let audio = ClipStream::Audio {
        index: stream.stream_index,
    };
    track_for(project, track_id, audio)?;
    let duration = stream_length(summary, stream, project.timebase())?;
    start
        .checked_add(duration)
        .map_err(|_| invalid("Clip timing overflows the timeline clock."))?;
    let samples = Timebase::new(1, stream.sample_rate)
        .map_err(|_| invalid("This audio stream has an invalid sample rate."))?;
    let source_in = stream
        .start()
        .rescale(samples, Rounding::Ceil)
        .map_err(|_| invalid("This audio stream's start cannot be represented."))?;
    Ok(Clip {
        id: ClipId::new(),
        media_id: summary.id,
        track_id,
        start,
        duration,
        source_in,
        stream: audio,
        link: None,
    })
}

/// An exact partner for `clip`: the one unlinked clip of the other family
/// with the same media, start, duration and source in-point.
pub fn link_candidate<'a>(clips: &'a [ClipSummary], clip: &ClipSummary) -> Option<&'a ClipSummary> {
    if clip.link.is_some() {
        return None;
    }
    let mut found = clips.iter().filter(|c| {
        c.link.is_none()
            && c.id != clip.id
            && c.stream.track_kind() != clip.stream.track_kind()
            && c.media_id == clip.media_id
            && c.start == clip.start
            && c.duration == clip.duration
            && c.source_in == clip.source_in
    });
    let first = found.next()?;
    found.next().is_none().then_some(first)
}

/// The first audio track (project order) free for the stream's whole interval.
fn first_free_audio_track(
    project: &Project,
    summary: &MediaSummary,
    stream: &AudioStreamMetadata,
    position: TimelineSeek,
) -> Result<TrackId, EngineError> {
    let start = resolve(project, position)?;
    let end = start
        .checked_add(stream_length(summary, stream, project.timebase())?)
        .map_err(|_| invalid("Clip timing overflows the timeline clock."))?;
    project
        .tracks()
        .iter()
        .find(|t| {
            t.kind == TrackKind::Audio
                && project
                    .clips()
                    .iter()
                    .filter(|c| c.track_id == t.id)
                    .all(|c| c.end().is_ok_and(|e| e <= start) || c.start >= end)
        })
        .map(|t| t.id)
        .ok_or_else(|| {
            invalid(
                "No audio track is free at this position. Add an audio track or move the playhead.",
            )
        })
}

fn ready<'a>(
    media: &'a BTreeMap<MediaId, MediaSummary>,
    availability: &BTreeMap<MediaId, MediaAvailability>,
    id: MediaId,
) -> Result<&'a MediaSummary, EngineError> {
    media
        .get(&id)
        .filter(|_| {
            availability
                .get(&id)
                .is_some_and(|a| a.state == Availability::Ready)
        })
        .ok_or_else(|| invalid("Only a verified, ready source can be added to the timeline."))
}

fn place_audio(
    project: &mut Project,
    summary: &MediaSummary,
    stream: &AudioStreamMetadata,
    track_id: TrackId,
    position: TimelineSeek,
) -> Result<Outcome, EngineError> {
    let start = resolve(project, position)?;
    let clip = independent_audio(project, summary, stream, track_id, start)?;
    let id = clip.id;
    let at = project.clips().len();
    let change = project
        .replace_clips(&[], &[(at, clip)])
        .map_err(project_error)?;
    Ok(Outcome {
        change,
        label: format!("add {}", crate::view::file_name(summary)),
        notice: None,
        selection: SelectionState::Clip(id),
    })
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
            let summary = ready(media, availability, media_id)?;
            // An audio-only source goes on an audio track as an independent clip.
            if summary.metadata.is_audio_only() {
                let stream = summary
                    .metadata
                    .primary_audio()
                    .ok_or_else(|| invalid("This source has no decodable audio stream."))?;
                return place_audio(project, summary, stream, track_id, position);
            }
            let video = summary
                .metadata
                .video()
                .ok_or_else(|| invalid("This source has no video stream."))?;
            video_track(project, track_id)?;
            let start = resolve(project, position)?;
            let length = whole_source(summary, tb)?;
            start
                .checked_add(length)
                .map_err(|_| invalid("Clip timing overflows the timeline clock."))?;
            let clip = Clip {
                stream: cutcap_core::ClipStream::Video,
                link: None,
                id: ClipId::new(),
                media_id,
                track_id,
                start,
                duration: length,
                source_in: video.start(),
            };
            let id = clip.id;
            let clips = with_linked_audio(project, summary, clip)?;
            let linked = clips.len() == 2;
            let base = project.clips().len();
            let insert: Vec<_> = clips
                .into_iter()
                .enumerate()
                .map(|(i, c)| (base + i, c))
                .collect();
            let change = project.replace_clips(&[], &insert).map_err(project_error)?;
            Ok(Outcome {
                change,
                label: format!(
                    "add {}{}",
                    crate::view::file_name(summary),
                    if linked { " with audio" } else { "" }
                ),
                notice: None,
                selection: SelectionState::Clip(id),
            })
        }
        Command::PlaceAudioStream {
            media_id,
            stream,
            track_id,
            position,
        } => {
            let summary = ready(media, availability, media_id)?;
            let stream = summary
                .metadata
                .audio_stream(stream)
                .ok_or_else(|| invalid("That audio stream cannot be decoded here."))?;
            let track_id = match track_id {
                Some(track) => track,
                None => first_free_audio_track(project, summary, stream, position)?,
            };
            place_audio(project, summary, stream, track_id, position)
        }
        Command::UnlinkClip { clip_id } => {
            let members = group(project, clip_id)?;
            if members.len() != 2 {
                return Err(invalid("That clip is not linked."));
            }
            let (_, clip) = find(project, clip_id)?;
            let replacements = members
                .into_iter()
                .map(|(i, member)| {
                    let id = member.id;
                    (
                        i,
                        id,
                        vec![Clip {
                            link: None,
                            ..member
                        }],
                    )
                })
                .collect();
            let change = replace_group(project, replacements)?;
            Ok(Outcome {
                change,
                label: format!("unlink {}", name(media, &clip)),
                notice: None,
                selection: SelectionState::Clip(clip_id),
            })
        }
        Command::LinkClips {
            clip_id,
            partner_id,
        } => {
            let (a_at, a) = find(project, clip_id)?;
            let (b_at, b) = find(project, partner_id)?;
            if a.link.is_some() || b.link.is_some() {
                return Err(invalid("Unlink those clips before linking them again."));
            }
            let stream = [a.stream, b.stream].into_iter().find_map(|s| match s {
                ClipStream::Audio { index } => Some(index),
                ClipStream::Video => None,
            });
            let decodable = stream.is_some_and(|index| {
                media
                    .get(&a.media_id)
                    .is_some_and(|m| m.metadata.audio_stream(index).is_some())
            });
            if a.stream.track_kind() == b.stream.track_kind() || !decodable {
                return Err(invalid(
                    "Only a video clip and an audio clip of the same source can be linked.",
                ));
            }
            if a.media_id != b.media_id
                || a.start != b.start
                || a.duration != b.duration
                || a.source_in != b.source_in
            {
                return Err(invalid(
                    "Linked clips must have the same source, timing and in-point.",
                ));
            }
            let link = Some(LinkId::new());
            let (a_id, b_id) = (a.id, b.id);
            let label = format!("link {}", name(media, &a));
            let change = replace_group(
                project,
                vec![
                    (a_at, a_id, vec![Clip { link, ..a }]),
                    (b_at, b_id, vec![Clip { link, ..b }]),
                ],
            )?;
            Ok(Outcome {
                change,
                label,
                notice: None,
                selection: SelectionState::Clip(clip_id),
            })
        }
        Command::MoveClip {
            clip_id,
            track_id,
            position,
            anchor,
        } => {
            let (_, clip) = find(project, clip_id)?;
            track_for(project, track_id, clip.stream)?;
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
            // The dragged clip may change track (within its family); a linked
            // partner keeps its own track and moves by the same time.
            let replacements = group(project, clip_id)?
                .into_iter()
                .map(|(i, member)| {
                    let track_id = if member.id == clip_id {
                        track_id
                    } else {
                        member.track_id
                    };
                    let id = member.id;
                    (
                        i,
                        id,
                        vec![Clip {
                            start,
                            track_id,
                            ..member
                        }],
                    )
                })
                .collect();
            let change = replace_group(project, replacements)?;
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
            let (_, clip) = find(project, clip_id)?;
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
            // Linked members share timing and source in-point: same new values.
            let replacements = group(project, clip_id)?
                .into_iter()
                .map(|(i, member)| {
                    let id = member.id;
                    (
                        i,
                        id,
                        vec![Clip {
                            start: trimmed.start,
                            duration: trimmed.duration,
                            source_in: trimmed.source_in,
                            ..member
                        }],
                    )
                })
                .collect();
            let change = replace_group(project, replacements)?;
            Ok(Outcome {
                change,
                label: format!("trim {}", name(media, &clip)),
                notice: adjustment(tb, requested, applied),
                selection: SelectionState::Clip(clip_id),
            })
        }
        Command::SplitClip { clip_id, position } => {
            let (_, clip) = find(project, clip_id)?;
            let requested = resolve(project, position)?;
            let length = ticks(clip.duration)?;
            let offset = cutcap_core::edit::split_offset(&clip, requested, tb)
                .map_err(|e| invalid(e.to_string()))?;
            let right_start = TimelineTime(clip.start.0 + offset);
            let right_source = shifted_source(clip.source_in, offset, tb).map_err(edit_error)?;
            let (left_length, right_length) = (duration(offset)?, duration(length - offset)?);
            // Both halves of a linked pair split at the same aligned offset; the
            // right halves form a new pair, the left halves keep the old link.
            let right_link = clip.link.map(|_| LinkId::new());
            let replacements = group(project, clip_id)?
                .into_iter()
                .map(|(i, member)| {
                    let left = Clip {
                        duration: left_length,
                        ..member.clone()
                    };
                    let right = Clip {
                        id: ClipId::new(),
                        start: right_start,
                        duration: right_length,
                        source_in: right_source,
                        link: right_link,
                        ..member.clone()
                    };
                    (i, member.id, vec![left, right])
                })
                .collect();
            let applied = right_start;
            let change = replace_group(project, replacements)?;
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
    let video = source
        .metadata
        .video()
        .ok_or_else(|| invalid("This source has no video stream."))?;
    let ticks = video
        .duration_ticks
        .filter(|t| *t > 0)
        .ok_or_else(|| invalid("This source has no known duration, so it cannot be placed yet."))?;
    let length = SourceTime {
        ticks,
        timebase: video.timebase,
    }
    .rescale(project, Rounding::Floor)
    .map_err(|_| invalid("Clip timing overflows the timeline clock."))?;
    u64::try_from(length.ticks)
        .ok()
        .and_then(|t| TimelineDuration::new(t).ok())
        .ok_or_else(|| invalid("This source is shorter than one timeline tick."))
}
