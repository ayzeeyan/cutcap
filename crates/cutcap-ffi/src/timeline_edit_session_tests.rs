//! Phase 3E edits through the session and the C ABI with the controlled
//! cancellable backend: view contract, playhead split, transport
//! invalidation, background save, persistence, relink and argument checks.
use super::*;
use cutcap_contracts::*;

fn id<T: serde::de::DeserializeOwned>(value: &Value) -> T {
    serde_json::from_value(value.clone()).unwrap()
}

/// The view with the separately fetched timeline merged in as "clips".
fn full(session: &mut Session) -> Value {
    let mut v = view(session);
    let timeline: Value = serde_json::from_str(session.timeline_json().unwrap()).unwrap();
    v["clips"] = timeline["clips"].clone();
    v
}

fn wait(session: &mut Session, done: impl Fn(&Value) -> bool) -> Value {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        session.pump();
        let current = full(session);
        if done(&current) {
            return current;
        }
        assert!(Instant::now() < deadline, "timed out; last view {current}");
        thread::sleep(Duration::from_millis(5));
    }
}

fn settled(session: &mut Session, count: usize) -> Value {
    wait(session, |v| {
        v["pending"] == 0 && v["clips"].as_array().is_some_and(|c| c.len() == count)
    })
}

/// One 3 s controlled source placed at 1 s on Video 1. Returns (view, media, track, clip).
fn placed(session: &mut Session) -> (Value, MediaId, TrackId, ClipId) {
    let v = imported(session, "take.mkv");
    let media: MediaId = id(&v["media"][0]["id"]);
    let track: TrackId = id(&v["tracks"][0]["id"]);
    session
        .place_clip(media, track, TimelineSeek::Milliseconds(1000))
        .unwrap();
    let v = settled(session, 1);
    let clip: ClipId = id(&v["clips"][0]["id"]);
    (v, media, track, clip)
}

#[test]
fn clip_rows_carry_exact_ticks_and_source_trim_limits() {
    let mut s = session(Duration::ZERO);
    let (v, _, _, _) = placed(&mut s);
    let clip = &v["clips"][0];
    assert_eq!(
        (clip["start_ticks"].as_i64(), clip["end_ticks"].as_i64()),
        (Some(720_000), Some(2_880_000))
    );
    // The whole source is placed: no extension either way, and never before zero.
    assert_eq!(clip["min_start_ticks"], 720_000);
    assert_eq!(clip["max_end_ticks"], 2_880_000);
    assert_eq!(clip["max_end_ms"], 4000);
    // After shrinking both ends the limits show how far each may grow back.
    let clip_id: ClipId = id(&clip["id"]);
    s.trim_clip(clip_id, ClipEdge::Start, TimelineSeek::Milliseconds(1500))
        .unwrap();
    s.trim_clip(clip_id, ClipEdge::End, TimelineSeek::Milliseconds(3000))
        .unwrap();
    let v = wait(&mut s, |v| {
        v["pending"] == 0 && v["clips"][0]["end_ticks"] == 2_160_000
    });
    let clip = &v["clips"][0];
    assert_eq!(clip["start_ms"], 1500);
    assert_eq!(clip["min_start_ms"], 1000);
    assert_eq!(clip["max_end_ms"], 4000);
    assert_eq!(v["history"]["depth"], 3);
}

#[test]
fn split_follows_the_rust_playhead_and_explains_when_it_cannot() {
    let mut s = session(Duration::ZERO);
    let (_, _, _, clip) = placed(&mut s);
    let none = full(&mut s);
    // The placed clip is selected, but the playhead (0) is before it.
    assert_eq!(none["editing"]["split_enabled"], false);
    assert!(
        none["editing"]["split_detail"]
            .as_str()
            .unwrap()
            .contains("inside the clip")
    );
    s.seek_timeline(TimelineSeek::Milliseconds(2500)).unwrap();
    let ready = wait(&mut s, |v| {
        v["timeline_preview"]["position_ms"] == 2500 && v["editing"]["split_enabled"] == true
    });
    assert!(
        ready["editing"]["split_detail"]
            .as_str()
            .unwrap()
            .contains("Ctrl+B")
    );
    s.split_clip_at_playhead(clip).unwrap();
    let split = settled(&mut s, 2);
    assert_eq!(
        split["clips"][0]["id"],
        clip.to_string(),
        "left part keeps the identity"
    );
    assert_eq!(split["clips"][0]["end_ticks"], 1_800_000);
    assert_eq!(split["clips"][1]["start_ticks"], 1_800_000);
    assert_eq!(split["history"]["undo"], "split take.mkv");
    // The playhead now sits exactly on the cut, which is not inside the left clip.
    assert_eq!(split["editing"]["split_enabled"], false);
    s.undo().unwrap();
    let restored = settled(&mut s, 1);
    assert_eq!(restored["clips"][0]["end_ticks"], 2_880_000);
}

#[test]
fn edits_stop_timeline_playback_without_resuming_and_reject_stale_frames() {
    let mut s = session(Duration::from_millis(2));
    let (_, _, _, clip) = placed(&mut s);
    s.seek_timeline(TimelineSeek::Milliseconds(1200)).unwrap();
    wait(&mut s, |v| {
        v["timeline_preview"]["position_ms"] == 1200 && v["timeline_preview"]["busy"] == false
    });
    s.play_timeline().unwrap();
    let playing = wait(&mut s, |v| {
        v["timeline_playback"]["state"] == "playing" && v["frame"].is_object()
    });
    let old = playing["frame"]["generation"].as_u64().unwrap();
    // Split at the moving playhead: one structural edit stops the transport.
    s.split_clip_at_playhead(clip).unwrap();
    let edited = settled(&mut s, 2);
    assert_eq!(edited["timeline_playback"]["state"], "stopped");
    assert_eq!(
        edited["timeline_playback"]["desired_playing"], false,
        "never auto-resumes"
    );
    assert!(
        !s.copy_allowed(old),
        "frames of the old plan cannot be copied"
    );
    let cut = edited["clips"][1]["start_ticks"].as_i64().unwrap();
    // The playhead is preserved where playback stopped (at or after the cut it made).
    assert!(
        edited["timeline_preview"]["position_ticks"]
            .as_i64()
            .unwrap()
            >= cut
    );
    // Playback continues on the edited timeline when asked.
    s.play_timeline().unwrap();
    wait(&mut s, |v| v["timeline_playback"]["state"] == "playing");
    let moved: ClipId = id(&edited["clips"][1]["id"]);
    let track: TrackId = id(&edited["clips"][1]["track_id"]);
    s.move_clip(
        moved,
        track,
        TimelineSeek::Milliseconds(9000),
        ClipEdge::Start,
    )
    .unwrap();
    let after = wait(&mut s, |v| {
        v["pending"] == 0 && v["clips"][1]["start_ms"] == 9000
    });
    assert_eq!(after["timeline_playback"]["state"], "stopped");
    s.shutdown().unwrap();
}

#[test]
fn an_edit_during_a_background_save_stays_dirty_and_round_trips_exactly() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("edited.cutcap");
    let mut s = session(Duration::ZERO);
    let (_, _, track, clip) = placed(&mut s);
    s.save_project(Some(path.clone())).unwrap();
    s.move_clip(
        clip,
        track,
        TimelineSeek::Milliseconds(5000),
        ClipEdge::Start,
    )
    .unwrap();
    let after = wait(&mut s, |v| {
        v["project"]["document"]["state"] == "saved"
            && v["pending"] == 0
            && v["clips"][0]["start_ms"] == 5000
    });
    assert_eq!(after["project"]["document"]["dirty"], true);
    let document: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(
        document["project"]["clips"][0]["start"], 720_000,
        "file holds the captured revision"
    );
    // Further edits, then save and reopen: identities, positions and source ranges survive.
    s.split_clip(clip, TimelineSeek::Milliseconds(6000))
        .unwrap();
    s.trim_clip(clip, ClipEdge::Start, TimelineSeek::Milliseconds(5500))
        .unwrap();
    let edited = wait(&mut s, |v| {
        v["pending"] == 0
            && v["clips"].as_array().is_some_and(|c| c.len() == 2)
            && v["clips"][0]["start_ms"] == 5500
    });
    let before: Value = serde_json::from_str(s.timeline_json().unwrap()).unwrap();
    saved(&mut s, &path);
    s.new_project().unwrap();
    ready(&mut s);
    s.open_project(path).unwrap();
    let opened = wait(&mut s, |v| {
        v["project"]["document"]["state"] == "opened" && v["pending"] == 0
    });
    for key in [
        "id",
        "track_id",
        "media_id",
        "start_ticks",
        "end_ticks",
        "min_start_ticks",
    ] {
        assert_eq!(opened["clips"][0][key], before["clips"][0][key], "{key}");
        assert_eq!(opened["clips"][1][key], before["clips"][1][key], "{key}");
    }
    assert_eq!(opened["history"]["can_undo"], false);
    assert_eq!(opened["project"]["document"]["dirty"], false);
    let _ = edited;
}

#[test]
fn relinking_a_moved_source_keeps_edited_clips() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("cut source.mkv");
    let moved = directory.path().join("moved.mkv");
    std::fs::write(&source, b"original edited content").unwrap();
    let path = directory.path().join("relink.cutcap");
    let mut s = session(Duration::ZERO);
    ready(&mut s);
    s.import(source.clone()).unwrap();
    let v = pump_until(&mut s, |v| v["frame"].is_object());
    let media: MediaId = id(&v["media"][0]["id"]);
    let track: TrackId = id(&v["tracks"][0]["id"]);
    s.place_clip(media, track, TimelineSeek::Milliseconds(0))
        .unwrap();
    let v = settled(&mut s, 1);
    let clip: ClipId = id(&v["clips"][0]["id"]);
    s.split_clip(clip, TimelineSeek::Milliseconds(1000))
        .unwrap();
    settled(&mut s, 2);
    let edited: Value = serde_json::from_str(s.timeline_json().unwrap()).unwrap();
    saved(&mut s, &path);
    std::fs::rename(&source, &moved).unwrap();
    s.open_project(path).unwrap();
    let missing = wait(&mut s, |v| {
        v["clips"]
            .as_array()
            .is_some_and(|c| c.len() == 2 && c.iter().all(|c| c["availability"] == "missing"))
    });
    // Missing clips stay editable inside their saved range.
    let right: ClipId = id(&missing["clips"][1]["id"]);
    s.trim_clip(right, ClipEdge::End, TimelineSeek::Milliseconds(2500))
        .unwrap();
    wait(&mut s, |v| {
        v["pending"] == 0 && v["clips"][1]["end_ticks"] == 1_800_000
    });
    s.relink_media(media, moved).unwrap();
    let relinked = wait(&mut s, |v| {
        v["clips"][0]["availability"] == "ready" && v["pending"] == 0
    });
    assert_eq!(relinked["clips"][0]["id"], edited["clips"][0]["id"]);
    assert_eq!(relinked["clips"][1]["id"], edited["clips"][1]["id"]);
    assert_eq!(
        relinked["clips"][1]["start_ticks"],
        edited["clips"][1]["start_ticks"]
    );
    assert_eq!(relinked["clips"][1]["end_ticks"], 1_800_000);
}

#[test]
fn rejected_edits_report_status_and_keep_history() {
    let mut s = session(Duration::ZERO);
    let (_, media, track, clip) = placed(&mut s);
    let audio: TrackId = id(&full(&mut s)["tracks"][1]["id"]);
    s.place_clip(media, track, TimelineSeek::Milliseconds(2000))
        .unwrap();
    let overlap = wait(&mut s, |v| {
        v["status"]["error"] == true && v["pending"] == 0
    });
    assert!(
        overlap["status"]["text"]
            .as_str()
            .unwrap()
            .contains("overlap")
    );
    s.move_clip(clip, audio, TimelineSeek::Milliseconds(0), ClipEdge::Start)
        .unwrap();
    let audio_drop = wait(&mut s, |v| {
        v["status"]["text"]
            .as_str()
            .is_some_and(|t| t.contains("video track"))
            && v["pending"] == 0
    });
    assert_eq!(audio_drop["history"]["depth"], 1);
    assert_eq!(audio_drop["clips"].as_array().unwrap().len(), 1);
    // A grid-aligned trim says so in the status line.
    s.trim_clip(
        clip,
        ClipEdge::Start,
        TimelineSeek::Ticks(TimelineTime(720_000 + 1000)),
    )
    .unwrap();
    let aligned = wait(&mut s, |v| {
        v["pending"] == 0 && v["clips"][0]["start_ticks"] == 721_440
    });
    assert!(
        aligned["status"]["text"]
            .as_str()
            .unwrap()
            .contains("source tick"),
        "{}",
        aligned["status"]
    );
    assert_eq!(aligned["editing"]["notice"], aligned["status"]["text"]);
}

#[test]
fn abi_edit_exports_validate_arguments() {
    let handle = register_for_test(session(Duration::ZERO));
    abi_pump_until(handle, |v| v["engine_ready"] == true);
    let good = uuid::Uuid::new_v4().to_string();
    let bad = b"not-a-uuid";
    // SAFETY (test): every pointer is a valid local slice, or null with a nonzero length.
    unsafe {
        assert_eq!(
            cutcap_session_place_clip(
                handle,
                bad.as_ptr(),
                bad.len(),
                good.as_ptr(),
                good.len(),
                0,
                0
            ),
            CUTCAP_INVALID_ARGUMENT
        );
        assert_eq!(
            cutcap_session_place_clip(
                handle,
                good.as_ptr(),
                good.len(),
                good.as_ptr(),
                good.len(),
                0,
                9
            ),
            CUTCAP_INVALID_ARGUMENT
        );
        assert_eq!(
            cutcap_session_move_clip(
                handle,
                good.as_ptr(),
                good.len(),
                good.as_ptr(),
                good.len(),
                0,
                0,
                2
            ),
            CUTCAP_INVALID_ARGUMENT
        );
        assert_eq!(
            cutcap_session_move_clip(
                handle,
                std::ptr::null(),
                36,
                good.as_ptr(),
                good.len(),
                0,
                0,
                0
            ),
            CUTCAP_INVALID_ARGUMENT
        );
        assert_eq!(
            cutcap_session_trim_clip(handle, good.as_ptr(), good.len(), 7, 0, 0),
            CUTCAP_INVALID_ARGUMENT
        );
        assert_eq!(
            cutcap_session_split_clip(handle, good.as_ptr(), good.len(), 0, 5),
            CUTCAP_INVALID_ARGUMENT
        );
        assert_eq!(
            cutcap_session_split_clip_at_playhead(handle, bad.as_ptr(), bad.len()),
            CUTCAP_INVALID_ARGUMENT
        );
        // Well-formed but unknown: accepted, rejected by the owner as a status.
        assert_eq!(
            cutcap_session_split_clip(handle, good.as_ptr(), good.len(), 1000, 1),
            CUTCAP_OK
        );
    }
    let rejected = abi_pump_until(handle, |v| {
        v["status"]["error"] == true && v["pending"] == 0
    });
    assert!(
        rejected["status"]["text"]
            .as_str()
            .unwrap()
            .contains("no longer"),
        "{}",
        rejected["status"]
    );
    assert_eq!(cutcap_session_destroy(handle), CUTCAP_OK);
    assert_eq!(cutcap_abi_version(), ABI_VERSION);
}
