//! Phase 4B waveform peaks: worker batching/throttling/cancellation, session
//! validation and stale-result gates, linked placement rows, and the C ABI.
//! Controlled backends only; real peaks against independently decoded PCM are
//! in the cutcap-media audio integration suite.
use super::*;
use crate::waveforms::{Job, Status, WaveformService};
use cutcap_contracts::*;
use std::sync::Mutex;

/// Blocks each batch until released; records batches and cancellations.
#[derive(Default)]
struct Gate {
    open: Mutex<bool>,
    batches: Mutex<Vec<Vec<i64>>>,
    cancelled: AtomicUsize,
}
impl MediaBackend for Gate {
    fn probe(&self, _: &Path, _: &Cancellation) -> Result<ProbeResult, MediaError> {
        Err(MediaError::Worker)
    }
    fn decode(&self, _: &DecodeRequest, _: &Cancellation) -> Result<DecodedFrame, MediaError> {
        Err(MediaError::Worker)
    }
    fn audio_peaks(
        &self,
        _: &MediaSummary,
        _: u32,
        first_chunk: i64,
        count: usize,
        cancel: &Cancellation,
    ) -> Result<Vec<cutcap_media::Peaks>, MediaError> {
        self.batches
            .lock()
            .unwrap()
            .push((first_chunk..first_chunk + count as i64).collect());
        while !*self.open.lock().unwrap() {
            if cancel.is_cancelled() {
                self.cancelled.fetch_add(1, Ordering::SeqCst);
                return Err(MediaError::Cancelled);
            }
            thread::sleep(Duration::from_millis(1));
        }
        Ok((0..count as i64)
            .map(|k| cutcap_media::Peaks {
                chunk: first_chunk + k,
                rate: 48_000,
                channels: 2,
                min: vec![0.0; 4],
                max: vec![0.0; 4],
                process_id: 3,
            })
            .collect())
    }
}

fn av_media() -> MediaSummary {
    MediaSummary {
        id: MediaId::new(),
        path: "C:/av.mkv".into(),
        metadata: parse_probe(AV_PROBE).unwrap().into(),
    }
}

fn job(token: u64, media: &MediaSummary, key: &str, chunk: i64) -> Job {
    Job {
        token,
        media: media.clone(),
        audio_key: key.into(),
        stream: 1,
        chunk,
    }
}

fn until(done: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !done() {
        assert!(Instant::now() < deadline, "condition not reached");
        thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn consecutive_chunks_batch_playback_throttles_and_unwanted_batches_cancel() {
    let gate = Arc::new(Gate::default());
    let mut service = WaveformService::start(gate.clone()).unwrap();
    let media = av_media();
    // Chunks 0..6 of one stream: batches of up to four consecutive chunks.
    service.set_demand((0..6).map(|c| job(c as u64, &media, "K", c)).collect());
    until(|| gate.batches.lock().unwrap().len() == 1);
    assert_eq!(gate.batches.lock().unwrap()[0], [0, 1, 2, 3]);
    // A new demand without the running chunks cancels the batch.
    service.set_demand(vec![job(40, &media, "K", 40)]);
    until(|| gate.cancelled.load(Ordering::SeqCst) == 1);
    until(|| gate.batches.lock().unwrap().len() == 2);
    assert_eq!(gate.batches.lock().unwrap()[1], [40]);
    *gate.open.lock().unwrap() = true;
    until(|| service.stats().analysed == 1);
    let done = service.take().unwrap();
    assert_eq!((done.token, done.status), (40, Status::Ready));
    assert_eq!(done.peaks.unwrap().chunk, 40);
    // During playback a batch holds a single chunk.
    service.set_throttled(true);
    service.set_demand((10..13).map(|c| job(c as u64, &media, "K", c)).collect());
    until(|| service.stats().analysed == 4);
    let batches = gate.batches.lock().unwrap().clone();
    assert!(batches[2..].iter().all(|b| b.len() == 1), "{batches:?}");
    assert!(service.stats().throttled);
    service.shutdown();
}

/// One linked clip at 1 s from an "av" source; returns the audio row.
fn linked(session: &mut Session) -> (Value, MediaId) {
    let v = imported(session, "av take.mkv");
    let media: MediaId = serde_json::from_value(v["media"][0]["id"].clone()).unwrap();
    let track: TrackId = serde_json::from_value(v["tracks"][0]["id"].clone()).unwrap();
    session
        .place_clip(media, track, TimelineSeek::Milliseconds(1000))
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        session.pump();
        let timeline: Value = serde_json::from_str(session.timeline_json().unwrap()).unwrap();
        if timeline["clips"].as_array().is_some_and(|c| c.len() == 2) {
            let audio = timeline["clips"]
                .as_array()
                .unwrap()
                .iter()
                .find(|c| c["stream"] == "audio")
                .unwrap()
                .clone();
            return (audio, media);
        }
        assert!(Instant::now() < deadline, "linked placement did not land");
        thread::sleep(Duration::from_millis(5));
    }
}

fn want(token: u64, row: &Value, chunk: i64) -> Value {
    serde_json::json!({
        "token": token,
        "media_id": row["media_id"],
        "audio_key": row["audio_key"],
        "stream": row["audio_stream"],
        "chunk": chunk,
    })
}

fn collect(session: &mut Session, count: usize) -> Vec<(u64, Status, Option<i64>, f32)> {
    let mut results = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(5);
    while results.len() < count {
        session.pump();
        if let Some(done) = session.next_waveform() {
            results.push((
                done.token,
                done.status,
                done.peaks.as_ref().map(|p| p.chunk),
                done.peaks.as_ref().map_or(0.0, |p| p.max[0]),
            ));
        }
        assert!(Instant::now() < deadline, "only {results:?}");
        thread::sleep(Duration::from_millis(2));
    }
    results.sort_by_key(|r| r.0);
    results
}

#[test]
fn session_validates_audio_keys_delivers_peaks_and_drops_stale_results() {
    let mut s = session(Duration::ZERO);
    let (row, media) = linked(&mut s);
    assert!(!row["audio_key"].as_str().unwrap().is_empty());
    assert_eq!(row["audio_rate"], 48_000);
    let wants = serde_json::json!([
        want(1, &row, 0),
        want(2, &row, 1),
        { "token": 3, "media_id": row["media_id"], "audio_key": "stale", "stream": 1, "chunk": 0 },
        { "token": 4, "media_id": row["media_id"], "audio_key": row["audio_key"], "stream": 1, "chunk": -1 },
        { "token": 5, "media_id": row["media_id"], "audio_key": row["audio_key"], "stream": 9, "chunk": 0 },
    ]);
    s.request_waveforms(&wants.to_string()).unwrap();
    let results = collect(&mut s, 2);
    assert_eq!(
        results,
        [
            (1, Status::Ready, Some(0), 0.01),
            (2, Status::Ready, Some(1), 0.02)
        ]
    );
    thread::sleep(Duration::from_millis(30));
    s.pump();
    assert!(
        s.next_waveform().is_none(),
        "stale key, bad chunk and unknown stream were dropped"
    );
    assert!(s.request_waveforms("{bad").is_err());
    let too_many = serde_json::Value::Array(vec![want(9, &row, 0); 129]);
    assert!(s.request_waveforms(&too_many.to_string()).is_err());
    // A replacement project discards anything still queued or waiting: once
    // it is in place, nothing requested for the old one is ever delivered.
    s.request_waveforms(&serde_json::json!([want(6, &row, 2)]).to_string())
        .unwrap();
    s.new_project().unwrap();
    pump_until(&mut s, |v| v["media"].as_array().is_some_and(Vec::is_empty));
    let deadline = Instant::now() + Duration::from_millis(300);
    while Instant::now() < deadline {
        s.pump();
        assert!(
            s.next_waveform().is_none(),
            "results never reach another project"
        );
        thread::sleep(Duration::from_millis(5));
    }
    let _ = media;
}

#[test]
fn abi_round_trip_announces_then_copies_interleaved_peaks() {
    assert_eq!(std::mem::size_of::<crate::abi::CutCapWaveformInfo>(), 40);
    let mut s = session(Duration::ZERO);
    let (row, _) = linked(&mut s);
    let handle = register_for_test(s);
    let wants = serde_json::json!([want(42, &row, 3)]).to_string();
    // SAFETY (test): valid local slices and outputs.
    unsafe {
        assert_eq!(
            cutcap_session_request_waveforms(handle, wants.as_ptr(), wants.len()),
            CUTCAP_OK
        );
        let mut info = crate::abi::CutCapWaveformInfo::default();
        assert_eq!(
            cutcap_session_next_waveform(handle, std::ptr::null_mut()),
            CUTCAP_INVALID_ARGUMENT
        );
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let mut changed = 0u32;
            cutcap_session_pump(handle, &mut changed);
            if cutcap_session_next_waveform(handle, &mut info) == CUTCAP_OK {
                break;
            }
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(2));
        }
        assert_eq!((info.token, info.status, info.chunk), (42, 0, 3));
        assert_eq!(
            (info.rate, info.channels, info.buckets, info.bucket_frames),
            (48_000, 2, 1024, 256)
        );
        let mut small = vec![0f32; 10];
        assert_eq!(
            cutcap_session_copy_waveform(handle, 42, small.as_mut_ptr(), small.len()),
            CUTCAP_INVALID_ARGUMENT
        );
        assert_eq!(
            cutcap_session_copy_waveform(handle, 41, small.as_mut_ptr(), small.len()),
            CUTCAP_STALE_FRAME
        );
        // The rejected copy left it announced: the corrected copy succeeds.
        let mut peaks = vec![9f32; 2048];
        assert_eq!(
            cutcap_session_copy_waveform(handle, 42, peaks.as_mut_ptr(), peaks.len()),
            CUTCAP_OK
        );
        assert_eq!(
            (peaks[0], peaks[1]),
            (-0.04, 0.04),
            "chunk 3: level 4/100, min then max"
        );
        assert_eq!((peaks[2046], peaks[2047]), (-0.04, 0.04));
        assert_eq!(
            cutcap_session_copy_waveform(handle, 42, peaks.as_mut_ptr(), peaks.len()),
            CUTCAP_STALE_FRAME
        );
    }
    assert_eq!(cutcap_session_destroy(handle), CUTCAP_OK);
}
