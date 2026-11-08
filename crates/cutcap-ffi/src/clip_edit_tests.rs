//! Phase 3E clip editing (place, move, trim, split) against the command owner
//! directly: deterministic, no workers or FFmpeg. Real-media continuity is in
//! the .NET/FFmpeg interop tests.
use super::*;
use cutcap_core::{Clip, map_clip_source};

fn probe(timebase: &str, start_pts: i64, duration: i64) -> Vec<u8> {
    format!(
        r#"{{"streams":[{{"index":0,"codec_type":"video","codec_name":"ffv1","width":320,"height":180,"time_base":"{timebase}","start_pts":{start_pts},"duration_ts":{duration}}}],"format":{{"format_name":"matroska"}}}}"#
    )
    .into_bytes()
}

fn send(
    engine: &mut Engine,
    project: ProjectId,
    command: Command,
) -> Result<ProjectSnapshot, EngineError> {
    match engine.handle(CommandEnvelope {
        request_id: RequestId::new(),
        project_id: Some(project),
        command,
    }) {
        EngineEvent::Completed { snapshot, .. } => Ok(snapshot),
        EngineEvent::Failed { error, .. } => Err(error),
        event => panic!("unexpected {event:?}"),
    }
}

struct Bench {
    engine: Engine,
    project: ProjectId,
    media: MediaId,
    video: TrackId,
    audio: TrackId,
}

impl Bench {
    /// A project with one ready source registered.
    fn new(probe: &[u8]) -> Self {
        let mut engine = Engine::default();
        let snapshot = match engine.handle(CommandEnvelope {
            request_id: RequestId::new(),
            project_id: None,
            command: Command::NewProject {
                name: "Edit".into(),
            },
        }) {
            EngineEvent::Completed { snapshot, .. } => snapshot,
            event => panic!("unexpected {event:?}"),
        };
        let media = MediaSummary {
            id: MediaId::new(),
            path: "C:/clips/take.mkv".into(),
            metadata: cutcap_media::parse_probe(probe).unwrap(),
        };
        let id = media.id;
        send(
            &mut engine,
            snapshot.id,
            Command::RegisterMedia {
                media: Box::new(media),
                fingerprint: None,
            },
        )
        .unwrap();
        Self {
            engine,
            project: snapshot.id,
            media: id,
            video: snapshot.tracks[0].id,
            audio: snapshot.tracks[1].id,
        }
    }
    fn send(&mut self, command: Command) -> Result<ProjectSnapshot, EngineError> {
        send(&mut self.engine, self.project, command)
    }
    fn place(&mut self, track: TrackId, at: i64) -> ClipSummary {
        let snapshot = self
            .send(Command::PlaceClip {
                media_id: self.media,
                track_id: track,
                position: TimelineSeek::Ticks(TimelineTime(at)),
            })
            .unwrap();
        let SelectionState::Clip(id) = snapshot.selection else {
            panic!("no selection")
        };
        snapshot.clips.into_iter().find(|c| c.id == id).unwrap()
    }
    fn clips(&self) -> Vec<ClipSummary> {
        self.engine.snapshot().unwrap().clips
    }
    fn state(&self) -> ProjectSnapshot {
        self.engine.snapshot().unwrap()
    }
    fn add_video_track(&mut self) -> TrackId {
        match self
            .send(Command::AddTrack {
                kind: TrackKind::Video,
            })
            .unwrap()
            .selection
        {
            SelectionState::Track(id) => id,
            other => panic!("{other:?}"),
        }
    }
    fn mark_saved(&mut self) {
        self.engine.document.saved_revision = Some(self.engine.document.revision);
    }
    fn tb(&self) -> Timebase {
        self.state().timebase
    }
}

fn ticks(t: i64) -> TimelineSeek {
    TimelineSeek::Ticks(TimelineTime(t))
}

fn core(c: &ClipSummary) -> Clip {
    Clip {
        stream: c.stream,
        link: c.link,
        id: c.id,
        media_id: c.media_id,
        track_id: c.track_id,
        start: c.start,
        duration: c.duration,
        source_in: c.source_in,
    }
}

/// The source time a timeline position maps to, using the evaluator's own mapping.
fn source_at(clip: &ClipSummary, position: i64, project: Timebase) -> i64 {
    map_clip_source(
        &core(clip),
        TimelineTime(position),
        project,
        clip.source_in.timebase,
    )
    .unwrap()
    .requested
    .ticks
}

const SEC: i64 = 720_000; // project ticks per second (1/720000)

#[test]
fn place_puts_the_whole_source_at_the_drop_position_and_rejects_overlap_and_audio() {
    let mut b = Bench::new(&probe("1/1000", 0, 3000));
    let clip = b.place(b.video, 5 * SEC);
    assert_eq!(
        (clip.start.0, clip.duration.ticks()),
        (5 * SEC, 3 * SEC as u64)
    );
    let before = b.state();
    for (command, fragment) in [
        (
            Command::PlaceClip {
                media_id: b.media,
                track_id: b.video,
                position: ticks(6 * SEC),
            },
            "overlap",
        ),
        (
            Command::PlaceClip {
                media_id: b.media,
                track_id: b.audio,
                position: ticks(0),
            },
            "video track",
        ),
        (
            Command::PlaceClip {
                media_id: MediaId::new(),
                track_id: b.video,
                position: ticks(0),
            },
            "ready source",
        ),
        (
            Command::PlaceClip {
                media_id: b.media,
                track_id: b.video,
                position: TimelineSeek::Milliseconds(-1),
            },
            "beginning",
        ),
        (
            Command::PlaceClip {
                media_id: b.media,
                track_id: b.video,
                position: ticks(i64::MAX - 10),
            },
            "overflows",
        ),
    ] {
        let error = b.send(command).unwrap_err();
        assert!(error.message.contains(fragment), "{fragment}: {error:?}");
        assert_eq!(b.state(), before, "{fragment} changed state");
    }
    // Milliseconds floor to project ticks.
    let snapshot = b
        .send(Command::PlaceClip {
            media_id: b.media,
            track_id: b.video,
            position: TimelineSeek::Milliseconds(9_000),
        })
        .unwrap();
    assert!(snapshot.clips.iter().any(|c| c.start.0 == 9 * SEC));
}

#[test]
fn move_within_and_across_tracks_preserves_identity_media_source_and_duration() {
    let mut b = Bench::new(&probe("1/1000", 0, 3000));
    let clip = b.place(b.video, 0);
    let second = b.add_video_track();
    b.mark_saved();
    let moved = b
        .send(Command::MoveClip {
            clip_id: clip.id,
            track_id: b.video,
            position: ticks(10 * SEC),
            anchor: ClipEdge::Start,
        })
        .unwrap();
    let after = &moved.clips[0];
    assert_eq!(after.id, clip.id);
    assert_eq!(
        (after.media_id, after.source_in, after.duration),
        (clip.media_id, clip.source_in, clip.duration)
    );
    assert_eq!(after.start.0, 10 * SEC);
    assert_eq!(moved.history.undo.as_deref(), Some("move take.mkv"));
    assert!(moved.document.dirty);
    // Across tracks, anchored by the end edge: one history entry.
    let across = b
        .send(Command::MoveClip {
            clip_id: clip.id,
            track_id: second,
            position: ticks(20 * SEC),
            anchor: ClipEdge::End,
        })
        .unwrap();
    assert_eq!(
        (across.clips[0].track_id, across.clips[0].start.0),
        (second, 17 * SEC)
    );
    assert_eq!(across.selection, SelectionState::Clip(clip.id));
    assert_eq!(across.history.depth, moved.history.depth + 1);
    // Undo twice returns to the saved, original clip exactly.
    b.send(Command::Undo).unwrap();
    let back = b.send(Command::Undo).unwrap();
    assert_eq!(back.clips, vec![clip.clone()]);
    assert!(!back.document.dirty);
    let redone = b.send(Command::Redo).unwrap();
    assert_eq!(redone.clips[0].start.0, 10 * SEC);
}

#[test]
fn invalid_moves_are_atomic_and_unrecorded() {
    let mut b = Bench::new(&probe("1/1000", 0, 3000));
    let first = b.place(b.video, 0);
    b.place(b.video, 3 * SEC);
    let before = b.state();
    for (command, fragment) in [
        (
            Command::MoveClip {
                clip_id: first.id,
                track_id: b.video,
                position: ticks(4 * SEC),
                anchor: ClipEdge::Start,
            },
            "overlap",
        ),
        (
            Command::MoveClip {
                clip_id: first.id,
                track_id: b.audio,
                position: ticks(9 * SEC),
                anchor: ClipEdge::Start,
            },
            "video track",
        ),
        (
            Command::MoveClip {
                clip_id: first.id,
                track_id: b.video,
                position: ticks(SEC),
                anchor: ClipEdge::End,
            },
            "beginning",
        ),
        (
            Command::MoveClip {
                clip_id: first.id,
                track_id: b.video,
                position: ticks(i64::MAX - 5),
                anchor: ClipEdge::Start,
            },
            "overflows",
        ),
        (
            Command::MoveClip {
                clip_id: first.id,
                track_id: b.video,
                position: ticks(0),
                anchor: ClipEdge::Start,
            },
            "already",
        ),
        (
            Command::MoveClip {
                clip_id: ClipId::new(),
                track_id: b.video,
                position: ticks(0),
                anchor: ClipEdge::Start,
            },
            "no longer",
        ),
        (
            Command::MoveClip {
                clip_id: first.id,
                track_id: TrackId::new(),
                position: ticks(0),
                anchor: ClipEdge::Start,
            },
            "track no longer",
        ),
    ] {
        let error = b.send(command).unwrap_err();
        assert!(error.message.contains(fragment), "{fragment}: {error:?}");
        assert_eq!(b.state(), before, "{fragment} changed state");
    }
}

#[test]
fn start_trim_keeps_the_retained_frames_exact_and_reports_grid_alignment() {
    // 1/15360 s stream: one stream tick is 46.875 project ticks; grid step 375.
    let mut b = Bench::new(&probe("1/15360", 1234, 15360 * 10));
    let tb = b.tb();
    assert_eq!(
        cutcap_core::edit::source_step(tb, Timebase::new(1, 15360).unwrap()),
        375
    );
    let clip = b.place(b.video, SEC);
    let probe_at = 4 * SEC + 12_345; // a retained position
    let original = source_at(&clip, probe_at, tb);
    // On the grid: no notice, exact source_in shift.
    let trimmed = b
        .send(Command::TrimClip {
            clip_id: clip.id,
            edge: ClipEdge::Start,
            position: ticks(SEC + 3750),
        })
        .unwrap();
    let t = &trimmed.clips[0];
    assert_eq!(trimmed.edit_notice, None);
    assert_eq!(
        (t.id, t.start.0, t.source_in.ticks),
        (clip.id, SEC + 3750, 1234 + 80)
    );
    assert_eq!(t.duration.ticks(), clip.duration.ticks() - 3750);
    assert_eq!(
        source_at(t, probe_at, tb),
        original,
        "retained frames keep their mapping"
    );
    // Off the grid: aligned to the nearest grid point, with a notice.
    let aligned = b
        .send(Command::TrimClip {
            clip_id: clip.id,
            edge: ClipEdge::Start,
            position: ticks(2 * SEC + 100),
        })
        .unwrap();
    let a = &aligned.clips[0];
    assert_eq!((a.start.0 - SEC) % 375, 0);
    assert_eq!(
        a.start.0,
        2 * SEC + 375,
        "rounded up to the next grid point"
    );
    assert!(
        aligned
            .edit_notice
            .as_deref()
            .is_some_and(|n| n.contains("source tick")),
        "{:?}",
        aligned.edit_notice
    );
    assert_eq!(source_at(a, probe_at, tb), original);
    // Extending back left is allowed within the source, not before its first PTS.
    let extended = b
        .send(Command::TrimClip {
            clip_id: clip.id,
            edge: ClipEdge::Start,
            position: ticks(SEC),
        })
        .unwrap();
    assert_eq!(extended.clips[0], clip);
    let error = b
        .send(Command::TrimClip {
            clip_id: clip.id,
            edge: ClipEdge::Start,
            position: ticks(SEC - 375),
        })
        .unwrap_err();
    assert!(error.message.contains("first frame"), "{error:?}");
    // Collapsing the clip is rejected.
    let error = b
        .send(Command::TrimClip {
            clip_id: clip.id,
            edge: ClipEdge::Start,
            position: ticks(11 * SEC + 375),
        })
        .unwrap_err();
    assert!(error.message.contains("timeline tick"), "{error:?}");
}

#[test]
fn end_trim_shrinks_and_extends_within_the_source_only() {
    let mut b = Bench::new(&probe("1/1000", 0, 3000));
    let clip = b.place(b.video, 0);
    let short = b
        .send(Command::TrimClip {
            clip_id: clip.id,
            edge: ClipEdge::End,
            position: ticks(SEC + 7),
        })
        .unwrap();
    let s = &short.clips[0];
    assert_eq!(
        (s.id, s.start, s.source_in, s.duration.ticks()),
        (clip.id, clip.start, clip.source_in, (SEC + 7) as u64)
    );
    // End trims keep source_in, so any tick is exact: no alignment.
    assert_eq!(short.edit_notice, None);
    let longer = b
        .send(Command::TrimClip {
            clip_id: clip.id,
            edge: ClipEdge::End,
            position: ticks(3 * SEC),
        })
        .unwrap();
    assert_eq!(longer.clips[0], clip, "extension back to the full source");
    let error = b
        .send(Command::TrimClip {
            clip_id: clip.id,
            edge: ClipEdge::End,
            position: ticks(3 * SEC + 1),
        })
        .unwrap_err();
    assert!(error.message.contains("end of its source"), "{error:?}");
    for position in [0, -5] {
        assert!(
            b.send(Command::TrimClip {
                clip_id: clip.id,
                edge: ClipEdge::End,
                position: ticks(position)
            })
            .is_err()
        );
    }
    // A trim into a neighbour is an overlap.
    b.send(Command::TrimClip {
        clip_id: clip.id,
        edge: ClipEdge::End,
        position: ticks(SEC),
    })
    .unwrap();
    b.place(b.video, 2 * SEC);
    let error = b
        .send(Command::TrimClip {
            clip_id: clip.id,
            edge: ClipEdge::End,
            position: ticks(2 * SEC + 1),
        })
        .unwrap_err();
    assert!(error.message.contains("overlap"), "{error:?}");
}

#[test]
fn split_preserves_mapping_order_and_identity_with_one_undo_step() {
    let mut b = Bench::new(&probe("1/90000", 900, 90000 * 6));
    let tb = b.tb();
    let other = b.place(b.video, 20 * SEC);
    let clip = b.place(b.video, SEC);
    let cut = 3 * SEC + 5; // off the 8-tick grid of a 1/90000 stream
    let before = b.state();
    let positions = [
        SEC,
        2 * SEC + 1,
        3 * SEC - 1,
        3 * SEC + 8,
        5 * SEC,
        7 * SEC - 1,
    ];
    let mapped: Vec<i64> = positions.iter().map(|p| source_at(&clip, *p, tb)).collect();
    let split = b
        .send(Command::SplitClip {
            clip_id: clip.id,
            position: ticks(cut),
        })
        .unwrap();
    assert_eq!(split.clips.len(), 3);
    let (left, right) = (&split.clips[1], &split.clips[2]);
    assert_eq!(
        split.clips[0], other,
        "unrelated clip untouched, order kept"
    );
    assert_eq!(left.id, clip.id, "the left part keeps the identity");
    assert_ne!(right.id, clip.id);
    assert_eq!(
        right.start.0,
        3 * SEC + 8,
        "aligned to the 8-tick source grid"
    );
    assert!(split.edit_notice.is_some());
    assert_eq!(left.start, clip.start);
    assert_eq!(
        left.duration.ticks() + right.duration.ticks(),
        clip.duration.ticks()
    );
    assert_eq!(right.start.0, left.start.0 + left.duration.ticks() as i64);
    assert_eq!(
        (left.media_id, right.media_id),
        (clip.media_id, clip.media_id)
    );
    // The left part's exclusive source end is exactly the right part's source_in.
    let left_end = map_clip_source(&core(left), left.start, tb, left.source_in.timebase)
        .unwrap()
        .end_exclusive;
    assert_eq!(left_end, right.source_in);
    for (p, expected) in positions.iter().zip(&mapped) {
        let part = if *p < right.start.0 { left } else { right };
        assert_eq!(
            source_at(part, *p, tb),
            *expected,
            "position {p} maps to the same source tick"
        );
    }
    assert_eq!(split.history.undo.as_deref(), Some("split take.mkv"));
    let undone = b.send(Command::Undo).unwrap();
    assert_eq!(
        undone.clips, before.clips,
        "one undo restores the exact original"
    );
    let redone = b.send(Command::Redo).unwrap();
    assert_eq!(
        redone.clips, split.clips,
        "redo reproduces the same identities and ranges"
    );
}

#[test]
fn split_and_trim_reject_edges_off_clip_and_unrepresentable_cuts() {
    let mut b = Bench::new(&probe("1/1000", 0, 3000));
    let clip = b.place(b.video, 0);
    let before = b.state();
    for position in [0, 3 * SEC, 4 * SEC, 100] {
        // 100 ticks aligns to 0 on the 720-tick grid of a 1/1000 stream: the edge.
        let error = b
            .send(Command::SplitClip {
                clip_id: clip.id,
                position: ticks(position),
            })
            .unwrap_err();
        assert!(
            error.message.contains("inside the clip") || error.message.contains("too close"),
            "{position}: {error:?}"
        );
        assert_eq!(b.state(), before);
    }
    assert!(
        b.send(Command::SplitClip {
            clip_id: ClipId::new(),
            position: ticks(SEC)
        })
        .is_err()
    );
    assert_eq!(b.state().history.depth, before.history.depth);
}

#[test]
fn repeated_splits_and_trims_never_accumulate_rounding() {
    let mut b = Bench::new(&probe("1/15360", -512, 15360 * 30));
    let tb = b.tb();
    let original = b.place(b.video, 0);
    let samples: Vec<i64> = (0..60)
        .map(|i| i * 360_007)
        .filter(|p| *p < 30 * SEC)
        .collect();
    let expected: Vec<i64> = samples
        .iter()
        .map(|p| source_at(&original, *p, tb))
        .collect();
    // Twenty off-grid splits of whatever clip lies under each cut.
    for k in 1..=20 {
        let cut = k * 1_000_003;
        let clips = b.clips();
        let target = clips
            .iter()
            .find(|c| c.start.0 < cut && cut < c.start.0 + c.duration.ticks() as i64)
            .unwrap();
        b.send(Command::SplitClip {
            clip_id: target.id,
            position: ticks(cut),
        })
        .unwrap();
    }
    let mut clips = b.clips();
    clips.sort_by_key(|c| c.start);
    assert_eq!(clips.len(), 21);
    for pair in clips.windows(2) {
        let end = map_clip_source(
            &core(&pair[0]),
            pair[0].start,
            tb,
            pair[0].source_in.timebase,
        )
        .unwrap()
        .end_exclusive;
        assert_eq!(
            pair[0].start.0 + pair[0].duration.ticks() as i64,
            pair[1].start.0,
            "no gap or overlap"
        );
        assert_eq!(
            end, pair[1].source_in,
            "no repeated or skipped source tick at a cut"
        );
    }
    for (p, e) in samples.iter().zip(&expected) {
        let part = clips
            .iter()
            .find(|c| c.start.0 <= *p && *p < c.start.0 + c.duration.ticks() as i64)
            .unwrap();
        assert_eq!(source_at(part, *p, tb), *e, "position {p}");
    }
    // Twenty undos return exactly to the single original clip.
    for _ in 0..20 {
        b.send(Command::Undo).unwrap();
    }
    assert_eq!(b.clips(), vec![original]);
}

#[test]
fn failed_edits_keep_saved_state_and_divergent_edits_drop_redo() {
    let mut b = Bench::new(&probe("1/1000", 0, 3000));
    let clip = b.place(b.video, 0);
    b.mark_saved();
    let saved = b.state();
    assert!(
        b.send(Command::TrimClip {
            clip_id: clip.id,
            edge: ClipEdge::End,
            position: ticks(9 * SEC)
        })
        .is_err()
    );
    assert_eq!(b.state(), saved);
    assert!(!b.state().document.dirty);
    b.send(Command::SplitClip {
        clip_id: clip.id,
        position: ticks(SEC),
    })
    .unwrap();
    let undone = b.send(Command::Undo).unwrap();
    assert!(!undone.document.dirty, "undo back to the saved revision");
    assert!(undone.history.can_redo);
    let diverged = b
        .send(Command::MoveClip {
            clip_id: clip.id,
            track_id: b.video,
            position: ticks(SEC),
            anchor: ClipEdge::Start,
        })
        .unwrap();
    assert!(!diverged.history.can_redo);
    assert!(diverged.document.dirty);
    // Selection never dirties or enters history.
    b.mark_saved();
    let selected = b
        .send(Command::Select {
            selection: SelectionState::None,
        })
        .unwrap();
    assert!(!selected.document.dirty);
    assert_eq!(selected.history.depth, diverged.history.depth);
}

#[test]
fn clips_of_missing_sources_can_be_edited_within_their_known_range() {
    let mut b = Bench::new(&probe("1/1000", 0, 3000));
    let clip = b.place(b.video, 0);
    b.engine.availability.get_mut(&b.media).unwrap().state = Availability::Missing;
    // Move and shrink work; the saved metadata still bounds extension.
    b.send(Command::MoveClip {
        clip_id: clip.id,
        track_id: b.video,
        position: ticks(SEC),
        anchor: ClipEdge::Start,
    })
    .unwrap();
    b.send(Command::TrimClip {
        clip_id: clip.id,
        edge: ClipEdge::End,
        position: ticks(2 * SEC),
    })
    .unwrap();
    assert!(
        b.send(Command::TrimClip {
            clip_id: clip.id,
            edge: ClipEdge::End,
            position: ticks(5 * SEC)
        })
        .is_err()
    );
    // Placing new clips still needs a Ready source.
    let error = b
        .send(Command::PlaceClip {
            media_id: b.media,
            track_id: b.video,
            position: ticks(10 * SEC),
        })
        .unwrap_err();
    assert!(error.message.contains("ready source"));
    // Without any metadata, only shrinking is possible.
    b.engine.media_details.clear();
    b.send(Command::TrimClip {
        clip_id: clip.id,
        edge: ClipEdge::End,
        position: ticks(SEC + SEC / 2),
    })
    .unwrap();
    assert!(
        b.send(Command::TrimClip {
            clip_id: clip.id,
            edge: ClipEdge::End,
            position: ticks(2 * SEC)
        })
        .is_err()
    );
}

#[test]
fn edits_renew_the_evaluation_generation_and_invalidate_the_index() {
    let mut b = Bench::new(&probe("1/1000", 0, 3000));
    let clip = b.place(b.video, 0);
    let generation = b.state().evaluation_generation;
    b.send(Command::SplitClip {
        clip_id: clip.id,
        position: ticks(SEC),
    })
    .unwrap();
    assert_ne!(
        b.state().evaluation_generation,
        generation,
        "transport epochs are invalidated"
    );
    // A failed edit leaves the generation (and therefore active playback) alone.
    let generation = b.state().evaluation_generation;
    assert!(
        b.send(Command::SplitClip {
            clip_id: clip.id,
            position: ticks(0)
        })
        .is_err()
    );
    assert_eq!(b.state().evaluation_generation, generation);
}

#[test]
#[ignore = "performance measurement; run with --ignored --nocapture --release"]
fn measure_edit_latency() {
    let mut b = Bench::new(&probe("1/15360", 0, 15360 * 3));
    let mut placed = 0usize;
    for target in [1_000usize, 5_000, 20_000] {
        while placed < target {
            b.place(b.video, placed as i64 * 3 * SEC);
            placed += 1;
        }
        let clip = b.clips()[target / 2].clone();
        let time = |b: &mut Bench, command: Command| {
            let started = std::time::Instant::now();
            b.send(command).unwrap();
            started.elapsed()
        };
        let far = (placed as i64 + 10) * 3 * SEC;
        let video = b.video;
        let mv = time(
            &mut b,
            Command::MoveClip {
                clip_id: clip.id,
                track_id: video,
                position: ticks(far),
                anchor: ClipEdge::Start,
            },
        );
        let trim = time(
            &mut b,
            Command::TrimClip {
                clip_id: clip.id,
                edge: ClipEdge::End,
                position: ticks(far + SEC),
            },
        );
        let split = time(
            &mut b,
            Command::SplitClip {
                clip_id: clip.id,
                position: ticks(far + SEC / 2),
            },
        );
        let undo = time(&mut b, Command::Undo);
        let redo = time(&mut b, Command::Redo);
        for _ in 0..3 {
            b.send(Command::Undo).unwrap();
        }
        println!(
            "clips={placed} move={mv:?} trim={trim:?} split={split:?} undo={undo:?} redo={redo:?}"
        );
    }
}
