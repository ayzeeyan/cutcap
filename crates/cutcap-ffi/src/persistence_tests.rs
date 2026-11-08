use super::*;
use cutcap_core::{Clip, MediaAsset, Track};
use std::time::{Duration, Instant};

fn document(directory: &Path) -> Document {
    let mut project = Project::new("编辑 session 🎬", Timebase::new(1, 48_000).unwrap()).unwrap();
    let video = TrackId::new();
    let audio = TrackId::new();
    for (id, kind) in [
        (video, TrackKind::Video),
        (audio, TrackKind::Audio),
        (TrackId::new(), TrackKind::Video),
    ] {
        project
            .add_track(Track {
                id,
                name: format!("{kind:?}"),
                kind,
            })
            .unwrap();
    }
    let mut sources = Vec::new();
    for (track_id, kind, name) in [
        (video, TrackKind::Video, "source 视频.mkv"),
        (audio, TrackKind::Audio, "sound.wav"),
    ] {
        let id = MediaId::new();
        let path = directory.join(name);
        fs::write(&path, b"owned synthetic fixture").unwrap();
        project
            .add_media(MediaAsset {
                id,
                path: fs::canonicalize(path).unwrap(),
                kind,
            })
            .unwrap();
        project
            .insert_clip(Clip {
                stream: match kind {
                    TrackKind::Video => cutcap_core::ClipStream::Video,
                    TrackKind::Audio => cutcap_core::ClipStream::Audio { index: 0 },
                },
                link: None,
                id: ClipId::new(),
                media_id: id,
                track_id,
                start: TimelineTime(0),
                duration: TimelineDuration::new(48_000).unwrap(),
                source_in: SourceTime {
                    ticks: -10,
                    timebase: Timebase::new(1, 1000).unwrap(),
                },
            })
            .unwrap();
        sources.push(SavedSource {
            id,
            metadata: None,
            fingerprint: None,
        });
    }
    Document::new(project, sources).unwrap()
}

#[test]
fn full_graph_and_order_round_trip_with_relative_unicode_references() {
    let directory = tempfile::tempdir().unwrap();
    let doc = document(directory.path());
    let path = directory.path().join("工程 🎞.cutcap");
    save(&doc, &path, &Cancellation::default()).unwrap();
    let bytes = fs::read(&path).unwrap();
    let stored: Document = serde_json::from_slice(&bytes).unwrap();
    assert!(stored.project.media().iter().all(|m| !m.path.is_absolute()));
    let loaded = open(&fs::canonicalize(&path).unwrap(), &Cancellation::default()).unwrap();
    assert_eq!(loaded.project, doc.project);
    assert_eq!(loaded.sources.len(), 2);
    assert!(!String::from_utf8(bytes).unwrap().contains("selection"));
}

#[test]
fn moving_project_directory_preserves_descendants_and_missing_sources() {
    let directory = tempfile::tempdir().unwrap();
    let original = directory.path().join("original");
    let moved = directory.path().join("moved");
    fs::create_dir(&original).unwrap();
    let doc = document(&original);
    save(
        &doc,
        &original.join("test.cutcap"),
        &Cancellation::default(),
    )
    .unwrap();
    fs::rename(&original, &moved).unwrap();
    fs::remove_file(moved.join("sound.wav")).unwrap();
    let loaded = open(
        &fs::canonicalize(moved.join("test.cutcap")).unwrap(),
        &Cancellation::default(),
    )
    .unwrap();
    assert_eq!(loaded.project.id(), doc.project.id());
    assert_eq!(loaded.project.media().len(), 2);
    assert!(
        loaded
            .project
            .media()
            .iter()
            .all(|m| m.path.starts_with(fs::canonicalize(&moved).unwrap()))
    );
    assert!(!loaded.project.media()[1].path.exists());
}

#[test]
fn invalid_schema_ids_graph_and_path_policy_are_rejected() {
    let directory = tempfile::tempdir().unwrap();
    let doc = document(directory.path());
    let valid = serde_json::to_value(&doc).unwrap();
    for (pointer, value) in [
        ("/format_version", serde_json::json!(99)),
        ("/project/schema_version", serde_json::json!(99)),
        (
            "/project/id",
            serde_json::json!("00000000-0000-0000-0000-000000000000"),
        ),
        (
            "/project/clips/0/track_id",
            serde_json::json!(TrackId::new()),
        ),
        ("/project/clips/0/duration", serde_json::json!(0)),
        ("/project/media/0/path", serde_json::json!("../secret.mkv")),
        ("/project/media/0/path", serde_json::json!("C:relative.mkv")),
        ("/sources/1/id", serde_json::json!(doc.sources[0].id)),
    ] {
        let mut invalid = valid.clone();
        *invalid.pointer_mut(pointer).unwrap() = value;
        let path = directory.path().join("invalid.cutcap");
        fs::write(&path, serde_json::to_vec(&invalid).unwrap()).unwrap();
        assert!(
            open(&path, &Cancellation::default()).is_err(),
            "accepted {pointer}"
        );
    }
}

#[test]
fn parsing_is_bounded_before_constructing_the_graph() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("bad.cutcap");
    File::create(&path)
        .unwrap()
        .set_len(MAX_DOCUMENT_BYTES + 1)
        .unwrap();
    assert!(
        open(&path, &Cancellation::default())
            .unwrap_err()
            .to_string()
            .contains("budget")
    );
    for bytes in [
        b"{ broken".to_vec(),
        format!("{}0{}", "[".repeat(34), "]".repeat(34)).into_bytes(),
        serde_json::to_vec(&"x".repeat(32_769)).unwrap(),
    ] {
        fs::write(&path, bytes).unwrap();
        assert!(open(&path, &Cancellation::default()).is_err());
    }
}

#[test]
fn failure_and_cancellation_before_commit_preserve_previous_bytes_and_remove_temp() {
    let directory = tempfile::tempdir().unwrap();
    let doc = document(directory.path());
    let path = directory.path().join("existing.cutcap");
    save(&doc, &path, &Cancellation::default()).unwrap();
    let previous = fs::read(&path).unwrap();
    let failure = save_with(&doc, &path, &Cancellation::default(), || {
        Err(invalid("injected write interruption"))
    });
    assert!(failure.is_err());
    assert_eq!(fs::read(&path).unwrap(), previous);
    let cancel = Cancellation::default();
    assert!(
        save_with(&doc, &path, &cancel, || {
            cancel.cancel();
            Ok(())
        })
        .is_err()
    );
    assert_eq!(fs::read(&path).unwrap(), previous);
    assert!(
        fs::read_dir(directory.path()).unwrap().all(|e| !e
            .unwrap()
            .file_name()
            .to_string_lossy()
            .ends_with(".tmp"))
    );
}

#[cfg(windows)]
#[test]
fn locked_destination_cannot_be_overwritten_and_source_files_are_never_targets() {
    use std::os::windows::fs::OpenOptionsExt;
    let directory = tempfile::tempdir().unwrap();
    let mut doc = document(directory.path());
    let path = directory.path().join("existing.cutcap");
    save(&doc, &path, &Cancellation::default()).unwrap();
    let previous = fs::read(&path).unwrap();
    let lock = OpenOptions::new()
        .read(true)
        .share_mode(1)
        .open(&path)
        .unwrap();
    assert!(save(&doc, &path, &Cancellation::default()).is_err());
    assert_eq!(fs::read(&path).unwrap(), previous);
    drop(lock);
    let id = doc.project.media()[0].id;
    doc.project
        .relink_media(id, fs::canonicalize(&path).unwrap())
        .unwrap();
    assert!(
        save(&doc, &path, &Cancellation::default())
            .unwrap_err()
            .to_string()
            .contains("source media")
    );
    assert_eq!(fs::read(&path).unwrap(), previous);
}

#[test]
fn long_windows_paths_and_external_absolute_references_round_trip() {
    let directory = tempfile::tempdir().unwrap();
    let external = tempfile::tempdir().unwrap();
    let mut doc = document(external.path());
    let mut long = directory.path().to_path_buf();
    for _ in 0..4 {
        long.push("unicode-视频-".to_owned() + &"segment".repeat(9));
    }
    fs::create_dir_all(&long).unwrap();
    let path = long.join("long.cutcap");
    save(&doc, &path, &Cancellation::default()).unwrap();
    assert!(path.as_os_str().len() > 260);
    let loaded = open(&path, &Cancellation::default()).unwrap();
    assert_eq!(loaded.project, doc.project);
    doc.project
        .relink_media(doc.project.media()[0].id, PathBuf::from("relative.mkv"))
        .unwrap();
    assert!(save(&doc, &path, &Cancellation::default()).is_err());
}

#[test]
fn cancelled_file_job_terminates_without_a_commit() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("not-written.cutcap");
    let (entered_tx, entered_rx) = std::sync::mpsc::sync_channel(1);
    let (resume_tx, resume_rx) = std::sync::mpsc::sync_channel(1);
    let doc = document(directory.path());
    let destination = path.clone();
    let mut job = FileJob::spawn(RequestId::new(), 1, true, move |cancel| {
        save_with(&doc, &destination, cancel, || {
            entered_tx.send(()).unwrap();
            resume_rx.recv_timeout(Duration::from_secs(2)).unwrap();
            Ok(())
        })
        .map(FileOutput::Saved)
    })
    .unwrap();
    entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    job.cancel();
    resume_tx.send(()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if let Some(result) = job.poll() {
            assert!(result.is_err());
            break;
        }
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(2));
    }
    assert!(!path.exists());
}

#[test]
#[ignore = "subprocess fixture invoked by killed_writer_preserves_previous_project"]
fn crash_save_child() {
    let Some(path) = std::env::var_os("CUTCAP_TEST_PROJECT") else {
        return;
    };
    let marker = PathBuf::from(std::env::var_os("CUTCAP_TEST_MARKER").unwrap());
    let path = PathBuf::from(path);
    let doc = open(&path, &Cancellation::default()).unwrap();
    let _result = save_with(&doc, &path, &Cancellation::default(), || {
        fs::write(marker, b"temporary file synced; destination unchanged").unwrap();
        loop {
            thread::park();
        }
    });
}

#[test]
fn killed_writer_preserves_previous_project() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("crash.cutcap");
    let marker = directory.path().join("ready");
    let doc = document(directory.path());
    save(&doc, &path, &Cancellation::default()).unwrap();
    let previous = fs::read(&path).unwrap();
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "persistence::tests::crash_save_child",
            "--ignored",
        ])
        .env("CUTCAP_TEST_PROJECT", &path)
        .env("CUTCAP_TEST_MARKER", &marker)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    let mut child = command.spawn().unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !marker.exists() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(5));
    }
    let reached_commit_boundary = marker.exists();
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(
        reached_commit_boundary,
        "child did not reach the injected precommit boundary"
    );
    assert_eq!(fs::read(&path).unwrap(), previous);
    assert_eq!(
        open(&fs::canonicalize(&path).unwrap(), &Cancellation::default())
            .unwrap()
            .project,
        doc.project
    );
}

/// A linked audiovisual pair on V1/A1 from one source with saved metadata.
fn linked_document(directory: &Path) -> Document {
    let mut project = Project::new("Linked", Timebase::DEFAULT_TIMELINE).unwrap();
    let (video, audio) = (TrackId::new(), TrackId::new());
    for (id, kind, name) in [
        (video, TrackKind::Video, "Video 1"),
        (audio, TrackKind::Audio, "Audio 1"),
    ] {
        project
            .add_track(Track {
                id,
                name: name.into(),
                kind,
            })
            .unwrap();
    }
    let media = MediaId::new();
    let path = directory.join("take.mp4");
    fs::write(&path, b"owned synthetic fixture").unwrap();
    project
        .add_media(MediaAsset {
            id: media,
            path: fs::canonicalize(path).unwrap(),
            kind: TrackKind::Video,
        })
        .unwrap();
    let link = Some(LinkId::new());
    let base = Clip {
        id: ClipId::new(),
        media_id: media,
        track_id: video,
        start: TimelineTime(720_000),
        duration: TimelineDuration::new(3_600_000).unwrap(),
        source_in: SourceTime {
            ticks: 1234,
            timebase: Timebase::new(1, 15360).unwrap(),
        },
        stream: cutcap_core::ClipStream::Video,
        link,
    };
    let audio_clip = Clip {
        id: ClipId::new(),
        track_id: audio,
        stream: cutcap_core::ClipStream::Audio { index: 1 },
        ..base.clone()
    };
    project
        .replace_clips(&[], &[(0, base), (1, audio_clip)])
        .unwrap();
    Document::new(
        project,
        vec![SavedSource {
            id: media,
            metadata: None,
            fingerprint: None,
        }],
    )
    .unwrap()
}

#[test]
fn linked_pairs_round_trip_with_streams_links_and_exact_timing() {
    let directory = tempfile::tempdir().unwrap();
    let doc = linked_document(directory.path());
    let path = directory.path().join("linked.cutcap");
    save(&doc, &path, &Cancellation::default()).unwrap();
    let text = fs::read_to_string(&path).unwrap();
    let stored: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(stored["project"]["schema_version"], 2);
    assert_eq!(stored["project"]["clips"][1]["stream"]["kind"], "audio");
    assert_eq!(stored["project"]["clips"][1]["stream"]["index"], 1);
    assert_eq!(
        stored["project"]["clips"][0]["link"],
        stored["project"]["clips"][1]["link"]
    );
    let loaded = open(&fs::canonicalize(&path).unwrap(), &Cancellation::default()).unwrap();
    assert_eq!(
        loaded.project, doc.project,
        "ids, streams, links and source in-points are exact"
    );
}

#[test]
fn schema_one_files_open_migrated_and_refused_migrations_leave_the_file_untouched() {
    let directory = tempfile::tempdir().unwrap();
    // A Phase 4A document: schema 1, no stream/link fields, video clips only.
    let doc = document(directory.path());
    let mut value = serde_json::to_value(&doc).unwrap();
    value["project"]["schema_version"] = 1.into();
    let clips = value["project"]["clips"].as_array_mut().unwrap();
    clips.retain(|c| c["stream"]["kind"] == "video");
    for clip in clips.iter_mut() {
        clip.as_object_mut().unwrap().remove("stream");
    }
    // Keep the manifest consistent with the remaining media.
    let kept: Vec<_> = clips.iter().map(|c| c["media_id"].clone()).collect();
    value["project"]["media"]
        .as_array_mut()
        .unwrap()
        .retain(|m| kept.contains(&m["id"]));
    value["sources"]
        .as_array_mut()
        .unwrap()
        .retain(|s| kept.contains(&s["id"]));
    for media in value["project"]["media"].as_array_mut().unwrap() {
        let name = std::path::Path::new(media["path"].as_str().unwrap())
            .file_name()
            .unwrap()
            .to_owned();
        media["path"] = name.to_string_lossy().into_owned().into();
    }
    let old = directory.path().join("phase4a.cutcap");
    let bytes = serde_json::to_vec_pretty(&value).unwrap();
    fs::write(&old, &bytes).unwrap();
    let loaded = open(&fs::canonicalize(&old).unwrap(), &Cancellation::default()).unwrap();
    assert_eq!(loaded.project.clips().len(), 1);
    assert_eq!(
        loaded.project.clips()[0].stream,
        cutcap_core::ClipStream::Video
    );
    assert_eq!(
        fs::read(&old).unwrap(),
        bytes,
        "opening never rewrites the file"
    );
    // A schema 1 clip on an audio track cannot be migrated: refused, file intact.
    let audio_track = value["project"]["tracks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["kind"] == "Audio")
        .unwrap()["id"]
        .clone();
    value["project"]["clips"][0]["track_id"] = audio_track;
    let bad = directory.path().join("unmigratable.cutcap");
    let bytes = serde_json::to_vec_pretty(&value).unwrap();
    fs::write(&bad, &bytes).unwrap();
    let error = open(&fs::canonicalize(&bad).unwrap(), &Cancellation::default()).unwrap_err();
    assert!(
        error.message.contains("cannot be migrated"),
        "{}",
        error.message
    );
    assert_eq!(fs::read(&bad).unwrap(), bytes);
}
