//! Phase 3B timeline editing through the session and the C ABI: the view
//! contract, persistence round trips, background saves and playback
//! independence (controlled backend, no FFmpeg).
use super::*;
use cutcap_contracts::{ClipId, MediaId, TrackId};

fn id<T: serde::de::DeserializeOwned>(value: &Value) -> T {
    serde_json::from_value(value.clone()).unwrap()
}

/// Imports one 3 s source and returns (view, media id, first video track id).
fn editable(session: &mut Session, path: &str) -> (Value, MediaId, TrackId) {
    let view = imported(session, path);
    let media = id(&view["media"][0]["id"]);
    let track = id(&view["tracks"][0]["id"]);
    (view, media, track)
}

fn clips(view: &Value) -> &Vec<Value> {
    view["clips"].as_array().unwrap()
}

fn with_clips(session: &mut Session, count: usize) -> Value {
    wait(session, |v| {
        v["clips"].as_array().is_some_and(|c| c.len() == count) && v["pending"] == 0
    })
}

/// The session view with the separately fetched timeline merged in as "clips".
fn full(session: &mut Session) -> Value {
    let mut view = view(session);
    let timeline: Value = serde_json::from_str(session.timeline_json().unwrap()).unwrap();
    assert_eq!(timeline["generation"], view["timeline_generation"]);
    view["clips"] = timeline["clips"].clone();
    view
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

fn abi_timeline(handle: u64) -> Value {
    let mut len = 0usize;
    // SAFETY (test): null buffer with zero capacity; `len` is a valid local.
    let status = unsafe { cutcap_session_timeline(handle, std::ptr::null_mut(), 0, &mut len) };
    assert_eq!(status, CUTCAP_BUFFER_TOO_SMALL);
    let mut buffer = vec![0u8; len];
    // SAFETY (test): `buffer` holds `len` writable bytes.
    let status = unsafe { cutcap_session_timeline(handle, buffer.as_mut_ptr(), len, &mut len) };
    assert_eq!(status, CUTCAP_OK);
    serde_json::from_slice(&buffer[..len]).unwrap()
}

fn abi_wait(handle: u64, done: impl Fn(&Value) -> bool) -> Value {
    abi_pump_until(handle, |v| {
        let mut merged = v.clone();
        merged["clips"] = abi_timeline(handle)["clips"].clone();
        done(&merged)
    });
    let mut view = abi_view(handle);
    let timeline = abi_timeline(handle);
    assert_eq!(timeline["generation"], view["timeline_generation"]);
    view["clips"] = timeline["clips"].clone();
    view
}

#[test]
fn inserted_clips_are_presented_with_exact_placement_and_a_clip_inspector() {
    let mut session = session(Duration::ZERO);
    let (_, media, track) = editable(&mut session, "dawn take.mkv");
    session.insert_clip(media, track).unwrap();
    session.insert_clip(media, track).unwrap();
    let view = with_clips(&mut session, 2);
    let [first, second] = [&clips(&view)[0], &clips(&view)[1]];
    assert_eq!(first["name"], "dawn take.mkv");
    assert_eq!(first["track_index"], 0);
    assert_eq!(
        (first["start_ms"].as_i64(), first["duration_ms"].as_i64()),
        (Some(0), Some(3000))
    );
    assert_eq!(second["start_ms"], 3000, "appended at the end of the track");
    assert_eq!(second["end_text"], "00:06.000");
    assert_eq!(first["availability"], "ready");
    assert_eq!(
        (first["selected"].as_bool(), second["selected"].as_bool()),
        (Some(false), Some(true))
    );
    assert_eq!(view["inspector"]["kind"], "clip");
    assert_eq!(view["inspector"]["title"], "dawn take.mkv");
    assert_eq!(view["history"]["can_undo"], true);
    assert_eq!(view["history"]["undo"], "add dawn take.mkv");
    assert_eq!(
        view["tracks"][0]["lane_label"], "",
        "clips replace the empty-lane label"
    );
    let ticks = view["inspector"]["sections"][0]["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["label"] == "Ticks")
        .unwrap()["value"]
        .clone();
    assert_eq!(ticks, "2160000 + 2160000 @ 1/720000 s");
    // Clip selection is transient and leaves the document as it was.
    let saved_path = tempfile::tempdir().unwrap();
    saved(&mut session, &saved_path.path().join("p.cutcap"));
    session.select_clip(id(&first["id"])).unwrap();
    let selected = wait(&mut session, |v| v["clips"][0]["selected"] == true);
    assert_eq!(selected["project"]["document"]["dirty"], false);
    assert_eq!(selected["history"]["depth"], 2);
}

#[test]
fn save_reopen_round_trips_clips_and_resets_history() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("edit.cutcap");
    let mut session = session(Duration::ZERO);
    let (_, media, track) = editable(&mut session, "take.mkv");
    for _ in 0..3 {
        session.insert_clip(media, track).unwrap();
    }
    let before = with_clips(&mut session, 3);
    saved(&mut session, &path);
    session.new_project().unwrap();
    ready(&mut session);
    let empty = full(&mut session);
    assert!(clips(&empty).is_empty());
    assert_eq!(empty["history"]["can_undo"], false);
    session.open_project(path).unwrap();
    let loaded = wait(&mut session, |v| {
        v["project"]["document"]["state"] == "opened" && v["pending"] == 0
    });
    assert_eq!(clips(&loaded).len(), 3);
    for (a, b) in clips(&before).iter().zip(clips(&loaded)) {
        for key in [
            "id",
            "track_id",
            "media_id",
            "start_ms",
            "duration_ms",
            "name",
        ] {
            assert_eq!(a[key], b[key], "{key}");
        }
        assert_eq!(b["selected"], false, "selection is not persisted");
    }
    assert_eq!(
        loaded["history"]["can_undo"], false,
        "history resets on open"
    );
    assert_eq!(loaded["project"]["document"]["dirty"], false);
}

#[test]
fn reopening_with_missing_media_keeps_clips_editable_but_refuses_new_placement() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("gone.mkv");
    std::fs::write(&source, b"source content").unwrap();
    let path = directory.path().join("missing.cutcap");
    let mut session = session(Duration::ZERO);
    ready(&mut session);
    session.import(source.clone()).unwrap();
    let imported = wait(&mut session, |v| v["frame"].is_object());
    let media: MediaId = id(&imported["media"][0]["id"]);
    let track: TrackId = id(&imported["tracks"][0]["id"]);
    session.insert_clip(media, track).unwrap();
    let placed = with_clips(&mut session, 1);
    saved(&mut session, &path);
    std::fs::remove_file(&source).unwrap();
    session.open_project(path).unwrap();
    let missing = wait(&mut session, |v| v["clips"][0]["availability"] == "missing");
    assert_eq!(missing["clips"][0]["id"], placed["clips"][0]["id"]);
    assert_eq!(missing["clips"][0]["start_ms"], 0);
    assert_eq!(missing["clips"][0]["duration_ms"], 3000);
    session.insert_clip(media, track).unwrap();
    let refused = wait(&mut session, |v| {
        v["status"]["error"] == true && v["pending"] == 0
    });
    assert!(
        refused["status"]["text"]
            .as_str()
            .unwrap()
            .contains("ready source")
    );
    assert_eq!(clips(&refused).len(), 1);
    assert_eq!(
        refused["project"]["document"]["dirty"], false,
        "a failed edit keeps the saved state"
    );
    // Existing clips of a missing source can still be removed and restored.
    session.remove_clip(id(&missing["clips"][0]["id"])).unwrap();
    let removed = with_clips(&mut session, 0);
    assert_eq!(
        removed["media"].as_array().unwrap().len(),
        1,
        "the media asset stays"
    );
    assert_eq!(removed["project"]["document"]["dirty"], true);
    session.undo().unwrap();
    let restored = with_clips(&mut session, 1);
    assert_eq!(restored["clips"][0]["id"], placed["clips"][0]["id"]);
    assert_eq!(restored["project"]["document"]["dirty"], false);
}

#[test]
fn an_edit_during_a_background_save_is_kept_and_leaves_the_document_dirty() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("racing.cutcap");
    let mut session = session(Duration::ZERO);
    let (_, media, track) = editable(&mut session, "take.mkv");
    session.save_project(Some(path.clone())).unwrap();
    // Queued behind the save: the save has already captured its revision.
    session.insert_clip(media, track).unwrap();
    let after = wait(&mut session, |v| {
        v["project"]["document"]["state"] == "saved"
            && v["pending"] == 0
            && !v["clips"].as_array().unwrap().is_empty()
    });
    assert_eq!(after["project"]["document"]["dirty"], true);
    let document: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(
        document["project"]["clips"].as_array().map(Vec::len),
        Some(0),
        "file holds the captured revision"
    );
    // Undo returns to the saved revision exactly.
    session.undo().unwrap();
    let undone = with_clips(&mut session, 0);
    assert_eq!(undone["project"]["document"]["dirty"], false);
    session.redo().unwrap();
    assert_eq!(
        with_clips(&mut session, 1)["project"]["document"]["dirty"],
        true
    );
}

#[test]
fn failed_edits_report_errors_without_history_or_dirtying() {
    let directory = tempfile::tempdir().unwrap();
    let mut session = session(Duration::ZERO);
    let (view, media, _) = editable(&mut session, "take.mkv");
    let audio: TrackId = id(&view["tracks"][1]["id"]);
    saved(&mut session, &directory.path().join("p.cutcap"));
    session.insert_clip(media, audio).unwrap();
    let failed = wait(&mut session, |v| {
        v["status"]["error"] == true && v["pending"] == 0
    });
    assert!(
        failed["status"]["text"]
            .as_str()
            .unwrap()
            .contains("video track")
    );
    session.remove_clip(ClipId::new()).unwrap();
    session.undo().unwrap();
    let after = wait(&mut session, |v| v["pending"] == 0);
    assert!(clips(&after).is_empty());
    assert_eq!(after["history"]["depth"], 0);
    assert_eq!(after["project"]["document"]["dirty"], false);
}

#[test]
fn editing_does_not_disturb_source_playback() {
    let mut session = session(Duration::from_millis(2));
    let (_, media, track) = editable(&mut session, r"C:\fixture.mkv");
    session.play().unwrap();
    let playing = wait(&mut session, |v| {
        v["playback"]["offered"].as_u64().is_some_and(|n| n > 0)
    });
    let epoch = playing["playback"]["epoch"].clone();
    session.insert_clip(media, track).unwrap();
    let edited = with_clips(&mut session, 1);
    assert_eq!(edited["playback"]["epoch"], epoch, "no restart, no seek");
    assert_eq!(edited["playback"]["state"], playing["playback"]["state"]);
    session.undo().unwrap();
    let undone = with_clips(&mut session, 0);
    assert_eq!(undone["playback"]["epoch"], epoch);
    assert_eq!(undone["source"]["ready"], true);
    session.shutdown().unwrap();
}

#[test]
#[ignore = "performance measurement; run with --ignored --nocapture"]
fn measure_large_timelines() {
    let mut session = session(Duration::ZERO);
    let (_, media, track) = editable(&mut session, "take.mkv");
    for count in [100usize, 1_000, 5_000] {
        let started = Instant::now();
        let mut submitted = clips(&full(&mut session)).len();
        while submitted < count {
            // Stay well inside the bounded command queue.
            for _ in 0..16.min(count - submitted) {
                session.insert_clip(media, track).unwrap();
                submitted += 1;
            }
            pump_until(&mut session, |v| v["pending"] == 0);
        }
        let filled = started.elapsed();
        let started = Instant::now();
        session.undo().unwrap();
        let undone = with_clips(&mut session, count - 1);
        let undo = started.elapsed();
        session.redo().unwrap();
        // View only, so the timeline document is still unbuilt for this snapshot.
        pump_until(&mut session, |v| {
            v["pending"] == 0 && v["history"]["can_redo"] == false
        });
        let started = Instant::now();
        let timeline = session.timeline_json().unwrap().len();
        let timeline_cost = started.elapsed();
        // The session view is re-serialized for every change, including each
        // playback frame; it no longer carries clips.
        let started = Instant::now();
        let view_bytes = serde_json::to_string(&session.view()).unwrap().len();
        let view_cost = started.elapsed();
        println!(
            "clips={count} fill={filled:?} undo_round_trip={undo:?} timeline_bytes={timeline} timeline_build={timeline_cost:?} view_bytes={view_bytes} view_build={view_cost:?} depth={}",
            undone["history"]["depth"]
        );
    }
}

// ---- ABI ------------------------------------------------------------------

#[test]
fn abi_editing_exports_validate_arguments_and_round_trip() {
    let handle = register_for_test(session(Duration::ZERO));
    abi_wait(handle, |v| v["engine_ready"] == true);
    let path = fixture("abi edit.mkv");
    let wide: Vec<u16> = path.to_string_lossy().encode_utf16().collect();
    // SAFETY (test): valid local UTF-16 slice.
    assert_eq!(
        unsafe { cutcap_session_import(handle, wide.as_ptr(), wide.len()) },
        CUTCAP_OK
    );
    let view = abi_wait(handle, |v| {
        v["frame"].is_object() && v["source"]["busy"] == false
    });
    let media = view["media"][0]["id"].as_str().unwrap().to_owned();
    let track = view["tracks"][0]["id"].as_str().unwrap().to_owned();
    let bad = b"not-a-uuid";
    // SAFETY (test): every pointer below is a valid local slice, or null with a nonzero length.
    unsafe {
        assert_eq!(
            cutcap_session_insert_clip(
                handle,
                bad.as_ptr(),
                bad.len(),
                track.as_ptr(),
                track.len()
            ),
            CUTCAP_INVALID_ARGUMENT
        );
        assert_eq!(
            cutcap_session_insert_clip(handle, media.as_ptr(), media.len(), std::ptr::null(), 36),
            CUTCAP_INVALID_ARGUMENT
        );
        assert_eq!(
            cutcap_session_remove_clip(handle, bad.as_ptr(), bad.len()),
            CUTCAP_INVALID_ARGUMENT
        );
        assert_eq!(
            cutcap_session_select_clip(handle, std::ptr::null(), 36),
            CUTCAP_INVALID_ARGUMENT
        );
        assert_eq!(
            cutcap_session_insert_clip(
                handle,
                media.as_ptr(),
                media.len(),
                track.as_ptr(),
                track.len()
            ),
            CUTCAP_OK
        );
    }
    let inserted = abi_wait(handle, |v| {
        v["clips"].as_array().is_some_and(|c| c.len() == 1)
    });
    let clip = inserted["clips"][0]["id"].as_str().unwrap().to_owned();
    // SAFETY (test): valid local slices.
    unsafe {
        assert_eq!(
            cutcap_session_remove_clip(handle, clip.as_ptr(), clip.len()),
            CUTCAP_OK
        );
    }
    abi_wait(handle, |v| {
        v["clips"].as_array().is_some_and(Vec::is_empty) && v["history"]["can_redo"] == false
    });
    assert_eq!(cutcap_session_undo(handle), CUTCAP_OK);
    let restored = abi_wait(handle, |v| {
        v["clips"].as_array().is_some_and(|c| c.len() == 1)
    });
    assert_eq!(restored["clips"][0]["id"], clip);
    assert_eq!(restored["history"]["can_redo"], true);
    assert_eq!(cutcap_session_redo(handle), CUTCAP_OK);
    abi_wait(handle, |v| v["clips"].as_array().is_some_and(Vec::is_empty));
    assert_eq!(
        cutcap_session_redo(handle),
        CUTCAP_OK,
        "engine rejections arrive as view status"
    );
    let rejected = abi_wait(handle, |v| v["status"]["text"] == "Nothing to redo.");
    assert_eq!(rejected["status"]["error"], true);
    assert_eq!(cutcap_session_undo(u64::MAX), CUTCAP_INVALID_HANDLE);
    assert_eq!(cutcap_session_destroy(handle), CUTCAP_OK);
}

/// Writes a real-FFmpeg project with many clips for native draw-time checks:
/// `CUTCAP_LARGE_PROJECT=<dir> cargo test -p cutcap-ffi --release generate_large -- --ignored`.
#[test]
#[ignore = "needs FFmpeg; generates a measurement project"]
fn generate_large_timeline_project() {
    let Ok(directory) = std::env::var("CUTCAP_LARGE_PROJECT") else {
        return;
    };
    let directory = Path::new(&directory);
    std::fs::create_dir_all(directory).unwrap();
    let source = directory.join("large source.mkv");
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/colors-cfr.mkv"),
        &source,
    )
    .unwrap();
    let mut session = Session::start().unwrap();
    ready(&mut session);
    session.import(source).unwrap();
    let imported = pump_until(&mut session, |v| v["media"][0]["availability"] == "ready");
    let media = id(&imported["media"][0]["id"]);
    let tracks: Vec<TrackId> = vec![id(&imported["tracks"][0]["id"])];
    let count = 5_000;
    let mut submitted = 0;
    while submitted < count {
        for _ in 0..16.min(count - submitted) {
            session.insert_clip(media, tracks[0]).unwrap();
            submitted += 1;
        }
        pump_until(&mut session, |v| v["pending"] == 0);
    }
    let started = Instant::now();
    saved(&mut session, &directory.join("large timeline.cutcap"));
    println!("saved {count} clips in {:?}", started.elapsed());
    session.shutdown().unwrap();
}
