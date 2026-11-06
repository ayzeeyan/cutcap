//! Session and ABI behavior against a controlled backend (no FFmpeg needed).
//! Real FFmpeg coverage stays in cutcap-media's ignored integration suite and
//! the .NET interop tests.
// ABI tests must call the exported unsafe functions exactly as the frontend does.
#![allow(unsafe_code)]
use crate::abi::*;
use crate::session::Session;
use cutcap_contracts::SourceTime;
use cutcap_media::*;
use serde_json::Value;
use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

const PROBE: &[u8] = br#"{"streams":[{"index":0,"codec_type":"video","codec_name":"ffv1","width":320,"height":180,"time_base":"1/1000","start_pts":0,"duration_ts":3000}],"format":{"format_name":"matroska"}}"#;

#[path = "timeline_preview_tests.rs"]
mod timeline_preview;

thread_local! {
    // Controlled media still exercises real source ownership/fingerprinting.
    // Each test thread cleans up its own disposable files, including Unicode.
    static FILES: tempfile::TempDir = tempfile::tempdir().unwrap();
}
fn fixture(path: &str) -> std::path::PathBuf {
    FILES.with(|directory| {
        let subdirectory = directory.path().join(uuid::Uuid::new_v4().to_string());
        std::fs::create_dir(&subdirectory).unwrap();
        let path = subdirectory.join(Path::new(path).file_name().unwrap());
        std::fs::write(&path, b"controlled backend source content").unwrap();
        path
    })
}

/// Probes succeed unless the file name contains "corrupt"; decodes wait
/// `delay` (cancellation-aware) and encode the requested PTS in the red channel.
struct Controlled {
    delay: Duration,
    decodes: AtomicUsize,
}

impl Controlled {
    fn new(delay: Duration) -> Arc<Self> {
        Arc::new(Self {
            delay,
            decodes: AtomicUsize::new(0),
        })
    }
}

impl MediaBackend for Controlled {
    fn stream_interval(
        &self,
        request: &DecodeRequest,
        minimum: SourceTime,
        cancel: &Cancellation,
        emit: &mut (dyn FnMut(DecodedFrame) -> Result<(), MediaError> + Send),
    ) -> Result<(), MediaError> {
        let mut positioned = request.clone();
        positioned.position.ticks = request
            .position
            .ticks
            .div_euclid(100)
            .saturating_mul(100)
            .max(minimum.ticks);
        for index in 0..30 {
            let mut next = positioned.clone();
            next.position.ticks += index * 100;
            let mut frame = self.decode(&next, cancel)?;
            frame.requested = request.position;
            emit(frame)?;
        }
        Ok(())
    }
    fn stream(
        &self,
        request: &DecodeRequest,
        cancel: &Cancellation,
        emit: &mut (dyn FnMut(DecodedFrame) -> Result<(), MediaError> + Send),
    ) -> Result<(), MediaError> {
        for index in 0..30 {
            let mut next = request.clone();
            next.position.ticks += index * 100;
            emit(self.decode(&next, cancel)?)?;
        }
        Ok(())
    }
    fn probe(&self, path: &Path, _: &Cancellation) -> Result<ProbeResult, MediaError> {
        if path.to_string_lossy().contains("corrupt") {
            return Err(MediaError::Process {
                stage: "Probe media",
                code: Some(1),
                detail: "invalid data".into(),
            });
        }
        Ok(ProbeResult {
            path: path.to_owned(),
            metadata: parse_probe(PROBE)?,
            probe_us: 7,
        })
    }
    fn decode(
        &self,
        request: &DecodeRequest,
        cancellation: &Cancellation,
    ) -> Result<DecodedFrame, MediaError> {
        let started = Instant::now();
        while started.elapsed() < self.delay {
            if cancellation.is_cancelled() {
                return Err(MediaError::Cancelled);
            }
            thread::sleep(Duration::from_millis(2));
        }
        self.decodes.fetch_add(1, Ordering::SeqCst);
        let red = u8::try_from(request.position.ticks.rem_euclid(256)).unwrap_or(0);
        let pixels = [red, 10, 20, 255].repeat(4 * 2);
        Ok(DecodedFrame {
            ticket: request.ticket,
            requested: request.position,
            actual: SourceTime {
                ticks: request.position.ticks,
                timebase: request.position.timebase,
            },
            image: RgbaFrame::new(4, 2, 16, pixels)?,
            decode_us: 3,
            process_id: 0,
        })
    }
}

fn session(delay: Duration) -> Session {
    Session::with_backend(Controlled::new(delay)).unwrap()
}

fn view(session: &mut Session) -> Value {
    serde_json::from_str(session.view_json().unwrap()).unwrap()
}

fn pump_until(session: &mut Session, done: impl Fn(&Value) -> bool) -> Value {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        session.pump();
        let current = view(session);
        if done(&current) {
            return current;
        }
        assert!(Instant::now() < deadline, "timed out; last view {current}");
        thread::sleep(Duration::from_millis(5));
    }
}

fn ready(session: &mut Session) -> Value {
    pump_until(session, |v| v["engine_ready"] == true)
}

fn imported(session: &mut Session, path: &str) -> Value {
    ready(session);
    session.import(fixture(path)).unwrap();
    pump_until(session, |v| {
        v["media"].as_array().is_some_and(|m| !m.is_empty())
            && v["frame"].is_object()
            && v["source"]["busy"] == false
    })
}

fn saved(session: &mut Session, path: &Path) -> Value {
    session.save_project(Some(path.to_owned())).unwrap();
    pump_until(session, |v| {
        v["project"]["document"]["state"] == "saved" && v["pending"] == 0
    })
}

#[test]
fn save_open_preserves_ids_order_and_tracks_but_not_selection() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("session.cutcap");
    let mut session = session(Duration::ZERO);
    ready(&mut session);
    session
        .add_track(cutcap_contracts::TrackKind::Video)
        .unwrap();
    session
        .add_track(cutcap_contracts::TrackKind::Audio)
        .unwrap();
    pump_until(&mut session, |v| v["tracks"].as_array().unwrap().len() == 4);
    let original = saved(&mut session, &path);
    assert_eq!(original["project"]["document"]["dirty"], false);
    session
        .select_track(serde_json::from_value(original["tracks"][0]["id"].clone()).unwrap())
        .unwrap();
    pump_until(&mut session, |v| v["tracks"][0]["selected"] == true);
    assert_eq!(view(&mut session)["project"]["document"]["dirty"], false);
    session.new_project().unwrap();
    ready(&mut session);
    session.open_project(path).unwrap();
    let loaded = pump_until(&mut session, |v| {
        v["project"]["document"]["state"] == "opened" && v["pending"] == 0
    });
    assert_eq!(loaded["project"]["id"], original["project"]["id"]);
    for (before, after) in original["tracks"]
        .as_array()
        .unwrap()
        .iter()
        .zip(loaded["tracks"].as_array().unwrap())
    {
        assert_eq!(after["id"], before["id"]);
        assert_eq!(after["name"], before["name"]);
        assert_eq!(after["kind"], before["kind"]);
        assert_eq!(after["selected"], false);
    }
    assert_eq!(loaded["project"]["document"]["dirty"], false);
}

#[test]
fn missing_and_changed_sources_preserve_project_and_relink_requires_original_content() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source 视频.mkv");
    let moved = directory.path().join("relocated.mkv");
    std::fs::write(&source, b"original source content").unwrap();
    let mut session = session(Duration::ZERO);
    ready(&mut session);
    session.import(source.clone()).unwrap();
    let imported = pump_until(&mut session, |v| v["frame"].is_object());
    let id: cutcap_contracts::MediaId =
        serde_json::from_value(imported["media"][0]["id"].clone()).unwrap();
    let path = directory.path().join("project.cutcap");
    saved(&mut session, &path);
    std::fs::rename(&source, &moved).unwrap();
    session.open_project(path.clone()).unwrap();
    let missing = pump_until(&mut session, |v| v["media"][0]["availability"] == "missing");
    assert_eq!(missing["project"]["id"], imported["project"]["id"]);
    assert_eq!(missing["media"][0]["id"], imported["media"][0]["id"]);
    session.select_media(id).unwrap();
    let unavailable = pump_until(&mut session, |v| v["media"][0]["selected"] == true);
    assert_eq!(unavailable["source"]["ready"], false);
    session.relink_media(id, moved.clone()).unwrap();
    let relinked = pump_until(&mut session, |v| {
        v["media"][0]["availability"] == "ready" && v["frame"].is_object()
    });
    assert_eq!(relinked["project"]["document"]["dirty"], true);
    assert_eq!(relinked["media"][0]["id"], imported["media"][0]["id"]);
    saved(&mut session, &path);
    session.pause();
    std::fs::write(&moved, b"modified source content").unwrap();
    session.open_project(path).unwrap();
    let changed = pump_until(&mut session, |v| v["media"][0]["availability"] == "changed");
    assert_eq!(changed["project"]["document"]["dirty"], false);
    assert_eq!(changed["media"][0]["id"], imported["media"][0]["id"]);
    session.relink_media(id, moved).unwrap();
    let failed = pump_until(&mut session, |v| {
        v["status"]["error"] == true && v["import"]["busy"] == false
    });
    assert_eq!(failed["media"][0]["availability"], "changed");
    assert_eq!(failed["project"]["document"]["dirty"], false);
}

#[test]
fn failed_open_preserves_live_project_and_shutdown_during_save_is_bounded() {
    let directory = tempfile::tempdir().unwrap();
    let mut session = session(Duration::ZERO);
    let original = imported(&mut session, "test.mkv");
    let path = directory.path().join("corrupt.cutcap");
    std::fs::write(&path, b"broken project").unwrap();
    session.open_project(path).unwrap();
    let failed = pump_until(&mut session, |v| {
        v["project"]["document"]["state"] == "failed"
    });
    assert_eq!(failed["project"]["id"], original["project"]["id"]);
    assert_eq!(failed["tracks"], original["tracks"]);
    assert_eq!(failed["media"], original["media"]);
    assert_eq!(
        failed["frame"]["generation"],
        original["frame"]["generation"]
    );
    session
        .save_project(Some(directory.path().join("closing.cutcap")))
        .unwrap();
    assert!(session.new_project().is_err());
    assert_eq!(
        view(&mut session)["frame"]["generation"],
        original["frame"]["generation"]
    );
    let before = Instant::now();
    session.shutdown().unwrap();
    assert!(before.elapsed() < Duration::from_secs(2));
}

#[test]
fn source_playback_state_is_independent_and_superseded_epochs_do_not_win() {
    let mut session = session(Duration::from_millis(2));
    imported(&mut session, r"C:\fixture.mkv");
    session.play().unwrap();
    let first = pump_until(&mut session, |v| {
        v["playback"]["offered"].as_u64().is_some_and(|n| n > 0)
    });
    let epoch = first["playback"]["epoch"].clone();
    let tracks = first["tracks"].clone();
    session.pause();
    let paused = view(&mut session);
    thread::sleep(Duration::from_millis(30));
    session.pump();
    assert_eq!(
        view(&mut session)["frame"]["generation"],
        paused["frame"]["generation"]
    );
    session.play().unwrap();
    assert_eq!(view(&mut session)["playback"]["epoch"], epoch);
    for ms in [2200, 100, 800] {
        session.seek(ms);
    }
    let latest = pump_until(&mut session, |v| {
        v["playback"]["offered_pts"]["ticks"] == 800
    });
    assert_ne!(latest["playback"]["epoch"], epoch);
    assert_eq!(latest["tracks"], tracks);
    session.new_project().unwrap();
    let next = ready(&mut session);
    assert_eq!(next["playback"]["state"], "stopped");
    assert!(next["frame"].is_null());
    session.shutdown().unwrap();
}

#[test]
fn import_registers_project_media_and_presents_first_frame() {
    let mut session = session(Duration::ZERO);
    let first = ready(&mut session);
    assert_eq!(first["tracks"].as_array().unwrap().len(), 2);
    assert_eq!(first["project"]["saved"], false);
    let view = imported(&mut session, r"C:\clips\ünï 视频 clip.mkv");
    let media = &view["media"][0];
    assert_eq!(media["name"], "ünï 视频 clip.mkv");
    assert_eq!(media["resolution"], "320 × 180");
    assert_eq!(media["duration_ms"], 3000);
    assert_eq!(media["selected"], true);
    assert_eq!(view["inspector"]["kind"], "source");
    assert_eq!(view["source"]["ready"], true);
    assert_eq!(view["source"]["duration_ms"], 3000);
    assert_eq!(view["import"]["message"], "Imported ünï 视频 clip.mkv.");
    assert_eq!(view["metrics"]["probe_us"], 7);
    assert!(
        view["source"]["caption"]
            .as_str()
            .unwrap()
            .contains("PTS 0 @ 1/1000")
    );
    session.shutdown().unwrap();
}

#[test]
fn rapid_seeks_present_only_the_latest_request() {
    let backend = Controlled::new(Duration::from_millis(40));
    let mut session = Session::with_backend(backend.clone()).unwrap();
    imported(&mut session, "C:/clips/a.mkv");
    let before = view(&mut session)["metrics"]["frames_presented"]
        .as_u64()
        .unwrap();
    for ms in [100, 1200, 2300, 1501] {
        session.seek(ms);
        session.pump();
    }
    let view = pump_until(&mut session, |v| v["source"]["busy"] == false);
    assert_eq!(view["source"]["position_ms"], 1501);
    assert!(
        view["source"]["caption"]
            .as_str()
            .unwrap()
            .contains("PTS 1501")
    );
    // Superseded requests were cancelled or discarded, never presented.
    assert_eq!(
        view["metrics"]["frames_presented"].as_u64().unwrap(),
        before + 1
    );
    let frame = session.frame().unwrap();
    assert_eq!(frame.image.pixels()[0], (1501 % 256) as u8);
    session.shutdown().unwrap();
}

#[test]
fn shutdown_during_probe_and_decode_is_bounded() {
    let mut session = session(Duration::from_secs(30));
    ready(&mut session);
    session.import(fixture("slow.mkv")).unwrap();
    pump_until(&mut session, |v| {
        v["media"].as_array().is_some_and(|m| !m.is_empty()) && v["source"]["busy"] == true
    });
    let started = Instant::now();
    session.shutdown().unwrap();
    assert!(started.elapsed() < Duration::from_secs(2));
    // A closed session rejects new work instead of reviving workers.
    assert!(
        session
            .add_track(cutcap_contracts::TrackKind::Audio)
            .is_err()
    );
    assert!(!session.pump());
}

#[test]
fn shutdown_does_not_wait_forever_for_an_uninterruptible_source_inspection() {
    use std::sync::{Mutex, mpsc};
    struct BlockedProbe {
        entered: mpsc::SyncSender<()>,
        release: Mutex<mpsc::Receiver<()>>,
    }
    impl MediaBackend for BlockedProbe {
        fn probe(&self, path: &Path, _: &Cancellation) -> Result<ProbeResult, MediaError> {
            self.entered.send(()).unwrap();
            self.release
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(3))
                .unwrap();
            Ok(ProbeResult {
                path: path.to_owned(),
                metadata: parse_probe(PROBE)?,
                probe_us: 1,
            })
        }
        fn decode(&self, _: &DecodeRequest, _: &Cancellation) -> Result<DecodedFrame, MediaError> {
            Err(MediaError::Worker)
        }
    }
    let (entered, waiting) = mpsc::sync_channel(1);
    let (release, blocked) = mpsc::sync_channel(1);
    let backend = Arc::new(BlockedProbe {
        entered,
        release: Mutex::new(blocked),
    });
    let mut session = Session::with_backend(backend).unwrap();
    ready(&mut session);
    let path = fixture("blocked.mkv");
    session.import(path.clone()).unwrap();
    waiting.recv_timeout(Duration::from_secs(2)).unwrap();
    let started = Instant::now();
    session.shutdown().unwrap();
    assert!(started.elapsed() < Duration::from_secs(1));
    assert!(!session.pump());
    release.send(()).unwrap();
    // On Windows the probe owns a read-only sharing lock. Successful rename
    // proves the detached cancelled task released it after the backend returned.
    let deadline = Instant::now() + Duration::from_secs(2);
    while std::fs::rename(&path, path.with_extension("released")).is_err() {
        assert!(
            Instant::now() < deadline,
            "cancelled probe retained its source handle"
        );
        thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn failed_probe_reports_an_error_and_registers_nothing() {
    let mut session = session(Duration::ZERO);
    ready(&mut session);
    session.import(fixture("corrupt.mp4")).unwrap();
    let view = pump_until(&mut session, |v| v["import"]["error"] == true);
    assert!(view["media"].as_array().unwrap().is_empty());
    assert_eq!(view["import"]["busy"], false);
    assert!(
        view["import"]["message"]
            .as_str()
            .unwrap()
            .contains("damaged or unsupported")
    );
    assert_eq!(view["status"]["error"], true);
    assert!(view["frame"].is_null());
}

#[test]
fn new_project_discards_media_frames_and_selection() {
    let mut session = session(Duration::ZERO);
    let first = imported(&mut session, "C:/clips/a.mkv");
    session.new_project().unwrap();
    let next = pump_until(&mut session, |v| {
        v["engine_ready"] == true && v["project"]["id"] != first["project"]["id"]
    });
    assert!(next["media"].as_array().unwrap().is_empty());
    assert!(next["frame"].is_null());
    assert_eq!(next["source"]["ready"], false);
    assert_eq!(next["inspector"]["kind"], "project");
}

#[test]
fn track_creation_and_selection_round_trip_through_the_view() {
    let mut session = session(Duration::ZERO);
    ready(&mut session);
    session
        .add_track(cutcap_contracts::TrackKind::Video)
        .unwrap();
    let view = pump_until(&mut session, |v| v["tracks"].as_array().unwrap().len() == 3);
    let badges: Vec<&str> = view["tracks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["badge"].as_str().unwrap())
        .collect();
    assert_eq!(badges, ["V1", "A1", "V2"]);
    assert_eq!(view["tracks"][2]["selected"], true);
    assert_eq!(view["inspector"]["kind"], "track");
    session.clear_selection().unwrap();
    let view = pump_until(&mut session, |v| v["tracks"][2]["selected"] == false);
    assert_eq!(view["inspector"]["kind"], "project");
}

// ---- ABI ------------------------------------------------------------------

fn abi_view(handle: u64) -> Value {
    let mut len = 0usize;
    // SAFETY (test): null buffer with zero capacity; `len` is a valid local.
    let status = unsafe { cutcap_session_view(handle, std::ptr::null_mut(), 0, &mut len) };
    assert_eq!(status, CUTCAP_BUFFER_TOO_SMALL);
    assert!(len > 0);
    let mut buffer = vec![0u8; len];
    // SAFETY (test): `buffer` holds `len` writable bytes.
    let status = unsafe { cutcap_session_view(handle, buffer.as_mut_ptr(), len, &mut len) };
    assert_eq!(status, CUTCAP_OK);
    serde_json::from_slice(&buffer[..len]).unwrap()
}

fn abi_pump_until(handle: u64, done: impl Fn(&Value) -> bool) -> Value {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let mut changed = 0u32;
        // SAFETY (test): `changed` is a valid local.
        assert_eq!(
            unsafe { cutcap_session_pump(handle, &mut changed) },
            CUTCAP_OK
        );
        let view = abi_view(handle);
        if done(&view) {
            return view;
        }
        assert!(Instant::now() < deadline, "timed out; last view {view}");
        thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn abi_version_mismatch_is_rejected_without_a_session() {
    let mut handle = 0u64;
    // SAFETY (test): `handle` is a valid local.
    let status = unsafe { cutcap_session_create(ABI_VERSION + 1, &mut handle) };
    assert_eq!(status, CUTCAP_ABI_MISMATCH);
    assert_eq!(handle, 0);
    assert!(last_error_for_test().contains("ABI"));
    assert_eq!(cutcap_abi_version(), ABI_VERSION);
}

#[test]
fn invalid_and_double_destroyed_handles_are_rejected() {
    assert_eq!(cutcap_session_destroy(0), CUTCAP_INVALID_HANDLE);
    assert_eq!(cutcap_session_new_project(u64::MAX), CUTCAP_INVALID_HANDLE);
    let handle = register_for_test(session(Duration::ZERO));
    assert_eq!(cutcap_session_destroy(handle), CUTCAP_OK);
    assert_eq!(cutcap_session_destroy(handle), CUTCAP_INVALID_HANDLE);
    let mut changed = 0u32;
    // SAFETY (test): `changed` is a valid local.
    assert_eq!(
        unsafe { cutcap_session_pump(handle, &mut changed) },
        CUTCAP_INVALID_HANDLE
    );
    assert!(last_error_for_test().contains("Unknown or closed"));
}

#[test]
fn abi_errors_propagate_codes_and_messages() {
    let handle = register_for_test(session(Duration::ZERO));
    abi_pump_until(handle, |v| v["engine_ready"] == true);
    let text = b"1.5";
    // SAFETY (test): `text` is a valid local slice.
    let status = unsafe { cutcap_session_seek_text(handle, text.as_ptr(), text.len()) };
    assert_eq!(status, CUTCAP_INVALID_ARGUMENT);
    assert!(last_error_for_test().contains("Enter a source time"));
    let invalid_utf8 = [0xff, 0xfe];
    // SAFETY (test): valid local slice.
    let status =
        unsafe { cutcap_session_seek_text(handle, invalid_utf8.as_ptr(), invalid_utf8.len()) };
    assert_eq!(status, CUTCAP_INVALID_ARGUMENT);
    let id = b"not-a-uuid";
    // SAFETY (test): valid local slice.
    let status = unsafe { cutcap_session_select_track(handle, id.as_ptr(), id.len()) };
    assert_eq!(status, CUTCAP_INVALID_ARGUMENT);
    let nil = b"00000000-0000-0000-0000-000000000000";
    // SAFETY (test): valid local slice.
    let status = unsafe { cutcap_session_select_media(handle, nil.as_ptr(), nil.len()) };
    assert_eq!(status, CUTCAP_INVALID_ARGUMENT);
    // SAFETY (test): null with nonzero length must be rejected, not read.
    let status = unsafe { cutcap_session_select_media(handle, std::ptr::null(), 36) };
    assert_eq!(status, CUTCAP_INVALID_ARGUMENT);
    // SAFETY (test): null path must be rejected, not read.
    let status = unsafe { cutcap_session_import(handle, std::ptr::null(), 4) };
    assert_eq!(status, CUTCAP_INVALID_ARGUMENT);
    assert_eq!(cutcap_session_add_track(handle, 7), CUTCAP_INVALID_ARGUMENT);
    // SAFETY (test): null output pointer must be rejected.
    let status = unsafe { cutcap_session_pump(handle, std::ptr::null_mut()) };
    assert_eq!(status, CUTCAP_INVALID_ARGUMENT);
    // The engine's own rejection (unknown media) arrives as a view status.
    let unknown = uuid::Uuid::new_v4().to_string();
    // SAFETY (test): valid local string.
    let status = unsafe { cutcap_session_select_media(handle, unknown.as_ptr(), unknown.len()) };
    assert_eq!(status, CUTCAP_OK);
    let view = abi_pump_until(handle, |v| {
        v["status"]["text"]
            .as_str()
            .is_some_and(|text| text.contains("no longer"))
    });
    assert!(
        view["status"]["text"]
            .as_str()
            .unwrap()
            .contains("no longer in this project")
    );
    assert_eq!(cutcap_session_destroy(handle), CUTCAP_OK);
}

#[test]
fn abi_import_accepts_unicode_wide_paths() {
    let handle = register_for_test(session(Duration::ZERO));
    abi_pump_until(handle, |v| v["engine_ready"] == true);
    let fixture = fixture(r"C:\Users\编辑\Видео\clip 🎬.mkv");
    let path: Vec<u16> = fixture.to_str().unwrap().encode_utf16().collect();
    // SAFETY (test): `path` is a valid local UTF-16 buffer.
    let status = unsafe { cutcap_session_import(handle, path.as_ptr(), path.len()) };
    assert_eq!(status, CUTCAP_OK);
    let view = abi_pump_until(handle, |v| {
        v["media"].as_array().is_some_and(|m| !m.is_empty())
    });
    assert_eq!(view["media"][0]["name"], "clip 🎬.mkv");
    assert_eq!(cutcap_session_destroy(handle), CUTCAP_OK);
}

#[test]
fn abi_frame_copy_validates_buffers_generation_and_stride() {
    let handle = register_for_test(session(Duration::ZERO));
    abi_pump_until(handle, |v| v["engine_ready"] == true);
    let mut info = CutCapFrameInfo::default();
    // SAFETY (test): `info` is a valid local.
    assert_eq!(
        unsafe { cutcap_session_frame_info(handle, &mut info) },
        CUTCAP_NO_FRAME
    );
    let fixture = fixture("a.mkv");
    let path: Vec<u16> = fixture.to_str().unwrap().encode_utf16().collect();
    // SAFETY (test): valid local buffer.
    assert_eq!(
        unsafe { cutcap_session_import(handle, path.as_ptr(), path.len()) },
        CUTCAP_OK
    );
    abi_pump_until(handle, |v| v["frame"].is_object());
    // SAFETY (test): `info` is a valid local.
    assert_eq!(
        unsafe { cutcap_session_frame_info(handle, &mut info) },
        CUTCAP_OK
    );
    assert_eq!((info.width, info.height, info.stride), (4, 2, 16));
    assert_eq!(info.pixel_format, CUTCAP_PIXEL_RGBA8_OPAQUE);
    let mut small = vec![0u8; 16];
    // SAFETY (test): `small` holds its length in writable bytes.
    let status = unsafe {
        cutcap_session_copy_frame(handle, info.generation, small.as_mut_ptr(), small.len(), 16)
    };
    assert_eq!(status, CUTCAP_INVALID_ARGUMENT);
    let mut narrow = vec![0u8; 64];
    // SAFETY (test): valid local buffer; stride below the row size must be rejected.
    let status = unsafe {
        cutcap_session_copy_frame(
            handle,
            info.generation,
            narrow.as_mut_ptr(),
            narrow.len(),
            8,
        )
    };
    assert_eq!(status, CUTCAP_INVALID_ARGUMENT);
    // SAFETY (test): valid local buffer; wrong generation.
    let status = unsafe {
        cutcap_session_copy_frame(handle, info.generation + 1, narrow.as_mut_ptr(), 64, 16)
    };
    assert_eq!(status, CUTCAP_STALE_FRAME);
    // Padded destination rows, as a locked bitmap may have.
    let mut padded = vec![0xAAu8; 24 + 16];
    // SAFETY (test): `padded` holds 40 writable bytes = stride 24 * 1 + row 16.
    let status = unsafe {
        cutcap_session_copy_frame(
            handle,
            info.generation,
            padded.as_mut_ptr(),
            padded.len(),
            24,
        )
    };
    assert_eq!(status, CUTCAP_OK);
    assert_eq!(&padded[0..4], &[0, 10, 20, 255]);
    assert_eq!(&padded[16..24], &[0xAA; 8], "padding must be untouched");
    assert_eq!(&padded[24..28], &[0, 10, 20, 255]);
    assert_eq!(cutcap_session_destroy(handle), CUTCAP_OK);
}

#[path = "timeline_tests.rs"]
mod timeline;
#[path = "timeline_edit_session_tests.rs"]
mod timeline_edit_session;
