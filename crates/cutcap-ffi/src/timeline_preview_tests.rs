//! Session/real ABI scheduling regressions with the controlled cancellable decoder.
use super::*;
use cutcap_contracts::*;

fn id<T: serde::de::DeserializeOwned>(value: &Value) -> T {
    serde_json::from_value(value.clone()).unwrap()
}
fn two_clips(session: &mut Session) -> (MediaId, MediaId, TrackId, ClipId, ClipId) {
    let first = imported(session, "A.mkv");
    let a = id(&first["media"][0]["id"]);
    let track = id(&first["tracks"][0]["id"]);
    session.insert_clip(a, track).unwrap();
    pump_until(session, |v| v["project"]["clip_count"] == 1);
    session.import(fixture("B.mkv")).unwrap();
    let second = pump_until(session, |v| {
        v["media"].as_array().unwrap().len() == 2
            && v["source"]["title"] == "B.mkv"
            && !v["source"]["busy"].as_bool().unwrap()
    });
    let b = id(&second["media"][1]["id"]);
    session.insert_clip(b, track).unwrap();
    pump_until(session, |v| v["project"]["clip_count"] == 2);
    let clips: Value = serde_json::from_str(session.timeline_json().unwrap()).unwrap();
    (
        a,
        b,
        track,
        id(&clips["clips"][0]["id"]),
        id(&clips["clips"][1]["id"]),
    )
}
fn timeline_at(session: &mut Session, milliseconds: i64) -> Value {
    session
        .seek_timeline(TimelineSeek::Milliseconds(milliseconds))
        .unwrap();
    pump_until(session, |v| {
        v["timeline_preview"]["position_ms"] == milliseconds
            && v["timeline_preview"]["busy"] == false
    })
}

#[test]
fn continuous_transport_rejects_old_epochs_preserves_source_and_stops_on_edits() {
    let mut s = session(Duration::from_millis(2));
    let (a, b, _, first, _) = two_clips(&mut s);
    timeline_at(&mut s, 0);
    s.play_timeline().unwrap();
    let running = pump_until(&mut s, |v| {
        v["timeline_playback"]["state"] == "playing" && v["frame"].is_object()
    });
    let epoch = running["timeline_playback"]["epoch"].clone();
    let old = running["frame"]["generation"].as_u64().unwrap();
    let source = running["source"]["position_ms"].clone();
    s.select_media(a).unwrap();
    s.select_clip(first).unwrap();
    let selected = pump_until(&mut s, |v| v["pending"] == 0);
    assert_eq!(selected["timeline_playback"]["epoch"], epoch);
    assert_eq!(selected["timeline_playback"]["state"], "playing");
    assert_eq!(selected["source"]["position_ms"], source);
    for target in [100, 3500, 300, 4000, 600] {
        s.seek_timeline(TimelineSeek::Milliseconds(target)).unwrap();
        s.pump();
    }
    let latest = pump_until(&mut s, |v| {
        v["timeline_playback"]["state"] == "playing"
            && v["timeline_playback"]["requested_ticks"] == 432000
            && v["frame"].is_object()
    });
    assert_ne!(latest["timeline_playback"]["epoch"], epoch);
    assert!(!s.copy_allowed(old));
    assert_eq!(latest["timeline_preview"]["media_id"], a.to_string());
    s.remove_clip(first).unwrap();
    let removed = pump_until(&mut s, |v| {
        v["project"]["clip_count"] == 1 && v["timeline_preview"]["state"] == "blank"
    });
    assert_eq!(removed["timeline_playback"]["state"], "stopped");
    assert!(s.frame().is_none());
    s.undo().unwrap();
    pump_until(&mut s, |v| {
        v["project"]["clip_count"] == 2 && v["timeline_preview"]["state"] == "ready"
    });
    s.play_timeline().unwrap();
    pump_until(&mut s, |v| v["timeline_playback"]["state"] == "playing");
    s.set_preview_mode(PreviewMode::Source).unwrap();
    assert_eq!(view(&mut s)["timeline_playback"]["state"], "paused");
    s.select_media(b).unwrap();
    pump_until(&mut s, |v| {
        v["pending"] == 0 && v["source"]["busy"] == false
    });
    s.set_preview_mode(PreviewMode::Timeline).unwrap();
    assert!(
        !view(&mut s)["timeline_playback"]["desired_playing"]
            .as_bool()
            .unwrap()
    );
    s.stop_timeline().unwrap();
    assert_eq!(view(&mut s)["timeline_playback"]["clock_ticks"], 0);
    assert_eq!(view(&mut s)["timeline_preview"]["position_ms"], 0);
    assert!(s.frame().is_none()); // command accepted, zero still not decoded yet
    pump_until(&mut s, |v| {
        v["timeline_preview"]["state"] == "ready" && v["timeline_preview"]["position_ms"] == 0
    });
}

#[test]
fn cancelling_a_continuous_seek_retains_its_current_timeline_position() {
    let mut s = session(Duration::from_millis(2));
    two_clips(&mut s);
    timeline_at(&mut s, 500);
    s.play_timeline().unwrap();
    pump_until(&mut s, |v| v["timeline_playback"]["state"] == "playing");
    s.pause_timeline().unwrap();
    s.seek_timeline(TimelineSeek::Milliseconds(1500)).unwrap();
    let before = pump_until(&mut s, |v| {
        v["timeline_playback"]["state"] == "paused" && v["timeline_preview"]["position_ms"] == 1500
    });
    s.cancel_timeline_preview().unwrap();
    let after = view(&mut s);
    assert_eq!(
        after["timeline_preview"]["position_ticks"],
        before["timeline_preview"]["position_ticks"]
    );
    assert_eq!(after["timeline_preview"]["state"], "cancelled");
    assert!(s.frame().is_none());
}

#[test]
fn timeline_transport_exports_validate_handles_mode_and_empty_track() {
    assert_eq!(cutcap_session_play_timeline(0), CUTCAP_INVALID_HANDLE);
    assert_eq!(cutcap_session_pause_timeline(0), CUTCAP_INVALID_HANDLE);
    assert_eq!(cutcap_session_stop_timeline(0), CUTCAP_INVALID_HANDLE);
    let handle = register_for_test(session(Duration::ZERO));
    abi_pump_until(handle, |v| v["engine_ready"] == true);
    assert_eq!(
        cutcap_session_play_timeline(handle),
        CUTCAP_INVALID_ARGUMENT
    );
    assert_eq!(cutcap_session_set_preview_mode(handle, 1), CUTCAP_OK);
    assert_eq!(cutcap_session_play_timeline(handle), CUTCAP_OK);
    abi_pump_until(handle, |v| v["timeline_playback"]["state"] == "ended");
    assert_eq!(cutcap_session_stop_timeline(handle), CUTCAP_OK);
    assert_eq!(cutcap_session_destroy(handle), CUTCAP_OK);
}

#[test]
fn stopping_for_an_edit_updates_all_requested_time_fields_before_evaluation() {
    let mut p = crate::timeline_preview::TimelinePreview::default();
    p.set_position(TimelineTime(720_001), Timebase::DEFAULT_TIMELINE)
        .unwrap();
    assert_eq!(p.view.position_ticks, 720_001);
    assert_eq!(p.view.position_ms, 1000);
    assert_eq!(p.view.position_text, "00:01.000");
    assert_eq!(p.view.position_input, "720001 ticks");
    p.set_position(TimelineTime(1_440_000), Timebase::DEFAULT_TIMELINE)
        .unwrap();
    assert_eq!(p.view.position_input, "00:02.000");
    let milliseconds = Timebase::new(1, 1000).unwrap();
    p.set_position(TimelineTime(1000), milliseconds).unwrap();
    assert_eq!(p.view.position_input, "00:01.000");
    assert_eq!(p.view.timebase, milliseconds);
    assert!(
        p.set_position(TimelineTime(i64::MAX), Timebase::new(u32::MAX, 1).unwrap())
            .is_err()
    );
    assert_eq!(p.view.position_ticks, 1000);
    assert_eq!(p.view.timebase, milliseconds);
}

#[test]
fn exact_cut_gap_mode_copy_permissions_and_transient_dirty_state() {
    let directory = tempfile::tempdir().unwrap();
    let mut session = session(Duration::from_millis(2));
    let (a, b, _, _, _) = two_clips(&mut session);
    saved(&mut session, &directory.path().join("timeline.cutcap"));
    session.seek(1250);
    let source = pump_until(&mut session, |v| v["source"]["busy"] == false);
    let source_generation = source["frame"]["generation"].as_u64().unwrap();
    let before_generation = source["timeline_generation"].clone();
    let first = timeline_at(&mut session, 500);
    assert_eq!(first["preview_mode"], "timeline");
    assert_eq!(first["timeline_preview"]["media_id"], a.to_string());
    assert_eq!(first["timeline_preview"]["actual"]["ticks"], 500);
    assert_eq!(first["timeline_generation"], before_generation);
    assert!(!session.copy_allowed(source_generation));
    let cut = timeline_at(&mut session, 3000);
    assert_eq!(cut["timeline_preview"]["media_id"], b.to_string());
    assert_eq!(cut["timeline_preview"]["actual"]["ticks"], 0);
    let timeline_generation = cut["frame"]["generation"].as_u64().unwrap();
    assert!(timeline_generation > source_generation);
    assert_eq!(
        timeline_at(&mut session, 6000)["timeline_preview"]["state"],
        "blank"
    );
    assert!(session.frame().is_none());
    assert_eq!(view(&mut session)["project"]["document"]["dirty"], false);
    assert!(session.play().is_err());
    session.set_preview_mode(PreviewMode::Source).unwrap();
    let restored = view(&mut session);
    assert_eq!(restored["source"]["position_ms"], 1250);
    assert_eq!(restored["frame"]["actual"]["ticks"], 1250);
    assert!(restored["frame"]["generation"].as_u64().unwrap() > timeline_generation);
    assert_eq!(restored["timeline_preview"]["position_ms"], 6000);
    assert!(!session.copy_allowed(timeline_generation));
    assert!(
        session
            .seek_timeline(TimelineSeek::Ticks(TimelineTime(-1)))
            .is_err()
    );
    assert_eq!(view(&mut session)["preview_mode"], "source");
}

#[test]
fn latest_seeks_removal_undo_track_changes_and_open_invalidate_old_frames() {
    let directory = tempfile::tempdir().unwrap();
    let mut session = session(Duration::from_millis(30));
    let (a, b, track, first, _) = two_clips(&mut session);
    for ms in [100, 900, 2500, 3000, 3500] {
        session
            .seek_timeline(TimelineSeek::Milliseconds(ms))
            .unwrap();
        session.pump();
    }
    let latest = pump_until(&mut session, |v| v["timeline_preview"]["state"] == "ready");
    assert_eq!(latest["timeline_preview"]["media_id"], b.to_string());
    assert_eq!(latest["timeline_preview"]["actual"]["ticks"], 500);
    timeline_at(&mut session, 500);
    session.remove_clip(first).unwrap();
    assert!(session.frame().is_none()); // invalidated at command acceptance
    let blank = pump_until(&mut session, |v| {
        v["project"]["clip_count"] == 1 && v["timeline_preview"]["state"] == "blank"
    });
    assert!(blank["frame"].is_null());
    session.undo().unwrap();
    let restored = pump_until(&mut session, |v| {
        v["project"]["clip_count"] == 2 && v["timeline_preview"]["state"] == "ready"
    });
    assert_eq!(restored["timeline_preview"]["media_id"], a.to_string());
    session.add_track(TrackKind::Video).unwrap();
    let empty = pump_until(&mut session, |v| {
        v["timeline_preview"]["track_name"] == "Video 2"
            && v["timeline_preview"]["state"] == "blank"
    });
    assert!(empty["frame"].is_null());
    session.select_track(track).unwrap();
    pump_until(&mut session, |v| {
        v["timeline_preview"]["track_name"] == "Video 1"
            && v["timeline_preview"]["state"] == "ready"
    });
    let path = directory.path().join("project.cutcap");
    saved(&mut session, &path);
    session.open_project(path).unwrap();
    let opened = pump_until(&mut session, |v| {
        v["project"]["document"]["state"] == "opened" && v["timeline_preview"]["state"] == "ready"
    });
    assert_eq!(opened["timeline_preview"]["position_ticks"], 0);
    assert_eq!(opened["timeline_preview"]["media_id"], a.to_string());
    assert_eq!(opened["project"]["document"]["dirty"], false);
}

#[test]
fn mode_change_during_decode_and_pause_on_leaving_source_are_safe() {
    let mut session = session(Duration::from_millis(40));
    two_clips(&mut session);
    session.play().unwrap();
    pump_until(&mut session, |v| v["playback"]["state"] == "playing");
    session
        .seek_timeline(TimelineSeek::Milliseconds(800))
        .unwrap();
    pump_until(&mut session, |v| {
        v["timeline_preview"]["state"] == "loading"
    });
    session.set_preview_mode(PreviewMode::Source).unwrap();
    let restored = view(&mut session);
    assert_eq!(restored["playback"]["state"], "paused");
    let generation = restored["frame"]["generation"].clone();
    thread::sleep(Duration::from_millis(70));
    session.pump();
    assert_eq!(view(&mut session)["frame"]["generation"], generation);
    session.play().unwrap();
    assert_eq!(view(&mut session)["playback"]["state"], "playing");
    let before = Instant::now();
    session.shutdown().unwrap();
    assert!(before.elapsed() < Duration::from_secs(2));
}

#[test]
fn abi_modes_timestamps_text_and_old_frame_copies_are_validated() {
    let mut session = session(Duration::ZERO);
    two_clips(&mut session);
    let old = session.frame().unwrap().generation;
    let handle = register_for_test(session);
    assert_eq!(
        cutcap_session_set_preview_mode(handle, 2),
        CUTCAP_INVALID_ARGUMENT
    );
    assert_eq!(
        cutcap_session_seek_timeline(handle, -1),
        CUTCAP_INVALID_ARGUMENT
    );
    // SAFETY: null input must be rejected without dereferencing it.
    assert_eq!(
        unsafe { cutcap_session_seek_timeline_text(handle, std::ptr::null(), 10) },
        CUTCAP_INVALID_ARGUMENT
    );
    let text = b"2160000 ticks";
    // SAFETY: valid readable local text slice.
    assert_eq!(
        unsafe { cutcap_session_seek_timeline_text(handle, text.as_ptr(), text.len()) },
        CUTCAP_OK
    );
    abi_pump_until(handle, |v| v["timeline_preview"]["state"] == "ready");
    let mut pixels = [0u8; 32];
    // SAFETY: valid writable local buffer; the old source generation is stale.
    assert_eq!(
        unsafe { cutcap_session_copy_frame(handle, old, pixels.as_mut_ptr(), pixels.len(), 16) },
        CUTCAP_STALE_FRAME
    );
    assert_eq!(cutcap_session_destroy(handle), CUTCAP_OK);
}
