//! Phase 4C through the session: gain previews and commits, controls that do
//! not disturb the transport, Unlink affordances and the view contract.
use super::*;
use cutcap_contracts::*;

fn id<T: serde::de::DeserializeOwned>(value: &Value) -> T {
    serde_json::from_value(value.clone()).unwrap()
}

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

/// A linked pair at 0 on Video 1 / Audio 1. Returns (view, audio track, video clip, audio clip).
fn linked(session: &mut Session) -> (Value, TrackId, ClipId, ClipId) {
    let v = imported(session, "av take.mkv");
    let media: MediaId = id(&v["media"][0]["id"]);
    let track: TrackId = id(&v["tracks"][0]["id"]);
    session
        .place_clip(media, track, TimelineSeek::Milliseconds(0))
        .unwrap();
    let v = wait(session, |v| {
        v["pending"] == 0 && v["clips"].as_array().is_some_and(|c| c.len() == 2)
    });
    let clip = |kind: &str| {
        v["clips"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["stream"] == kind)
            .unwrap()["id"]
            .clone()
    };
    let (video, audio) = (id(&clip("video")), id(&clip("audio")));
    let track: TrackId = id(&v["tracks"][1]["id"]);
    (v, track, video, audio)
}

fn settled(session: &mut Session) -> Value {
    wait(session, |v| v["pending"] == 0)
}

#[test]
fn a_volume_drag_is_live_transient_and_one_undo_step_when_it_ends() {
    let mut s = session(Duration::ZERO);
    let (v, track, _, _) = linked(&mut s);
    let depth = v["history"]["depth"].as_u64().unwrap();
    let revision = v["project"]["document"]["revision"].clone();
    assert_eq!(v["tracks"][1]["gain_text"], "0.0 dB");
    // Many previews: the view follows, nothing enters history or the document.
    for cdb in [-100, -250, -400, -600] {
        s.preview_track_gain(track, cdb).unwrap();
    }
    let v = settled(&mut s);
    assert_eq!(v["tracks"][1]["gain_cdb"], -600);
    assert_eq!(v["tracks"][1]["gain_text"], "\u{2212}6.0 dB");
    assert_eq!(v["history"]["depth"].as_u64().unwrap(), depth);
    assert_eq!(v["project"]["document"]["revision"], revision);
    // The commit is one entry.
    s.commit_track_gain(track, -600).unwrap();
    let v = wait(&mut s, |v| {
        v["pending"] == 0 && v["history"]["depth"].as_u64() == Some(depth + 1)
    });
    assert_eq!(v["tracks"][1]["gain_cdb"], -600);
    assert!(v["history"]["undo"].as_str().unwrap().contains("volume"));
    assert_eq!(v["history"]["undo_track"], true);
    s.undo().unwrap();
    let v = wait(&mut s, |v| v["tracks"][1]["gain_cdb"] == 0);
    assert_eq!(v["history"]["depth"].as_u64().unwrap(), depth);
    // A cancelled drag restores the stored value.
    s.preview_track_gain(track, -3000).unwrap();
    assert_eq!(settled(&mut s)["tracks"][1]["gain_cdb"], -3000);
    s.cancel_track_gain().unwrap();
    assert_eq!(settled(&mut s)["tracks"][1]["gain_cdb"], 0);
    // Committing the stored value records nothing.
    s.preview_track_gain(track, -100).unwrap();
    s.commit_track_gain(track, 0).unwrap();
    let v = settled(&mut s);
    assert_eq!(v["history"]["depth"].as_u64().unwrap(), depth);
    assert_eq!(v["tracks"][1]["gain_cdb"], 0);
    // Out of range and video tracks are refused before anything happens.
    assert!(s.preview_track_gain(track, 601).is_err());
    let video: TrackId = id(&v["tracks"][0]["id"]);
    assert!(s.preview_track_gain(video, -100).is_err());
}

#[test]
fn mute_and_solo_are_reflected_with_the_documented_precedence() {
    let mut s = session(Duration::ZERO);
    let (_, a, _, _) = linked(&mut s);
    s.add_track(TrackKind::Audio).unwrap();
    let v = wait(&mut s, |v| {
        v["tracks"].as_array().unwrap().len() == 3 && v["pending"] == 0
    });
    let b: TrackId = id(&v["tracks"][2]["id"]);
    assert_eq!(v["tracks"][1]["audible"], true);
    assert_eq!(
        v["tracks"][0]["audible"], false,
        "video tracks are not in the mix"
    );
    s.set_track_mute(a, true).unwrap();
    let v = wait(&mut s, |v| v["tracks"][1]["muted"] == true);
    assert_eq!(v["tracks"][1]["audible"], false);
    assert_eq!(v["tracks"][2]["audible"], true);
    s.set_track_solo(a, true).unwrap();
    let v = wait(&mut s, |v| v["tracks"][1]["soloed"] == true);
    assert_eq!(v["tracks"][1]["audible"], false, "mute wins over solo");
    assert_eq!(
        v["tracks"][2]["audible"], false,
        "a solo elsewhere silences it"
    );
    s.set_track_solo(b, true).unwrap();
    let v = wait(&mut s, |v| v["tracks"][2]["soloed"] == true);
    assert_eq!(v["tracks"][2]["audible"], true);
    s.undo().unwrap();
    let v = wait(&mut s, |v| v["tracks"][2]["soloed"] == false);
    assert_eq!(v["tracks"][2]["audible"], false);
}

#[test]
fn track_controls_never_restart_the_timeline_transport_but_edits_do() {
    let mut s = session(Duration::ZERO);
    let (_, track, _, audio) = linked(&mut s);
    s.set_preview_mode(PreviewMode::Timeline).unwrap();
    s.play_timeline().unwrap();
    let v = wait(&mut s, |v| v["timeline_playback"]["state"] == "playing");
    let epoch = v["timeline_playback"]["epoch"].clone();
    s.set_track_mute(track, true).unwrap();
    s.preview_track_gain(track, -300).unwrap();
    s.commit_track_gain(track, -300).unwrap();
    let v = wait(&mut s, |v| {
        v["pending"] == 0 && v["tracks"][1]["muted"] == true
    });
    assert_eq!(v["timeline_playback"]["epoch"], epoch);
    assert_eq!(v["timeline_playback"]["state"], "playing");
    s.undo().unwrap();
    let v = wait(&mut s, |v| {
        v["pending"] == 0 && v["tracks"][1]["gain_cdb"] == 0
    });
    assert_eq!(
        v["timeline_playback"]["epoch"], epoch,
        "undo of a control keeps playing"
    );
    assert_eq!(v["timeline_playback"]["state"], "playing");
    // A structural edit ends the epoch, as before.
    s.unlink_clip(audio).unwrap();
    let v = wait(&mut s, |v| {
        v["pending"] == 0 && v["timeline_playback"]["epoch"] != epoch
    });
    assert_ne!(v["timeline_playback"]["state"], "playing");
}

#[test]
fn unlink_and_relink_are_offered_from_the_engine_state() {
    let mut s = session(Duration::ZERO);
    let (_, _, video, audio) = linked(&mut s);
    s.select_clip(audio).unwrap();
    let v = wait(&mut s, |v| {
        v["pending"] == 0 && v["editing"]["can_unlink"] == true
    });
    assert_eq!(v["inspector"]["badge"], "Audio");
    assert!(
        v["inspector"]["detail"]
            .as_str()
            .unwrap()
            .contains("Audio clip on Audio 1")
    );
    let rows: Vec<String> = v["inspector"]["sections"][1]["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            format!(
                "{}={}",
                r["label"].as_str().unwrap(),
                r["value"].as_str().unwrap()
            )
        })
        .collect();
    assert!(rows.iter().any(|r| r.starts_with("Stream=#")), "{rows:?}");
    assert!(
        rows.iter().any(|r| r.starts_with("Sample rate=")),
        "{rows:?}"
    );
    assert!(
        rows.iter().any(|r| r.starts_with("Source interval=")),
        "{rows:?}"
    );
    assert!(rows.iter().any(|r| r == "Link=Linked to video"), "{rows:?}");
    s.unlink_clip(audio).unwrap();
    let v = wait(&mut s, |v| {
        v["pending"] == 0 && v["editing"]["can_unlink"] == false
    });
    assert_eq!(v["editing"]["link_candidate"], video.to_string());
    s.link_clips(audio, video).unwrap();
    let v = wait(&mut s, |v| {
        v["pending"] == 0 && v["editing"]["can_unlink"] == true
    });
    assert!(v["editing"]["link_candidate"].is_null());
}

#[test]
fn repeated_solo_toggles_land_on_the_last_state_without_disturbing_playback() {
    let mut s = session(Duration::ZERO);
    let (_, first, _, _) = linked(&mut s);
    s.add_track(TrackKind::Audio).unwrap();
    let v = wait(&mut s, |v| {
        v["pending"] == 0 && v["tracks"].as_array().is_some_and(|t| t.len() == 3)
    });
    let depth = v["history"]["depth"].as_u64().unwrap();
    s.set_preview_mode(PreviewMode::Timeline).unwrap();
    s.play_timeline().unwrap();
    let v = wait(&mut s, |v| v["timeline_playback"]["state"] == "playing");
    let epoch = v["timeline_playback"]["epoch"].clone();
    // Nine toggles sent as fast as a user could ever press: on, off, ... on.
    for press in 0..9 {
        s.set_track_solo(first, press % 2 == 0).unwrap();
    }
    let v = wait(&mut s, |v| {
        v["pending"] == 0 && v["history"]["depth"].as_u64() == Some(depth + 9)
    });
    assert_eq!(v["tracks"][1]["soloed"], true);
    assert_eq!(v["tracks"][1]["audible"], true);
    assert_eq!(v["tracks"][2]["audible"], false);
    assert_eq!(v["timeline_playback"]["epoch"], epoch);
    assert_eq!(v["timeline_playback"]["state"], "playing");
    // One more toggle clears it, and every track is audible again.
    s.set_track_solo(first, false).unwrap();
    let v = wait(&mut s, |v| {
        v["pending"] == 0 && v["tracks"][1]["soloed"] == false
    });
    assert_eq!(v["tracks"][2]["audible"], true);
    // Each press was its own step, and undoing them replays the states in reverse.
    for step in (0..10).rev() {
        s.undo().unwrap();
        let expected = step % 2 == 1;
        let v = wait(&mut s, |v| {
            v["pending"] == 0 && v["history"]["depth"].as_u64() == Some(depth + step)
        });
        assert_eq!(v["tracks"][1]["soloed"], expected, "after undo to {step}");
        assert_eq!(v["timeline_playback"]["epoch"], epoch);
    }
}

#[test]
fn replacing_the_project_while_mixing_ends_the_mix_and_keeps_no_controls() {
    let mut s = session(Duration::ZERO);
    let (_, track, _, _) = linked(&mut s);
    let old_project = view(&mut s)["project"]["id"].clone();
    s.set_track_mute(track, true).unwrap();
    wait(&mut s, |v| {
        v["pending"] == 0 && v["tracks"][1]["muted"] == true
    });
    s.set_preview_mode(PreviewMode::Timeline).unwrap();
    s.play_timeline().unwrap();
    wait(&mut s, |v| v["timeline_playback"]["state"] == "playing");
    // A drag is still in progress when the project goes away.
    s.preview_track_gain(track, -900).unwrap();
    s.new_project().unwrap();
    let v = wait(&mut s, |v| {
        v["pending"] == 0 && v["project"]["id"] != old_project && v["engine_ready"] == true
    });
    assert_ne!(v["timeline_playback"]["state"], "playing");
    assert_eq!(v["media"].as_array().unwrap().len(), 0);
    assert_eq!(v["clips"].as_array().unwrap().len(), 0);
    assert_eq!(v["audio"]["mix"]["active_tracks"], 0);
    assert_eq!(v["tracks"][1]["muted"], false);
    assert_eq!(
        v["tracks"][1]["gain_cdb"], 0,
        "the drag died with the project"
    );
    assert_eq!(v["history"]["depth"], 0);
    // The old track id means nothing now and cannot touch the new project.
    assert!(s.commit_track_gain(track, -900).is_err());
    let v = settled(&mut s);
    assert_eq!(v["tracks"][1]["gain_cdb"], 0);
    assert_eq!(v["history"]["depth"], 0);
}

#[test]
fn an_audio_only_source_goes_missing_and_relinks_without_losing_its_clip() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("song.mp3");
    let moved = directory.path().join("moved song.mp3");
    std::fs::write(&source, b"original song content").unwrap();
    let mut s = session(Duration::ZERO);
    ready(&mut s);
    s.import(source.clone()).unwrap();
    let v = wait(&mut s, |v| {
        v["media"].as_array().is_some_and(|m| !m.is_empty()) && v["import"]["busy"] == false
    });
    assert_eq!(v["media"][0]["audio_only"], true);
    let media: MediaId = id(&v["media"][0]["id"]);
    let track: TrackId = id(&v["tracks"][1]["id"]);
    s.place_audio_stream(media, 0, Some(track), TimelineSeek::Milliseconds(0))
        .unwrap();
    let v = wait(&mut s, |v| {
        v["pending"] == 0 && v["clips"].as_array().is_some_and(|c| c.len() == 1)
    });
    let clip = v["clips"][0].clone();
    let path = directory.path().join("song project.cutcap");
    saved(&mut s, &path);

    std::fs::rename(&source, &moved).unwrap();
    s.open_project(path).unwrap();
    let v = wait(&mut s, |v| {
        v["pending"] == 0 && v["media"][0]["availability"] == "missing"
    });
    assert_eq!(v["media"][0]["id"], media.to_string());
    assert_eq!(v["clips"][0]["id"], clip["id"], "the clip survives");
    assert_eq!(v["clips"][0]["stream"], clip["stream"]);
    assert_eq!(v["project"]["document"]["dirty"], false);

    s.relink_media(media, moved).unwrap();
    let v = wait(&mut s, |v| {
        v["pending"] == 0 && v["media"][0]["availability"] == "ready"
    });
    assert_eq!(
        v["media"][0]["audio_only"], true,
        "still no invented picture"
    );
    assert_eq!(v["clips"][0]["id"], clip["id"]);
    assert_eq!(v["project"]["document"]["dirty"], true);
}

#[test]
fn clicking_the_current_source_brings_its_inspector_back_without_reloading_it() {
    let mut s = session(Duration::ZERO);
    let (v, _, _, audio) = linked(&mut s);
    let media: MediaId = id(&v["media"][0]["id"]);
    s.select_clip(audio).unwrap();
    let v = wait(&mut s, |v| {
        v["pending"] == 0 && v["inspector"]["kind"] == "clip"
    });
    assert_eq!(v["media"][0]["selected"], true, "still the current source");
    assert!(v["frame"].is_object());
    s.select_media(media).unwrap();
    // Reloading would have cleared the monitor synchronously.
    assert!(view(&mut s)["frame"].is_object());
    let v = wait(&mut s, |v| {
        v["pending"] == 0 && v["inspector"]["kind"] == "source"
    });
    assert!(v["frame"].is_object());
    assert_eq!(v["media"][0]["audio_streams"].as_array().unwrap().len(), 1);
    assert_eq!(
        v["clips"].as_array().unwrap().len(),
        2,
        "the clips are untouched"
    );
}

#[test]
fn a_timeline_holding_only_audio_has_its_length_and_plays() {
    let mut s = session(Duration::ZERO);
    ready(&mut s);
    s.import(fixture("song.mp3")).unwrap();
    let v = wait(&mut s, |v| {
        v["media"].as_array().is_some_and(|m| !m.is_empty()) && v["import"]["busy"] == false
    });
    let media: MediaId = id(&v["media"][0]["id"]);
    let track: TrackId = id(&v["tracks"][1]["id"]);
    s.place_audio_stream(media, 0, Some(track), TimelineSeek::Milliseconds(0))
        .unwrap();
    wait(&mut s, |v| {
        v["pending"] == 0 && v["clips"].as_array().is_some_and(|c| c.len() == 1)
    });
    s.set_preview_mode(PreviewMode::Timeline).unwrap();
    let v = wait(&mut s, |v| {
        v["pending"] == 0 && v["timeline_preview"]["extent_ms"] == 6000
    });
    assert!(v["timeline_preview"]["extent_ticks"].as_i64().unwrap() > 0);
    s.play_timeline().unwrap();
    let v = wait(&mut s, |v| v["timeline_playback"]["state"] == "playing");
    assert_eq!(v["timeline_preview"]["extent_ms"], 6000);
    // Time passes with no picture to show.
    let first = v["timeline_playback"]["clock_ms"].as_i64().unwrap();
    wait(&mut s, |v| {
        v["timeline_playback"]["clock_ms"].as_i64().unwrap() > first + 50
    });
}

#[test]
fn audio_that_outlasts_the_picture_extends_the_timeline() {
    let mut s = session(Duration::ZERO);
    ready(&mut s);
    s.import(fixture("clip.mkv")).unwrap();
    let v = wait(&mut s, |v| {
        v["media"].as_array().is_some_and(|m| !m.is_empty()) && v["frame"].is_object()
    });
    let picture: MediaId = id(&v["media"][0]["id"]);
    let video: TrackId = id(&v["tracks"][0]["id"]);
    let audio: TrackId = id(&v["tracks"][1]["id"]);
    s.place_clip(picture, video, TimelineSeek::Milliseconds(0))
        .unwrap();
    let v = wait(&mut s, |v| {
        v["pending"] == 0 && v["clips"].as_array().is_some_and(|c| c.len() == 1)
    });
    let picture_end = v["clips"][0]["end_ms"].as_i64().unwrap();
    s.import(fixture("song.mp3")).unwrap();
    let v = wait(&mut s, |v| {
        v["media"].as_array().is_some_and(|m| m.len() == 2) && v["import"]["busy"] == false
    });
    let song = v["media"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["audio_only"] == true)
        .unwrap();
    let song: MediaId = id(&song["id"]);
    s.place_audio_stream(
        song,
        0,
        Some(audio),
        TimelineSeek::Milliseconds(picture_end),
    )
    .unwrap();
    wait(&mut s, |v| {
        v["pending"] == 0 && v["clips"].as_array().is_some_and(|c| c.len() == 2)
    });
    s.set_preview_mode(PreviewMode::Timeline).unwrap();
    wait(&mut s, |v| {
        v["pending"] == 0 && v["timeline_preview"]["extent_ms"] == picture_end + 6000
    });
}
