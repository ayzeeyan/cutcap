//! Phase 4A filmstrip thumbnails: worker demand/cancellation/backpressure,
//! session validation and stale-result gates, clip-row source intervals after
//! edits, and the C ABI. Controlled backends only; real pixels/PTS are in the
//! cutcap-media FFmpeg integration suite and the .NET interop tests.
use super::*;
use crate::thumbnails::{Job, MAX_WAITING, Status, ThumbnailService};
use cutcap_contracts::*;
use std::sync::Mutex;

/// Blocks each batch until released; records batch sizes and cancellations.
#[derive(Default)]
struct Gate {
    open: Mutex<bool>,
    batches: Mutex<Vec<Vec<u64>>>,
    cancelled: AtomicUsize,
}
impl MediaBackend for Gate {
    fn probe(&self, _: &Path, _: &Cancellation) -> Result<ProbeResult, MediaError> {
        Err(MediaError::Worker)
    }
    fn decode(&self, _: &DecodeRequest, _: &Cancellation) -> Result<DecodedFrame, MediaError> {
        Err(MediaError::Worker)
    }
    fn thumbnails(
        &self,
        _: &MediaSummary,
        _: u32,
        batch: &[ThumbnailRequest],
        cancel: &Cancellation,
    ) -> Result<Vec<Result<Thumbnail, MediaError>>, MediaError> {
        self.batches
            .lock()
            .unwrap()
            .push(batch.iter().map(|r| r.token).collect());
        loop {
            if cancel.is_cancelled() {
                self.cancelled.fetch_add(1, Ordering::SeqCst);
                return Err(MediaError::Cancelled);
            }
            if *self.open.lock().unwrap() {
                break;
            }
            thread::sleep(Duration::from_millis(2));
        }
        Ok(batch
            .iter()
            .map(|r| {
                Ok(Thumbnail {
                    token: r.token,
                    requested: r.position,
                    actual: r.position,
                    image: RgbaFrame::new(2, 1, 8, vec![1, 2, 3, 255, 1, 2, 3, 255])?,
                    process_id: 7,
                })
            })
            .collect())
    }
}

fn media(name: &str) -> MediaSummary {
    MediaSummary {
        id: MediaId::new(),
        path: name.into(),
        metadata: parse_probe(PROBE).unwrap().into(),
    }
}

fn job(token: u64, media: &MediaSummary, key: &str, ticks: i64) -> Job {
    Job {
        token,
        media: media.clone(),
        source_key: key.into(),
        position: SourceTime {
            ticks,
            timebase: media.metadata.video().unwrap().timebase,
        },
        height: 40,
    }
}

#[track_caller]
fn until(done: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !done() {
        assert!(
            Instant::now() < deadline,
            "timed out at {}",
            std::panic::Location::caller()
        );
        thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn demand_batches_one_source_cancels_unwanted_work_and_coalesces_duplicates() {
    let gate = Arc::new(Gate::default());
    let service = ThumbnailService::start(gate.clone()).unwrap();
    let (a, b) = (media("a.mkv"), media("b.mkv"));
    // Duplicates collapse; one batch holds at most six samples of one source.
    let mut jobs: Vec<Job> = (0..8).map(|t| job(t, &a, "A", t as i64 * 100)).collect();
    jobs.push(job(3, &a, "A", 300));
    jobs.push(job(50, &b, "B", 0));
    service.set_demand(jobs);
    until(|| gate.batches.lock().unwrap().len() == 1);
    assert_eq!(gate.batches.lock().unwrap()[0], [0, 1, 2, 3, 4, 5]);
    assert_eq!(
        service.stats().pending,
        3,
        "tokens 6, 7 and the other source's 50 wait"
    );
    // Scrolling away: nothing in flight is wanted any more → the batch is cancelled.
    service.set_demand(vec![job(60, &b, "B", 1000)]);
    until(|| gate.cancelled.load(Ordering::SeqCst) == 1);
    until(|| gate.batches.lock().unwrap().len() == 2);
    assert_eq!(
        gate.batches.lock().unwrap()[1],
        [60],
        "old pending samples were dropped"
    );
    *gate.open.lock().unwrap() = true;
    until(|| service.stats().waiting == 1);
    let done = service.take().unwrap();
    assert_eq!(
        (done.token, done.status, done.source_key.as_str()),
        (60, Status::Ready, "B")
    );
    assert!(service.take().is_none());
    let stats = service.stats();
    assert_eq!(
        (stats.batches, stats.cancelled_batches, stats.decoded),
        (2, 1, 1)
    );
    assert_eq!(stats.last_process_id, Some(7));
}

#[test]
fn wanted_in_flight_work_continues_throttling_shrinks_batches_and_backpressure_bounds_results() {
    let gate = Arc::new(Gate::default());
    let service = ThumbnailService::start(gate.clone()).unwrap();
    let a = media("a.mkv");
    service.set_throttled(true);
    service.set_demand((0..5).map(|t| job(t, &a, "A", t as i64)).collect());
    until(|| gate.batches.lock().unwrap().len() == 1);
    assert_eq!(
        gate.batches.lock().unwrap()[0],
        [0, 1],
        "two samples per batch during playback"
    );
    // The same demand again (a re-plan after a tiny scroll) keeps the batch running.
    service.set_demand((0..5).map(|t| job(t, &a, "A", t as i64)).collect());
    thread::sleep(Duration::from_millis(30));
    assert_eq!(gate.cancelled.load(Ordering::SeqCst), 0);
    assert_eq!(
        service.stats().pending,
        3,
        "in-flight tokens are not queued twice"
    );
    service.set_throttled(false);
    *gate.open.lock().unwrap() = true;
    until(|| service.stats().waiting == 5);
    // Results are bounded: the worker stops when MAX_WAITING are uncopied.
    service.set_demand(
        (100..100 + MAX_WAITING as u64 + 20)
            .map(|t| job(t, &a, "A", t as i64))
            .collect(),
    );
    // The bound is checked before each batch, so it holds within one batch.
    until(|| service.stats().waiting >= MAX_WAITING);
    let waiting = service.stats().waiting;
    thread::sleep(Duration::from_millis(40));
    assert_eq!(service.stats().waiting, waiting, "the worker paused");
    assert!(
        waiting <= MAX_WAITING + cutcap_media::MAX_BATCH,
        "{waiting}"
    );
    assert!(service.stats().pending > 0);
    while service.take().is_some() {}
    until(|| service.stats().pending == 0 && service.stats().in_flight == 0);
}

#[test]
fn clear_and_shutdown_cancel_a_running_batch_promptly() {
    let gate = Arc::new(Gate::default());
    let mut service = ThumbnailService::start(gate.clone()).unwrap();
    let a = media("a.mkv");
    service.set_demand(vec![job(1, &a, "A", 0)]);
    until(|| gate.batches.lock().unwrap().len() == 1);
    service.clear();
    until(|| gate.cancelled.load(Ordering::SeqCst) == 1);
    assert!(
        service.take().is_none(),
        "a cleared project receives nothing"
    );
    service.set_demand(vec![job(2, &a, "A", 0)]);
    until(|| gate.batches.lock().unwrap().len() == 2);
    let started = Instant::now();
    service.shutdown();
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "shutdown during decode is bounded"
    );
    assert_eq!(gate.cancelled.load(Ordering::SeqCst), 2);
}

// ---- Session ---------------------------------------------------------------

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

fn want(token: u64, clip: &Value, sample_us: i64) -> serde_json::Value {
    serde_json::json!({
        "token": token,
        "media_id": clip["media_id"],
        "source_key": clip["source_key"],
        "sample_us": sample_us,
        "height": 40,
    })
}

fn collect(session: &mut Session, count: usize) -> Vec<(u64, Status, Option<i64>)> {
    let mut results = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(5);
    while results.len() < count {
        session.pump();
        if let Some(done) = session.next_thumbnail() {
            results.push((
                done.token,
                done.status,
                done.thumbnail.as_ref().map(|t| t.actual.ticks),
            ));
        }
        assert!(Instant::now() < deadline, "only {results:?}");
        thread::sleep(Duration::from_millis(2));
    }
    results.sort_by_key(|r| r.0);
    results
}

/// One 3 s controlled clip at 1 s on Video 1. Returns (clip row, media, track, clip id).
fn placed(session: &mut Session) -> (Value, MediaId, TrackId, ClipId) {
    let v = imported(session, "strip.mkv");
    let media: MediaId = serde_json::from_value(v["media"][0]["id"].clone()).unwrap();
    let track: TrackId = serde_json::from_value(v["tracks"][0]["id"].clone()).unwrap();
    session
        .place_clip(media, track, TimelineSeek::Milliseconds(1000))
        .unwrap();
    let v = wait(session, |v| {
        v["pending"] == 0 && v["clips"].as_array().is_some_and(|c| c.len() == 1)
    });
    let clip: ClipId = serde_json::from_value(v["clips"][0]["id"].clone()).unwrap();
    (v["clips"][0].clone(), media, track, clip)
}

#[test]
fn clip_rows_carry_the_exact_source_interval_through_trim_split_and_move() {
    let mut s = session(Duration::ZERO);
    let (row, _, track, clip) = placed(&mut s);
    assert_eq!(
        (
            row["source_in_ticks"].as_i64(),
            row["source_end_ticks"].as_i64()
        ),
        (Some(0), Some(3000))
    );
    assert_eq!(
        (row["source_in_us"].as_i64(), row["source_end_us"].as_i64()),
        (Some(0), Some(3_000_000))
    );
    assert!(
        row["source_key"]
            .as_str()
            .unwrap()
            .starts_with(&row["media_id"].as_str().unwrap().to_string())
    );
    assert!((row["thumb_aspect"].as_f64().unwrap() - 320.0 / 180.0).abs() < 1e-9);
    // Trim the start by 1 s: the filmstrip interval starts 1 s later in the source.
    s.trim_clip(clip, ClipEdge::Start, TimelineSeek::Milliseconds(2000))
        .unwrap();
    let trimmed = wait(&mut s, |v| {
        v["pending"] == 0 && v["clips"][0]["source_in_ticks"] == 1000
    });
    assert_eq!(trimmed["clips"][0]["source_in_us"], 1_000_000);
    // Split: left ends exactly where right begins in the source.
    s.split_clip(clip, TimelineSeek::Milliseconds(3000))
        .unwrap();
    let split = wait(&mut s, |v| {
        v["pending"] == 0 && v["clips"].as_array().unwrap().len() == 2
    });
    assert_eq!(
        split["clips"][0]["source_end_ticks"],
        split["clips"][1]["source_in_ticks"]
    );
    assert_eq!(split["clips"][1]["source_in_us"], 2_000_000);
    // Move: timeline position changes, source interval and source key do not.
    let right: ClipId = serde_json::from_value(split["clips"][1]["id"].clone()).unwrap();
    s.move_clip(
        right,
        track,
        TimelineSeek::Milliseconds(9000),
        ClipEdge::Start,
    )
    .unwrap();
    let moved = wait(&mut s, |v| {
        v["pending"] == 0 && v["clips"][1]["start_ms"] == 9000
    });
    for key in [
        "source_in_ticks",
        "source_end_ticks",
        "source_in_us",
        "source_end_us",
        "source_key",
    ] {
        assert_eq!(moved["clips"][1][key], split["clips"][1][key], "{key}");
    }
    // Thumbnail work never dirties the document or enters history.
    assert_eq!(moved["history"]["depth"], 4);
}

#[test]
fn session_validates_wants_delivers_by_token_and_maps_samples_by_ceiling() {
    let mut s = session(Duration::ZERO);
    let (row, _, _, _) = placed(&mut s);
    let wants = serde_json::json!([
        want(1, &row, 0),
        want(2, &row, 1_250_500), // 1250.5 ms → ceil 1251 ticks on the 1/1000 clock
        want(3, &row, 3_000_000), // at/after the source end: no frame, no decode
        { "token": 4, "media_id": row["media_id"], "source_key": "stale-key", "sample_us": 0, "height": 40 },
        { "token": 5, "media_id": MediaId::new(), "source_key": row["source_key"], "sample_us": 0, "height": 40 },
    ]);
    s.request_thumbnails(&wants.to_string()).unwrap();
    let results = collect(&mut s, 3);
    assert_eq!(
        results,
        [
            (1, Status::Ready, Some(0)),
            (2, Status::Ready, Some(1251)),
            (3, Status::NoFrame, None)
        ]
    );
    thread::sleep(Duration::from_millis(30));
    s.pump();
    assert!(
        s.next_thumbnail().is_none(),
        "stale key and unknown media were dropped"
    );
    let stats = s.thumbnail_stats();
    assert_eq!(stats.decoded, 2);
    // The published view follows thumbnail progress (no other change republishes it).
    let shown = full(&mut s);
    assert_eq!(shown["thumbnails"]["decoded"], 2);
    // The past-end sample completed without a decode: it is not a decoder no-frame result.
    assert_eq!(shown["thumbnails"]["no_frame"], 0);
    assert!(shown["thumbnails"]["batches"].as_u64().unwrap() >= 1);
    assert!(s.request_thumbnails("{not json").is_err());
    assert!(
        s.request_thumbnails(&serde_json::Value::Array(vec![want(9, &row, 0); 129]).to_string())
            .is_err()
    );
    assert_eq!(
        full(&mut s)["project"]["document"]["dirty"],
        true,
        "unchanged (a new project is dirty)"
    );
}

#[test]
fn stale_results_never_reach_a_new_project_or_a_relinked_source() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("relink me.mkv");
    let moved = directory.path().join("moved.mkv");
    std::fs::write(&source, b"filmstrip source content").unwrap();
    let path = directory.path().join("strip.cutcap");
    let mut s = session(Duration::from_millis(60));
    ready(&mut s);
    s.import(source.clone()).unwrap();
    let v = pump_until(&mut s, |v| v["frame"].is_object());
    let media: MediaId = serde_json::from_value(v["media"][0]["id"].clone()).unwrap();
    let track: TrackId = serde_json::from_value(v["tracks"][0]["id"].clone()).unwrap();
    s.place_clip(media, track, TimelineSeek::Milliseconds(0))
        .unwrap();
    let row = wait(&mut s, |v| {
        v["pending"] == 0 && v["clips"].as_array().is_some_and(|c| c.len() == 1)
    })["clips"][0]
        .clone();
    // A request in flight while the project is replaced is discarded.
    s.request_thumbnails(&serde_json::json!([want(1, &row, 0)]).to_string())
        .unwrap();
    saved(&mut s, &path);
    s.new_project().unwrap();
    ready(&mut s);
    thread::sleep(Duration::from_millis(150));
    s.pump();
    assert!(s.next_thumbnail().is_none());
    // Reopen with the source moved: missing → no key → nothing is requested.
    std::fs::rename(&source, &moved).unwrap();
    s.open_project(path).unwrap();
    let missing = wait(&mut s, |v| {
        v["clips"][0]["availability"] == "missing" && v["pending"] == 0
    });
    assert_eq!(missing["clips"][0]["source_key"], "");
    s.request_thumbnails(&serde_json::json!([want(2, &row, 0)]).to_string())
        .unwrap();
    thread::sleep(Duration::from_millis(120));
    s.pump();
    assert!(
        s.next_thumbnail().is_none(),
        "old key of a missing source is refused"
    );
    // Relink: a new key; the old key stays refused, the new one decodes.
    s.relink_media(media, moved).unwrap();
    let relinked = wait(&mut s, |v| {
        v["clips"][0]["availability"] == "ready" && v["pending"] == 0
    });
    let new_key = relinked["clips"][0]["source_key"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_ne!(new_key, row["source_key"].as_str().unwrap());
    s.request_thumbnails(
        &serde_json::json!([want(3, &row, 0), want(4, &relinked["clips"][0], 0)]).to_string(),
    )
    .unwrap();
    let results = collect(&mut s, 1);
    assert_eq!(results, [(4, Status::Ready, Some(0))]);
    let started = Instant::now();
    s.request_thumbnails(&serde_json::json!([want(5, &relinked["clips"][0], 500)]).to_string())
        .unwrap();
    s.shutdown().unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "shutdown during a thumbnail batch is bounded"
    );
}

#[test]
fn abi_thumbnail_exports_validate_and_copy_by_token() {
    let handle = register_for_test(session(Duration::ZERO));
    abi_pump_until(handle, |v| v["engine_ready"] == true);
    let bad = b"[{\"token\":1}]";
    // SAFETY (test): local slices, or null with a nonzero length.
    unsafe {
        assert_eq!(
            cutcap_session_request_thumbnails(handle, bad.as_ptr(), bad.len()),
            CUTCAP_INVALID_ARGUMENT
        );
        assert_eq!(
            cutcap_session_request_thumbnails(handle, std::ptr::null(), 4),
            CUTCAP_INVALID_ARGUMENT
        );
        let mut info = CutCapThumbnailInfo::default();
        assert_eq!(
            cutcap_session_next_thumbnail(handle, &mut info),
            CUTCAP_NO_FRAME
        );
        assert_eq!(
            cutcap_session_next_thumbnail(handle, std::ptr::null_mut()),
            CUTCAP_INVALID_ARGUMENT
        );
        let mut buffer = [0u8; 16];
        assert_eq!(
            cutcap_session_copy_thumbnail(handle, 99, buffer.as_mut_ptr(), buffer.len(), 8),
            CUTCAP_STALE_FRAME
        );
    }
    assert_eq!(std::mem::size_of::<CutCapThumbnailInfo>(), 40);
    assert_eq!(cutcap_session_destroy(handle), CUTCAP_OK);
}

#[test]
fn abi_round_trip_announces_then_copies_real_rows() {
    let mut s = session(Duration::ZERO);
    let (row, _, _, _) = placed(&mut s);
    let handle = register_for_test(s);
    let wants = serde_json::json!([want(42, &row, 500_000)]).to_string();
    // SAFETY (test): valid local slices and outputs.
    unsafe {
        assert_eq!(
            cutcap_session_request_thumbnails(handle, wants.as_ptr(), wants.len()),
            CUTCAP_OK
        );
        let mut info = CutCapThumbnailInfo::default();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let mut changed = 0u32;
            cutcap_session_pump(handle, &mut changed);
            if cutcap_session_next_thumbnail(handle, &mut info) == CUTCAP_OK {
                break;
            }
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(2));
        }
        assert_eq!((info.token, info.status, info.actual_ticks), (42, 0, 500));
        assert_eq!((info.timebase_num, info.timebase_den), (1, 1000));
        assert_eq!(
            (info.width, info.height, info.stride),
            (71, 40, 284),
            "16:9 at 40 px"
        );
        let stride = info.stride as usize + 16;
        let mut buffer = vec![0u8; stride * info.height as usize];
        assert_eq!(
            cutcap_session_copy_thumbnail(handle, 41, buffer.as_mut_ptr(), buffer.len(), stride),
            CUTCAP_STALE_FRAME
        );
        assert_eq!(
            cutcap_session_copy_thumbnail(handle, 42, buffer.as_mut_ptr(), 10, stride),
            CUTCAP_INVALID_ARGUMENT
        );
        // A rejected destination leaves the thumbnail announced: the corrected copy succeeds.
        assert_eq!(
            cutcap_session_copy_thumbnail(handle, 42, buffer.as_mut_ptr(), buffer.len(), stride),
            CUTCAP_OK
        );
        assert_eq!(&buffer[..4], &[500u16.rem_euclid(256) as u8, 40, 60, 255]);
        assert_eq!(
            &buffer[stride..stride + 4],
            &[244, 40, 60, 255],
            "second row honours the stride"
        );
        assert_eq!(
            cutcap_session_copy_thumbnail(handle, 42, buffer.as_mut_ptr(), buffer.len(), stride),
            CUTCAP_STALE_FRAME
        );
    }
    assert_eq!(cutcap_session_destroy(handle), CUTCAP_OK);
}
