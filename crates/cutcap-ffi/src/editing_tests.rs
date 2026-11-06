//! Phase 3B clip editing and history against the command owner directly
//! (deterministic, no workers or FFmpeg). Session/persistence round trips are
//! in tests.rs.
use super::*;
use crate::history::MAX_HISTORY;

const PROBE: &[u8] = br#"{"streams":[{"index":0,"codec_type":"video","codec_name":"ffv1","width":320,"height":180,"time_base":"1/1000","start_pts":0,"duration_ts":3000}],"format":{"format_name":"matroska"}}"#;

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

/// New project with one ready 3 s source (1/1000 clock). Returns (project, media, video track, audio track).
fn editor(engine: &mut Engine, probe: &[u8]) -> (ProjectId, MediaId, TrackId, TrackId) {
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
        path: "C:/clips/source video.mkv".into(),
        metadata: cutcap_media::parse_probe(probe).unwrap(),
    };
    let id = media.id;
    send(
        engine,
        snapshot.id,
        Command::RegisterMedia {
            media: Box::new(media),
            fingerprint: None,
        },
    )
    .unwrap();
    (
        snapshot.id,
        id,
        snapshot.tracks[0].id,
        snapshot.tracks[1].id,
    )
}

/// Marks the current revision saved, as a completed Save does.
fn mark_saved(engine: &mut Engine) {
    engine.document.saved_revision = Some(engine.document.revision);
}

fn dirty(engine: &Engine) -> bool {
    engine.snapshot().unwrap().document.dirty
}

#[test]
fn append_policy_uses_exact_source_duration_and_selects_the_new_clip() {
    let mut engine = Engine::default();
    let (project, media, video, _) = editor(&mut engine, PROBE);
    let first = send(
        &mut engine,
        project,
        Command::InsertClip {
            media_id: media,
            track_id: video,
        },
    )
    .unwrap();
    let clip = &first.clips[0];
    assert_eq!(clip.start, TimelineTime::ZERO);
    // 3000 ticks @ 1/1000 s = 3 s = 2,160,000 ticks @ 1/720000 s.
    assert_eq!(clip.duration.ticks(), 2_160_000);
    assert_eq!(clip.source_in.ticks, 0);
    assert_eq!((clip.media_id, clip.track_id), (media, video));
    assert_eq!(first.selection, SelectionState::Clip(clip.id));
    let second = send(
        &mut engine,
        project,
        Command::InsertClip {
            media_id: media,
            track_id: video,
        },
    )
    .unwrap();
    assert_eq!(
        second.clips[1].start,
        TimelineTime(2_160_000),
        "appended after the last clip"
    );
    assert_eq!(second.clips[0], first.clips[0], "existing clip untouched");
    assert_eq!(second.tracks[0].clip_count, 2);
    assert_eq!(second.history.depth, 2);
    assert_eq!(second.history.undo.as_deref(), Some("add source video.mkv"));
}

#[test]
fn invalid_insertions_and_removals_are_atomic_and_never_recorded() {
    let mut engine = Engine::default();
    let (project, media, video, audio) = editor(&mut engine, PROBE);
    mark_saved(&mut engine);
    let before = engine.snapshot().unwrap();
    let cases = [
        Command::InsertClip {
            media_id: media,
            track_id: audio,
        },
        Command::InsertClip {
            media_id: MediaId::new(),
            track_id: video,
        },
        Command::InsertClip {
            media_id: media,
            track_id: TrackId::new(),
        },
        Command::RemoveClip {
            clip_id: ClipId::new(),
        },
        Command::Undo,
        Command::Redo,
    ];
    for command in cases {
        let error = send(&mut engine, project, command.clone()).expect_err("must fail");
        assert!(
            matches!(error.code, ErrorCode::InvalidInput | ErrorCode::NotFound),
            "{command:?}: {error:?}"
        );
        assert_eq!(
            engine.snapshot().unwrap(),
            before,
            "{command:?} changed state"
        );
    }
    assert!(!dirty(&engine));
    // A stale project identity is rejected before any mutation.
    let stale = send(
        &mut engine,
        ProjectId::new(),
        Command::InsertClip {
            media_id: media,
            track_id: video,
        },
    );
    assert_eq!(stale.unwrap_err().code, ErrorCode::StaleProject);
    assert_eq!(engine.snapshot().unwrap(), before);
}

#[test]
fn sources_without_a_usable_duration_or_not_ready_cannot_be_placed() {
    let mut engine = Engine::default();
    let no_duration = br#"{"streams":[{"index":0,"codec_type":"video","codec_name":"ffv1","width":320,"height":180,"time_base":"1/1000","start_pts":0}],"format":{"format_name":"matroska"}}"#;
    let (project, media, video, _) = editor(&mut engine, no_duration);
    let error = send(
        &mut engine,
        project,
        Command::InsertClip {
            media_id: media,
            track_id: video,
        },
    )
    .unwrap_err();
    assert!(error.message.contains("no known duration"), "{error:?}");
    // A registered source that is not Ready (e.g. missing after reopen) is refused.
    let (project, media, video, _) = editor(&mut engine, PROBE);
    engine.availability.get_mut(&media).unwrap().state = Availability::Missing;
    let error = send(
        &mut engine,
        project,
        Command::InsertClip {
            media_id: media,
            track_id: video,
        },
    )
    .unwrap_err();
    assert!(error.message.contains("ready source"), "{error:?}");
    assert!(engine.snapshot().unwrap().clips.is_empty());
}

#[test]
fn undo_redo_restore_identity_order_selection_and_saved_cleanliness() {
    let mut engine = Engine::default();
    let (project, media, video, _) = editor(&mut engine, PROBE);
    let one = send(
        &mut engine,
        project,
        Command::InsertClip {
            media_id: media,
            track_id: video,
        },
    )
    .unwrap();
    let two = send(
        &mut engine,
        project,
        Command::InsertClip {
            media_id: media,
            track_id: video,
        },
    )
    .unwrap();
    mark_saved(&mut engine);
    let saved = engine.snapshot().unwrap();
    // Save → Remove → Undo is clean again and restores the exact clip at its index.
    let first = one.clips[0].id;
    let removed = send(&mut engine, project, Command::RemoveClip { clip_id: first }).unwrap();
    assert_eq!(removed.clips, vec![two.clips[1].clone()]);
    assert_eq!(removed.media_count, 1, "media asset kept");
    assert!(removed.document.dirty);
    let undone = send(&mut engine, project, Command::Undo).unwrap();
    assert_eq!(undone.clips, saved.clips);
    assert_eq!(undone.selection, SelectionState::Clip(first));
    assert!(!undone.document.dirty, "back at the saved revision");
    assert_eq!(undone.document.revision, saved.document.revision);
    // Redo removal, then undo it again: dirty, clean.
    let redone = send(&mut engine, project, Command::Redo).unwrap();
    assert_eq!(redone.clips.len(), 1);
    assert_eq!(
        redone.selection,
        SelectionState::None,
        "removed clip deselected"
    );
    assert!(redone.document.dirty);
    assert!(
        !send(&mut engine, project, Command::Undo)
            .unwrap()
            .document
            .dirty
    );
    // Undo both inserts to an empty timeline, then redo them with the same identities.
    send(&mut engine, project, Command::Undo).unwrap();
    let empty = send(&mut engine, project, Command::Undo).unwrap();
    assert!(empty.clips.is_empty());
    assert!(empty.document.dirty, "older than the saved revision");
    assert!(!empty.history.can_undo && empty.history.can_redo);
    send(&mut engine, project, Command::Redo).unwrap();
    let all = send(&mut engine, project, Command::Redo).unwrap();
    assert_eq!(all.clips, saved.clips);
    assert!(!all.document.dirty);
}

#[test]
fn edit_then_save_then_undo_redo_and_divergent_edits() {
    let mut engine = Engine::default();
    let (project, media, video, _) = editor(&mut engine, PROBE);
    let inserted = send(
        &mut engine,
        project,
        Command::InsertClip {
            media_id: media,
            track_id: video,
        },
    )
    .unwrap();
    mark_saved(&mut engine);
    assert!(
        send(&mut engine, project, Command::Undo)
            .unwrap()
            .document
            .dirty
    );
    let back = send(&mut engine, project, Command::Redo).unwrap();
    assert!(!back.document.dirty, "redo returns to the saved revision");
    assert_eq!(back.clips, inserted.clips);
    // Undo, then a different edit: the redo branch is discarded.
    send(&mut engine, project, Command::Undo).unwrap();
    let divergent = send(
        &mut engine,
        project,
        Command::InsertClip {
            media_id: media,
            track_id: video,
        },
    )
    .unwrap();
    assert!(!divergent.history.can_redo);
    assert_ne!(divergent.clips[0].id, inserted.clips[0].id);
    assert_eq!(
        send(&mut engine, project, Command::Redo)
            .unwrap_err()
            .message,
        "Nothing to redo."
    );
    assert!(
        divergent.document.dirty,
        "a different state than the saved one, even at the same clip count"
    );
}

#[test]
fn undo_after_an_unrelated_mutation_takes_a_fresh_revision() {
    let mut engine = Engine::default();
    let (project, media, video, _) = editor(&mut engine, PROBE);
    mark_saved(&mut engine);
    send(
        &mut engine,
        project,
        Command::InsertClip {
            media_id: media,
            track_id: video,
        },
    )
    .unwrap();
    send(
        &mut engine,
        project,
        Command::AddTrack {
            kind: TrackKind::Video,
        },
    )
    .unwrap();
    let undone = send(&mut engine, project, Command::Undo).unwrap();
    assert!(undone.clips.is_empty());
    assert_eq!(
        undone.tracks.len(),
        3,
        "track creation is not undone by a clip undo"
    );
    assert!(
        undone.document.dirty,
        "the added track keeps the document different from the save"
    );
}

#[test]
fn selection_never_dirties_or_enters_history() {
    let mut engine = Engine::default();
    let (project, media, video, _) = editor(&mut engine, PROBE);
    let inserted = send(
        &mut engine,
        project,
        Command::InsertClip {
            media_id: media,
            track_id: video,
        },
    )
    .unwrap();
    mark_saved(&mut engine);
    let clip = inserted.clips[0].id;
    for selection in [
        SelectionState::None,
        SelectionState::Clip(clip),
        SelectionState::Track(video),
    ] {
        let snapshot = send(&mut engine, project, Command::Select { selection }).unwrap();
        assert_eq!(snapshot.selection, selection);
        assert!(!snapshot.document.dirty);
        assert_eq!(snapshot.history.depth, 1);
    }
    let missing = send(
        &mut engine,
        project,
        Command::Select {
            selection: SelectionState::Clip(ClipId::new()),
        },
    );
    assert_eq!(missing.unwrap_err().code, ErrorCode::NotFound);
}

#[test]
fn history_is_bounded_and_reset_by_new_project() {
    let mut engine = Engine::default();
    let (project, media, video, _) = editor(&mut engine, PROBE);
    let mut last = None;
    for _ in 0..MAX_HISTORY + 15 {
        last = Some(
            send(
                &mut engine,
                project,
                Command::InsertClip {
                    media_id: media,
                    track_id: video,
                },
            )
            .unwrap(),
        );
    }
    let last = last.unwrap();
    assert_eq!(last.clips.len(), MAX_HISTORY + 15);
    assert_eq!(last.history.depth, MAX_HISTORY);
    for _ in 0..MAX_HISTORY {
        send(&mut engine, project, Command::Undo).unwrap();
    }
    let floor = engine.snapshot().unwrap();
    assert_eq!(
        floor.clips.len(),
        15,
        "the oldest edits fell off the bounded history"
    );
    assert!(!floor.history.can_undo);
    let (fresh, ..) = editor(&mut engine, PROBE);
    let snapshot = engine.snapshot().unwrap();
    assert_eq!(snapshot.id, fresh);
    assert!(!snapshot.history.can_undo && !snapshot.history.can_redo);
}

#[test]
#[ignore = "performance measurement; run with --ignored --nocapture --release"]
fn measure_command_latency_history_and_snapshot_size() {
    let mut engine = Engine::default();
    let (project, media, video, _) = editor(&mut engine, PROBE);
    let mut placed = 0usize;
    for target in [1_000usize, 10_000, 50_000] {
        while placed < target {
            send(
                &mut engine,
                project,
                Command::InsertClip {
                    media_id: media,
                    track_id: video,
                },
            )
            .unwrap();
            placed += 1;
        }
        let started = std::time::Instant::now();
        let rounds = 20;
        for _ in 0..rounds {
            send(&mut engine, project, Command::Undo).unwrap();
            send(&mut engine, project, Command::Redo).unwrap();
        }
        let undo_redo = started.elapsed() / (2 * rounds);
        let started = std::time::Instant::now();
        let snapshot = send(
            &mut engine,
            project,
            Command::InsertClip {
                media_id: media,
                track_id: video,
            },
        )
        .unwrap();
        let insert = started.elapsed();
        placed += 1;
        let snapshot_bytes = serde_json::to_vec(&snapshot).unwrap().len();
        // Entries hold one Clip value plus a short label; measured, not estimated.
        let entry = std::mem::size_of::<crate::history::Edit>() + "add source video.mkv".len();
        println!(
            "clips={placed} insert={insert:?} undo_or_redo={undo_redo:?} snapshot_json_bytes={snapshot_bytes} history_entries={} history_bytes~={}",
            snapshot.history.depth,
            snapshot.history.depth * entry
        );
    }
}

#[test]
fn timeline_display_overflow_is_rejected_before_owner_position_changes() {
    let mut engine = Engine::default();
    let (project, _, _, _) = editor(&mut engine, PROBE);
    let mut data = serde_json::to_value(engine.project.as_ref().unwrap()).unwrap();
    data["timebase"] = serde_json::json!({"numerator":u32::MAX,"denominator":1});
    engine.project = Some(serde_json::from_value(data).unwrap());
    let revision = engine.document.revision;
    let error = engine
        .evaluate(
            Some(project),
            TimelineSeek::Ticks(TimelineTime(i64::MAX)),
            None,
        )
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidInput);
    assert_eq!(engine.timeline_position, TimelineTime::ZERO);
    assert_eq!(engine.document.revision, revision);
}
