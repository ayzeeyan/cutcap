//! Phase 5A export measurements. Not a test: run by
//! `scripts/native-ui/export-bench.ps1`, in release, with real media. It reports
//! what each scenario did; the script adds process memory and CPU around it.
use cutcap_contracts::*;
use cutcap_engine::session::Session;
use cutcap_media::FfmpegBackend;
use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::Arc,
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

fn ms(value: i64) -> TimelineSeek {
    TimelineSeek::Milliseconds(value)
}

fn id<T: serde::de::DeserializeOwned>(value: &Value) -> T {
    serde_json::from_value(value.clone()).unwrap()
}

struct Editor {
    session: Session,
    video: TrackId,
}

impl Editor {
    fn new() -> Self {
        let mut session = Session::with_backend(Arc::new(FfmpegBackend::default())).unwrap();
        let v = pump(&mut session, 30, |v| {
            v["engine_ready"] == true && v["export"]["available"] == true
        });
        Self {
            session,
            video: id(&v["tracks"][0]["id"]),
        }
    }
    fn view(&mut self) -> Value {
        serde_json::from_str(self.session.view_json().unwrap()).unwrap()
    }
    fn settle(&mut self) -> Value {
        pump(&mut self.session, 60, |v| v["pending"] == 0)
    }
    fn import(&mut self, path: &Path) -> MediaId {
        let count = self.view()["media"].as_array().unwrap().len();
        self.session.import(path.to_owned()).unwrap();
        let v = pump(&mut self.session, 120, |v| {
            v["media"].as_array().unwrap().len() > count
                && v["import"]["busy"] == false
                && v["media"][count]["availability"] == "ready"
        });
        id(&v["media"][count]["id"])
    }
    /// One clip of `len` ms from `source_at` ms of a long source, at `at` ms.
    fn cut(&mut self, media: MediaId, at: i64, source_at: i64, len: i64) {
        // Placed far away first, because the whole source would overlap.
        let parked = 40_000_000 + at * 400;
        let before = self.clip_count();
        self.session
            .place_clip(media, self.video, ms(parked))
            .unwrap();
        self.settle();
        let clip = self.clip_at(parked);
        assert!(self.clip_count() > before, "placement refused");
        self.session
            .trim_clip(clip, ClipEdge::End, ms(parked + source_at + len))
            .unwrap();
        self.settle();
        if source_at > 0 {
            self.session
                .trim_clip(clip, ClipEdge::Start, ms(parked + source_at))
                .unwrap();
            self.settle();
        }
        self.session
            .move_clip(clip, self.video, ms(at), ClipEdge::Start)
            .unwrap();
        self.settle();
    }
    fn clip_count(&mut self) -> usize {
        let t: Value = serde_json::from_str(self.session.timeline_json().unwrap()).unwrap();
        t["clips"].as_array().unwrap().len()
    }
    fn clip_at(&mut self, at_ms: i64) -> ClipId {
        let t: Value = serde_json::from_str(self.session.timeline_json().unwrap()).unwrap();
        let found = t["clips"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["stream"] == "video" && c["start_ticks"] == at_ms * 720)
            .cloned()
            .unwrap();
        id(&found["id"])
    }
}

fn pump(session: &mut Session, seconds: u64, done: impl Fn(&Value) -> bool) -> Value {
    let deadline = Instant::now() + Duration::from_secs(seconds);
    loop {
        session.pump();
        let current: Value = serde_json::from_str(session.view_json().unwrap()).unwrap();
        if done(&current) {
            return current;
        }
        assert!(
            Instant::now() < deadline,
            "timed out: {}",
            current["export"]["job"]
        );
        thread::sleep(Duration::from_millis(2));
    }
}

fn mark(text: &str) {
    if let Some(path) = std::env::var_os("CUTCAP_BENCH_MARKS") {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis();
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        writeln!(file, "{text} {now}").unwrap();
    }
}

/// Runs one export and reports what it did.
fn measure(e: &mut Editor, name: &str, folder: &Path) -> Value {
    let path: PathBuf = folder.join(format!("{name}.mp4"));
    let _ = fs::remove_file(&path);
    let started = Instant::now();
    let mut first_state = json!({});
    let mut prepared = None;
    let mut first_frame = None;
    mark(&format!("{name} start"));
    e.session
        .start_export(path.clone(), Some(e.video), false, true)
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(1200);
    let done = loop {
        e.session.pump();
        let v = e.view();
        let job = &v["export"]["job"];
        if job["state"] == "preparing" && prepared.is_none() {
            prepared = Some(started.elapsed().as_millis() as u64);
        }
        if job["frames_encoded"].as_u64().unwrap_or(0) > 0 && first_frame.is_none() {
            first_frame = Some(started.elapsed().as_millis() as u64);
            first_state = job["state"].clone();
        }
        if job["running"] == false {
            break v;
        }
        assert!(Instant::now() < deadline, "{name} timed out");
        thread::sleep(Duration::from_millis(5));
    };
    mark(&format!("{name} stop"));
    let job = done["export"]["job"].clone();
    assert_eq!(job["state"], "completed", "{name}: {job}");
    let result = job["result"].clone();
    let n = |key: &str| result[key].as_f64().unwrap_or(0.0);
    let (frames, encode_s) = (n("frames"), n("encode_us") / 1e6);
    let seconds = frames / 30.0;
    json!({
        "name": name,
        "output_seconds": seconds,
        "frames": frames,
        "total_s": n("total_us") / 1e6,
        "validate_ms": prepared,
        "first_frame_encoded_ms": first_frame,
        "state_at_first_frame": first_state,
        "mix_s": n("mix_us") / 1e6,
        "mix_times_real_time": if n("mix_us") > 0.0 { seconds / (n("mix_us") / 1e6) } else { 0.0 },
        "encode_s": encode_s,
        "encoded_fps": frames / encode_s,
        "encode_times_real_time": seconds / encode_s,
        "whole_job_times_real_time": seconds / (n("total_us") / 1e6),
        "decoded_frames": n("decoded_frames"),
        "reused_frames": n("reused_frames"),
        "video_decoder_starts": n("video_decoder_starts"),
        "clip_changes": n("clip_changes"),
        "cut_wait_max_ms": n("cut_wait_max_us") / 1000.0,
        "cut_wait_mean_ms": if n("clip_changes") > 0.0 { n("cut_wait_total_us") / 1000.0 / n("clip_changes") } else { 0.0 },
        "audio_decoder_starts": n("audio_decoder_starts"),
        "audio_decoders_peak": n("audio_decoders_peak"),
        "pcm_scratch_mib": frames * 1600.0 * 8.0 / 1_048_576.0,
        "output_mib": n("bytes") / 1_048_576.0,
        "bitrate_mbps": n("bytes") * 8.0 / seconds / 1e6,
    })
}

#[test]
#[ignore = "export measurements; run by scripts/native-ui/export-bench.ps1"]
fn measure_export_scenarios() {
    let Some(root) = std::env::var_os("CUTCAP_BENCH_SOURCES") else {
        return;
    };
    let root = PathBuf::from(root);
    let out = tempfile::tempdir_in(&root).unwrap();
    let av = root.join("av long 1080p.mp4");
    let mut rows = Vec::new();

    let mut e = Editor::new();
    let media = e.import(&av);
    for i in 0..6 {
        e.cut(media, i * 5000, 10_000 + i * 21_000, 5000);
    }
    rows.push(measure(&mut e, "edited_30s_1080p_6_clips", out.path()));

    let mut e = Editor::new();
    let media = e.import(&av);
    for i in 0..12 {
        e.cut(media, i * 5000, 3000 + i * 13_000, 5000);
    }
    rows.push(measure(&mut e, "edited_60s_1080p_12_clips", out.path()));

    let mut e = Editor::new();
    let media = e.import(&av);
    let video = e.video;
    e.session.place_clip(media, video, ms(0)).unwrap();
    e.settle();
    rows.push(measure(&mut e, "long_180s_1080p_single_clip", out.path()));

    let mut e = Editor::new();
    let media = e.import(&root.join("av vfr 720p.mkv"));
    let video = e.video;
    e.session.place_clip(media, video, ms(0)).unwrap();
    e.settle();
    rows.push(measure(&mut e, "vfr_720p", out.path()));

    let mut e = Editor::new();
    let media = e.import(&root.join("av sync 4k.mp4"));
    let video = e.video;
    e.session.place_clip(media, video, ms(0)).unwrap();
    e.settle();
    rows.push(measure(&mut e, "source_4k", out.path()));

    let mut e = Editor::new();
    let media = e.import(&av);
    let video = e.video;
    let tone = e.import(&root.join("bench tone.m4a"));
    e.cut(media, 0, 5000, 30_000);
    let mut track;
    for _ in 0..7 {
        let count = e.view()["tracks"].as_array().unwrap().len();
        e.session.add_track(TrackKind::Audio).unwrap();
        let v = pump(&mut e.session, 20, |v| {
            v["pending"] == 0 && v["tracks"].as_array().unwrap().len() > count
        });
        track = id(&v["tracks"][count]["id"]);
        e.session
            .place_audio_stream(tone, 0, Some(track), ms(0))
            .unwrap();
        e.settle();
    }
    let _ = video;
    rows.push(measure(&mut e, "audio_8_tracks_30s", out.path()));

    let mut e = Editor::new();
    let media = e.import(&av);
    for i in 0..240 {
        e.cut(media, i * 250, 2000 + (i * 613) % 170_000, 250);
    }
    rows.push(measure(&mut e, "many_cuts_60s_240_clips", out.path()));

    // Cancelling and closing while encoding.
    let mut e = Editor::new();
    let media = e.import(&av);
    let video = e.video;
    e.session.place_clip(media, video, ms(0)).unwrap();
    e.settle();
    e.session
        .start_export(out.path().join("cancel.mp4"), Some(video), false, true)
        .unwrap();
    let running = pump(&mut e.session, 60, |v| {
        v["export"]["job"]["frames_encoded"].as_u64().unwrap_or(0) > 60
    });
    let job = running["export"]["job"]["id"].as_str().unwrap().to_owned();
    let began = Instant::now();
    e.session.cancel_export(&job).unwrap();
    pump(&mut e.session, 30, |v| {
        v["export"]["job"]["state"] == "cancelled"
    });
    let cancel_ms = began.elapsed().as_millis() as u64;

    let mut e = Editor::new();
    let media = e.import(&av);
    let video = e.video;
    e.session.place_clip(media, video, ms(0)).unwrap();
    e.settle();
    e.session
        .start_export(out.path().join("close.mp4"), Some(video), false, true)
        .unwrap();
    pump(&mut e.session, 60, |v| {
        v["export"]["job"]["frames_encoded"].as_u64().unwrap_or(0) > 60
    });
    let began = Instant::now();
    e.session.shutdown().unwrap();
    let shutdown_ms = began.elapsed().as_millis() as u64;
    drop(e);

    let report = json!({ "scenarios": rows, "cancel_ms": cancel_ms, "shutdown_during_encode_ms": shutdown_ms });
    println!("BENCH_JSON {report}");
}
