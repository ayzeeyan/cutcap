//! Phase 5A against the real external FFmpeg, through the real session: real
//! projects are edited and exported, then the MP4 is decoded independently by
//! the FFmpeg/FFprobe CLI and compared with what the timeline must contain.
//! H.264 and AAC are lossy, so pictures and tones are compared with tolerances.
//! Run with `cargo test -p cutcap-ffi --test export_media -- --ignored --test-threads=1`.
use cutcap_contracts::*;
use cutcap_engine::session::Session;
use cutcap_media::{FfmpegBackend, MediaBackend};
use serde_json::Value;
use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

fn ms(value: i64) -> TimelineSeek {
    TimelineSeek::Milliseconds(value)
}

fn run_tool(tool: &str, args: &[&str], path: Option<&Path>, tail: &[&str]) -> Vec<u8> {
    let mut command = Command::new(tool);
    command.args(args);
    if let Some(path) = path {
        command.arg(path);
    }
    command.args(tail);
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{tool} {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
        .join(name)
}

/// A source whose picture changes color every `step` seconds, with one tone.
fn colored(dir: &Path, name: &str, colors: &[&str], step: f64, tone: u32, fps: u32) -> PathBuf {
    let path = dir.join(name);
    let total = colors.len() as f64 * step;
    let mut video = format!("color=c=black:s=640x360:r={fps}:d={total}");
    for (i, color) in colors.iter().enumerate() {
        let (a, b) = (i as f64 * step, (i + 1) as f64 * step);
        video += &format!(",drawbox=c={color}:t=fill:enable='gte(t,{a})*lt(t,{b})'");
    }
    let audio = format!("sine=frequency={tone}:sample_rate=48000:duration={total},volume=2");
    run_tool(
        "ffmpeg",
        &[
            "-hide_banner",
            "-v",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            &video,
            "-f",
            "lavfi",
            "-i",
            &audio,
            "-map",
            "0:v",
            "-map",
            "1:a",
            "-c:v",
            "libx264",
            "-g",
            "48",
            "-bf",
            "2",
            "-crf",
            "18",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
            "-b:a",
            "256k",
        ],
        Some(&path),
        &[],
    );
    path
}

fn tone_wav(dir: &Path, name: &str, hz: u32, seconds: f64) -> PathBuf {
    let path = dir.join(name);
    let audio = format!("sine=frequency={hz}:sample_rate=48000:duration={seconds},volume=2");
    run_tool(
        "ffmpeg",
        &[
            "-hide_banner",
            "-v",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            &audio,
            "-c:a",
            "pcm_s16le",
        ],
        Some(&path),
        &[],
    );
    path
}

struct Editor {
    session: Session,
    dir: tempfile::TempDir,
    video: TrackId,
    audio: TrackId,
}

fn id<T: serde::de::DeserializeOwned>(value: &Value) -> T {
    serde_json::from_value(value.clone()).unwrap()
}

impl Editor {
    fn new() -> Self {
        let mut session = Session::with_backend(Arc::new(FfmpegBackend::default())).unwrap();
        let v = pump_until(&mut session, 20, |v| {
            v["engine_ready"] == true && v["export"]["available"] == true
        });
        Self {
            session,
            dir: tempfile::tempdir().unwrap(),
            video: id(&v["tracks"][0]["id"]),
            audio: id(&v["tracks"][1]["id"]),
        }
    }
    fn view(&mut self) -> Value {
        serde_json::from_str(self.session.view_json().unwrap()).unwrap()
    }
    fn clips(&mut self) -> Vec<Value> {
        let timeline: Value = serde_json::from_str(self.session.timeline_json().unwrap()).unwrap();
        timeline["clips"].as_array().unwrap().clone()
    }
    fn settle(&mut self) -> Value {
        pump_until(&mut self.session, 20, |v| v["pending"] == 0)
    }
    fn import(&mut self, path: &Path) -> MediaId {
        let count = self.view()["media"].as_array().unwrap().len();
        self.session.import(path.to_owned()).unwrap();
        let v = pump_until(&mut self.session, 30, |v| {
            v["media"].as_array().unwrap().len() > count
                && v["import"]["busy"] == false
                && v["media"][count]["availability"] == "ready"
        });
        id(&v["media"][count]["id"])
    }
    /// Places a source; the picture clip of the placement is returned.
    fn place(&mut self, media: MediaId, track: TrackId, at_ms: i64) -> ClipId {
        let before = self.clips().len();
        self.session.place_clip(media, track, ms(at_ms)).unwrap();
        pump_until(&mut self.session, 20, |v| v["pending"] == 0);
        let clips = self.clips();
        assert!(clips.len() > before, "placement was refused");
        let found = clips
            .iter()
            .rev()
            .find(|c| c["stream"] == "video" && c["start_ticks"] == at_ms * 720)
            .unwrap_or_else(|| panic!("no clip at {at_ms}: {clips:?}"));
        id(&found["id"])
    }
    fn add_audio_track(&mut self) -> TrackId {
        let count = self.view()["tracks"].as_array().unwrap().len();
        self.session.add_track(TrackKind::Audio).unwrap();
        let v = pump_until(&mut self.session, 20, |v| {
            v["pending"] == 0 && v["tracks"].as_array().unwrap().len() > count
        });
        id(&v["tracks"][count]["id"])
    }
    /// Runs one export to completion and returns the job view.
    fn export(&mut self, path: &Path, overwrite: bool) -> Value {
        self.session
            .start_export(path.to_owned(), Some(self.video), overwrite, true)
            .unwrap();
        let v = pump_until(&mut self.session, 240, |v| {
            v["export"]["job"]["running"] == false
        });
        let job = v["export"]["job"].clone();
        assert_eq!(job["state"], "completed", "{job}");
        job
    }
    fn output(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }
    fn leftovers(&self) -> Vec<String> {
        std::fs::read_dir(self.dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(".cutcap-"))
            .collect()
    }
}

fn pump_until(session: &mut Session, seconds: u64, done: impl Fn(&Value) -> bool) -> Value {
    let deadline = Instant::now() + Duration::from_secs(seconds);
    loop {
        session.pump();
        let current: Value = serde_json::from_str(session.view_json().unwrap()).unwrap();
        if done(&current) {
            return current;
        }
        assert!(
            Instant::now() < deadline,
            "timed out; export {}",
            current["export"]["job"]
        );
        thread::sleep(Duration::from_millis(10));
    }
}

/// The decoded picture shown at `seconds`, as 1280x720 RGB, via plain FFmpeg.
fn picture(path: &Path, seconds: f64) -> Vec<u8> {
    let time = format!("{seconds:.4}");
    run_tool(
        "ffmpeg",
        &["-hide_banner", "-v", "error", "-ss", &time, "-i"],
        Some(path),
        &["-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "rgb24", "-"],
    )
}

fn mean_rgb(image: &[u8], area: (usize, usize, usize, usize)) -> (f64, f64, f64) {
    let (x0, y0, x1, y1) = area;
    let mut sum = (0u64, 0u64, 0u64);
    let mut count = 0u64;
    for y in y0..y1 {
        for x in x0..x1 {
            let at = (y * 1280 + x) * 3;
            sum.0 += u64::from(image[at]);
            sum.1 += u64::from(image[at + 1]);
            sum.2 += u64::from(image[at + 2]);
            count += 1;
        }
    }
    let n = count as f64;
    (sum.0 as f64 / n, sum.1 as f64 / n, sum.2 as f64 / n)
}

const CENTRE: (usize, usize, usize, usize) = (600, 320, 680, 400);

fn assert_rgb(found: (f64, f64, f64), wanted: (f64, f64, f64), what: &str) {
    let near = |a: f64, b: f64| (a - b).abs() < 32.0;
    assert!(
        near(found.0, wanted.0) && near(found.1, wanted.1) && near(found.2, wanted.2),
        "{what}: found {found:?}, wanted {wanted:?}"
    );
}

const RED: (f64, f64, f64) = (255.0, 0.0, 0.0);
const LIME: (f64, f64, f64) = (0.0, 255.0, 0.0);
const BLUE: (f64, f64, f64) = (0.0, 0.0, 255.0);
const WHITE: (f64, f64, f64) = (255.0, 255.0, 255.0);
const YELLOW: (f64, f64, f64) = (255.0, 255.0, 0.0);
const CYAN: (f64, f64, f64) = (0.0, 255.0, 255.0);
const MAGENTA: (f64, f64, f64) = (255.0, 0.0, 255.0);
const BLACK: (f64, f64, f64) = (0.0, 0.0, 0.0);

/// Mono `f32` PCM at 48 kHz, decoded independently, edit list honoured.
fn decoded_audio(path: &Path) -> Vec<f32> {
    run_tool(
        "ffmpeg",
        &["-hide_banner", "-v", "error", "-i"],
        Some(path),
        &[
            "-map",
            "0:a:0",
            "-ac",
            "1",
            "-ar",
            "48000",
            "-f",
            "f32le",
            "-c:a",
            "pcm_f32le",
            "-",
        ],
    )
    .as_chunks::<4>()
    .0
    .iter()
    .map(|b| f32::from_le_bytes(*b))
    .collect()
}

/// The amplitude of a pure frequency over `[from, to)` seconds (Goertzel).
fn tone(pcm: &[f32], from: f64, to: f64, hz: f64) -> f64 {
    let (a, b) = ((from * 48_000.0) as usize, (to * 48_000.0) as usize);
    let window = &pcm[a..b.min(pcm.len())];
    let w = 2.0 * std::f64::consts::PI * hz / 48_000.0;
    let (mut re, mut im) = (0.0, 0.0);
    for (n, sample) in window.iter().enumerate() {
        let phase = w * n as f64;
        re += f64::from(*sample) * phase.cos();
        im -= f64::from(*sample) * phase.sin();
    }
    2.0 * (re * re + im * im).sqrt() / window.len() as f64
}

fn probe(path: &Path) -> Value {
    serde_json::from_slice(&run_tool(
        "ffprobe",
        &["-v", "error", "-show_entries"],
        None,
        &[
            "stream=codec_type,codec_name,profile,width,height,pix_fmt,nb_frames,avg_frame_rate,r_frame_rate,sample_rate,channels,duration,start_time,color_space,color_primaries,color_transfer,color_range:format=format_name,duration",
            "-of",
            "json",
            path.to_str().unwrap(),
        ],
    ))
    .unwrap()
}

#[test]
#[ignore = "needs external FFmpeg with libx264"]
fn an_edited_multitrack_timeline_is_what_the_file_contains() {
    let mut e = Editor::new();
    let dir = e.dir.path().to_owned();
    let a = colored(
        &dir,
        "a.mp4",
        &["red", "lime", "blue", "white"],
        1.0,
        440,
        30,
    );
    let b = colored(&dir, "b.mp4", &["yellow", "cyan", "magenta"], 1.0, 880, 24);
    let c = tone_wav(&dir, "c.wav", 1320, 2.0);
    let d = tone_wav(&dir, "d.wav", 1760, 7.0);
    let (a, b, c, d) = (e.import(&a), e.import(&b), e.import(&c), e.import(&d));
    let (video, audio) = (e.video, e.audio);
    // A: trim the first second away and move it to the start: lime, blue, white.
    let clip = e.place(a, video, 0);
    e.session
        .trim_clip(clip, ClipEdge::Start, ms(1000))
        .unwrap();
    e.settle();
    e.session
        .move_clip(clip, video, ms(0), ClipEdge::Start)
        .unwrap();
    e.settle();
    // B after a gap, split, with the second part moved later: another gap.
    let clip = e.place(b, video, 3500);
    e.session.split_clip(clip, ms(4500)).unwrap();
    e.settle();
    let right = e
        .clips()
        .iter()
        .find(|c| c["stream"] == "video" && c["start_ticks"] == 4500 * 720)
        .map(|c| id::<ClipId>(&c["id"]))
        .unwrap();
    e.session
        .move_clip(right, video, ms(5000), ClipEdge::Start)
        .unwrap();
    e.settle();
    // Independent audio at 1 s, 6 dB down, and a muted track that must vanish.
    let second = e.add_audio_track();
    e.session
        .place_audio_stream(c, 0, Some(second), ms(1000))
        .unwrap();
    e.settle();
    e.session.commit_track_gain(second, -600).unwrap();
    e.settle();
    let third = e.add_audio_track();
    e.session
        .place_audio_stream(d, 0, Some(third), ms(0))
        .unwrap();
    e.settle();
    e.session.set_track_mute(third, true).unwrap();
    e.settle();
    let _ = audio;

    let out = e.output("edited export.mp4");
    let job = e.export(&out, false);
    assert_eq!(job["frames_total"], 210);
    assert_eq!(e.leftovers(), Vec::<String>::new());

    let info = probe(&out);
    let streams = info["streams"].as_array().unwrap();
    let v = streams.iter().find(|s| s["codec_type"] == "video").unwrap();
    let s = streams.iter().find(|s| s["codec_type"] == "audio").unwrap();
    assert_eq!(v["codec_name"], "h264");
    assert_eq!(
        (v["width"].as_u64(), v["height"].as_u64()),
        (Some(1280), Some(720))
    );
    assert_eq!(v["pix_fmt"], "yuv420p");
    assert_eq!(v["avg_frame_rate"], "30/1");
    assert_eq!(v["nb_frames"], "210");
    assert_eq!(v["color_space"], "bt709");
    assert_eq!(v["color_primaries"], "bt709");
    assert_eq!(v["color_transfer"], "bt709");
    assert_eq!(v["color_range"], "tv");
    assert_eq!(s["codec_name"], "aac");
    assert_eq!(s["sample_rate"], "48000");
    assert_eq!(s["channels"], 2);
    let duration: f64 = info["format"]["duration"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    assert!((duration - 7.0).abs() < 0.05, "duration {duration}");

    for (time, wanted, what) in [
        (0.5, LIME, "A from its in-point"),
        (1.5, BLUE, "A second 2"),
        (2.5, WHITE, "A last second"),
        (3.25, BLACK, "gap before B"),
        (4.0, YELLOW, "B first part"),
        (4.75, BLACK, "gap after the split"),
        (5.5, CYAN, "B second part"),
        (6.5, MAGENTA, "B last second"),
    ] {
        assert_rgb(mean_rgb(&picture(&out, time), CENTRE), wanted, what);
    }

    let pcm = decoded_audio(&out);
    let present = |from: f64, to: f64, hz: f64| {
        let level = tone(&pcm, from, to, hz);
        assert!(level > 0.03, "{hz} Hz expected in {from}..{to}: {level}");
    };
    let absent = |from: f64, to: f64, hz: f64| {
        let level = tone(&pcm, from, to, hz);
        assert!(level < 0.01, "{hz} Hz unexpected in {from}..{to}: {level}");
    };
    present(0.1, 2.9, 440.0);
    absent(3.1, 6.9, 440.0);
    absent(3.1, 3.4, 880.0);
    present(3.6, 4.4, 880.0);
    absent(4.6, 4.9, 880.0);
    present(5.1, 6.9, 880.0);
    absent(0.1, 0.9, 1320.0);
    present(1.1, 2.9, 1320.0);
    absent(3.1, 6.9, 1320.0);
    absent(0.1, 6.9, 1760.0);

    // A later export of a changed project shows the change.
    e.session.undo().unwrap();
    e.settle();
    let later = e.output("second export.mp4");
    let job = e.export(&later, false);
    assert_eq!(job["state"], "completed");
    let pcm = decoded_audio(&later);
    assert!(
        tone(&pcm, 0.1, 6.9, 1760.0) > 0.03,
        "undoing the mute is exported"
    );
}

#[test]
#[ignore = "needs external FFmpeg with libx264"]
fn gain_is_amplitude_in_the_encoded_file() {
    let mut e = Editor::new();
    let dir = e.dir.path().to_owned();
    let t = tone_wav(&dir, "t.wav", 1000, 2.0);
    let t = e.import(&t);
    let audio = e.audio;
    e.session
        .place_audio_stream(t, 0, Some(audio), ms(0))
        .unwrap();
    e.settle();
    let full = e.output("full.mp4");
    e.export(&full, false);
    e.session.commit_track_gain(audio, -1200).unwrap();
    e.settle();
    let cut = e.output("cut.mp4");
    e.export(&cut, false);
    let reference = tone(&decoded_audio(&full), 0.2, 1.8, 1000.0);
    let lowered = tone(&decoded_audio(&cut), 0.2, 1.8, 1000.0);
    let ratio = lowered / reference;
    let wanted = 10f64.powf(-12.0 / 20.0);
    assert!(
        (ratio / wanted - 1.0).abs() < 0.03,
        "ratio {ratio}, wanted {wanted}"
    );
    assert!(reference > 0.2, "{reference}");
    assert_eq!(
        mean_rgb(&picture(&full, 1.0), CENTRE).0.round(),
        0.0,
        "audio only: black"
    );
}

/// (frame times of bright frames, click times), from independent decoding.
fn flashes_and_clicks(path: &Path) -> (Vec<f64>, Vec<f64>) {
    let gray = run_tool(
        "ffmpeg",
        &["-hide_banner", "-v", "error", "-i"],
        Some(path),
        &[
            "-vf",
            "scale=4:4",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "gray",
            "-",
        ],
    );
    let bright: Vec<usize> = gray
        .chunks(16)
        .enumerate()
        .filter(|(_, f)| f.iter().map(|b| u64::from(*b)).sum::<u64>() / 16 > 128)
        .map(|(i, _)| i)
        .collect();
    let flashes = bright.iter().map(|i| *i as f64 / 30.0).collect();
    let pcm = decoded_audio(path);
    let mut clicks = Vec::new();
    let mut last = -1.0;
    for (i, s) in pcm.iter().enumerate() {
        let t = i as f64 / 48_000.0;
        if s.abs() > 0.2 && t - last > 0.5 {
            clicks.push(t);
            last = t;
        }
    }
    (flashes, clicks)
}

#[test]
#[ignore = "needs external FFmpeg with libx264"]
fn picture_and_sound_stay_together_through_a_trim_and_a_gap() {
    let mut e = Editor::new();
    let dir = e.dir.path().to_owned();
    let path = dir.join("sync.mp4");
    // A one-frame flash and a 10 ms click on every whole second.
    let video = "color=c=black:s=640x360:r=30:d=8,drawbox=c=white:t=fill:enable='lt(mod(n,30),1)'";
    let audio = "aevalsrc='if(lt(mod(t,1),0.01),0.8*sin(2*PI*3000*t),0)':s=48000:d=8";
    run_tool(
        "ffmpeg",
        &[
            "-hide_banner",
            "-v",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            video,
            "-f",
            "lavfi",
            "-i",
            audio,
            "-map",
            "0:v",
            "-map",
            "1:a",
            "-c:v",
            "libx264",
            "-g",
            "15",
            "-bf",
            "2",
            "-crf",
            "16",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
            "-b:a",
            "256k",
        ],
        Some(&path),
        &[],
    );
    let media = e.import(&path);
    let video = e.video;
    let clip = e.place(media, video, 0);
    e.session.trim_clip(clip, ClipEdge::Start, ms(500)).unwrap();
    e.settle();
    e.session
        .move_clip(clip, video, ms(250), ClipEdge::Start)
        .unwrap();
    e.settle();
    let out = e.output("sync export.mp4");
    e.export(&out, false);
    let (flashes, clicks) = flashes_and_clicks(&out);
    // Source second 1 lands at 0.75 s; there are 7 whole seconds inside.
    assert_eq!(flashes.len(), 7, "{flashes:?}");
    assert_eq!(clicks.len(), 7, "{clicks:?}");
    let offsets: Vec<f64> = flashes.iter().zip(&clicks).map(|(f, c)| c - f).collect();
    for (i, click) in clicks.iter().enumerate() {
        let wanted = 0.75 + i as f64;
        assert!(
            (click - wanted).abs() < 0.004,
            "click {i} at {click}, wanted {wanted}"
        );
    }
    for offset in &offsets {
        assert!(offset.abs() <= 1.0 / 30.0 + 0.003, "A/V offset {offset}");
    }
    // No cumulative drift: the first and last offsets are the same.
    let drift = offsets[6] - offsets[0];
    assert!(drift.abs() < 0.002, "drift {drift}: {offsets:?}");
}

#[test]
#[ignore = "needs external FFmpeg with libx264"]
fn variable_frame_rate_and_offset_sources_keep_their_real_times() {
    let mut e = Editor::new();
    let vfr = e.import(&fixture("colors-vfr.mkv"));
    let video = e.video;
    e.place(vfr, video, 0);
    let out = e.output("vfr.mp4");
    e.export(&out, false);
    // Real PTS 0, 200, 700, 1500, 2300 ms: red, green, blue, red, green.
    for (time, wanted) in [
        (0.1, RED),
        (0.5, LIME),
        (1.0, BLUE),
        (1.8, RED),
        (2.35, LIME),
    ] {
        let found = mean_rgb(&picture(&out, time), CENTRE);
        assert_rgb(found, wanted, &format!("VFR at {time}"));
    }
    // A stream starting at PTS 5000 still starts at its own first frame.
    let mut e = Editor::new();
    let offset = e.import(&fixture("offset.mkv"));
    let video = e.video;
    e.place(offset, video, 0);
    let out = e.output("offset.mp4");
    e.export(&out, false);
    let reference = run_tool(
        "ffmpeg",
        &["-hide_banner", "-v", "error", "-i"],
        Some(&fixture("offset.mkv")),
        &[
            "-frames:v",
            "1",
            "-vf",
            "scale=1280:720",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "rgb24",
            "-",
        ],
    );
    assert_rgb(
        mean_rgb(&picture(&out, 0.0), CENTRE),
        mean_rgb(&reference, CENTRE),
        "first frame of the offset source",
    );
}

#[test]
#[ignore = "needs external FFmpeg with libx264"]
fn a_trim_deep_inside_a_long_gop_b_frame_source_is_frame_correct() {
    let mut e = Editor::new();
    let dir = e.dir.path().to_owned();
    let path = dir.join("gop.mp4");
    run_tool(
        "ffmpeg",
        &[
            "-hide_banner",
            "-v",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=s=640x360:r=30:d=6",
            "-c:v",
            "libx264",
            "-g",
            "150",
            "-bf",
            "3",
            "-crf",
            "18",
            "-pix_fmt",
            "yuv420p",
        ],
        Some(&path),
        &[],
    );
    let media = e.import(&path);
    let video = e.video;
    let clip = e.place(media, video, 0);
    e.session
        .trim_clip(clip, ClipEdge::Start, ms(3000))
        .unwrap();
    e.settle();
    e.session
        .move_clip(clip, video, ms(0), ClipEdge::Start)
        .unwrap();
    e.settle();
    let out = e.output("gop export.mp4");
    let job = e.export(&out, false);
    assert_eq!(job["frames_total"], 90);
    for second in [0.0, 1.0, 2.5] {
        let exported = picture(&out, second);
        let original = run_tool(
            "ffmpeg",
            &[
                "-hide_banner",
                "-v",
                "error",
                "-ss",
                &format!("{:.4}", second + 3.0),
                "-i",
            ],
            Some(&path),
            &[
                "-frames:v",
                "1",
                "-vf",
                "scale=1280:720:flags=bicubic",
                "-f",
                "rawvideo",
                "-pix_fmt",
                "rgb24",
                "-",
            ],
        );
        let difference = exported
            .iter()
            .zip(&original)
            .map(|(a, b)| u64::from(a.abs_diff(*b)))
            .sum::<u64>() as f64
            / exported.len() as f64;
        assert!(
            difference < 6.0,
            "at {second} s the mean difference is {difference}"
        );
    }
}

#[test]
#[ignore = "needs external FFmpeg with libx264"]
fn every_picture_shape_fits_without_distortion() {
    let e = Editor::new();
    let dir = e.dir.path().to_owned();
    let make = |name: &str, size: &str, extra: &[&str]| {
        let path = dir.join(name);
        let source = format!("color=c=red:s={size}:r=30:d=1");
        let mut args = vec![
            "-hide_banner",
            "-v",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            source.as_str(),
        ];
        args.extend_from_slice(extra);
        args.extend_from_slice(&["-c:v", "libx264", "-pix_fmt", "yuv420p"]);
        run_tool("ffmpeg", &args, Some(&path), &[]);
        path
    };
    let four_three = make("fourthree.mp4", "640x480", &[]);
    let wide = make("wide.mp4", "1920x800", &[]);
    let portrait = make("portrait.mp4", "720x1280", &[]);
    let anamorphic = make("pal.mp4", "720x576", &["-vf", "setsar=64/45"]);
    let cases = [
        // (source, inside the picture, outside it)
        (four_three, (640, 360), Some((80, 360))),
        (wide, (640, 360), Some((640, 40))),
        (portrait, (640, 360), Some((200, 360))),
        (anamorphic, (640, 360), None),
    ];
    for (index, (source, inside, outside)) in cases.into_iter().enumerate() {
        let mut e2 = Editor::new();
        let media = e2.import(&source);
        let video = e2.video;
        e2.place(media, video, 0);
        let out = e2.output(&format!("shape{index}.mp4"));
        e2.export(&out, false);
        let image = picture(&out, 0.4);
        let at = |p: (usize, usize)| (p.0 - 8, p.1 - 8, p.0 + 8, p.1 + 8);
        assert_rgb(mean_rgb(&image, at(inside)), RED, "picture");
        match outside {
            Some(point) => assert_rgb(mean_rgb(&image, at(point)), BLACK, "letterbox"),
            // A full 16:9 display picture touches the frame's corners.
            None => assert_rgb(mean_rgb(&image, (10, 10, 30, 30)), RED, "full frame"),
        }
    }
    // Rotation metadata: the 320x180 fixture displays 90 degrees turned.
    let mut e3 = Editor::new();
    let rotated = e3.import(&fixture("rotated.mp4"));
    let video = e3.video;
    e3.place(rotated, video, 0);
    let out = e3.output("rotated.mp4");
    e3.export(&out, false);
    let image = picture(&out, 0.2);
    assert_rgb(
        mean_rgb(&image, (20, 340, 60, 380)),
        BLACK,
        "portrait pillarbox",
    );
    let brightness = mean_rgb(&image, (500, 200, 780, 500));
    assert!(
        brightness.0 + brightness.1 + brightness.2 > 60.0,
        "{brightness:?}"
    );
    let _ = e;
}

#[test]
#[ignore = "needs external FFmpeg with libx264"]
fn cancelling_a_real_export_kills_the_encoder_and_keeps_the_old_file() {
    let mut e = Editor::new();
    let dir = e.dir.path().to_owned();
    let long = colored(&dir, "long.mp4", &["red", "lime", "blue"], 10.0, 440, 30);
    let media = e.import(&long);
    let video = e.video;
    e.place(media, video, 0);
    let out = e.output("keep me.mp4");
    std::fs::write(&out, b"an earlier export").unwrap();
    e.session
        .start_export(out.clone(), Some(video), true, true)
        .unwrap();
    let running = pump_until(&mut e.session, 30, |v| {
        v["export"]["job"]["state"] == "encoding"
            && v["export"]["job"]["frames_encoded"].as_u64().unwrap_or(0) > 10
    });
    let job = running["export"]["job"]["id"].as_str().unwrap().to_owned();
    let asked = Instant::now();
    e.session.cancel_export(&job).unwrap();
    pump_until(&mut e.session, 20, |v| {
        v["export"]["job"]["state"] == "cancelled"
    });
    let spent = asked.elapsed();
    assert!(spent < Duration::from_secs(5), "cancel took {spent:?}");
    assert_eq!(std::fs::read(&out).unwrap(), b"an earlier export");
    assert_eq!(e.leftovers(), Vec::<String>::new());
    assert!(
        !ffmpeg_running(&dir),
        "no encoder or decoder may outlive the job"
    );
    // The same session can export again afterwards.
    let job = e.export(&out, true);
    assert_eq!(job["state"], "completed");
    assert_ne!(std::fs::read(&out).unwrap(), b"an earlier export");
}

/// Whether any FFmpeg is running that was given a path in this test's folder.
fn ffmpeg_running(dir: &Path) -> bool {
    let needle = dir.file_name().unwrap().to_string_lossy().into_owned();
    let listing = Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-Command",
            "Get-CimInstance Win32_Process -Filter \"Name='ffmpeg.exe'\" | ForEach-Object CommandLine",
        ])
        .output()
        .unwrap();
    String::from_utf8_lossy(&listing.stdout).contains(&needle)
}

/// The picture a plain decode of the source shows, which is what the editor shows.
fn plain_frame(path: &Path, seconds: f64) -> Vec<u8> {
    run_tool(
        "ffmpeg",
        &[
            "-hide_banner",
            "-v",
            "error",
            "-ss",
            &format!("{seconds:.3}"),
            "-i",
        ],
        Some(path),
        &["-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "rgb24", "-"],
    )
}

#[test]
#[ignore = "needs external FFmpeg with libx264"]
fn exported_colors_match_the_source_tagged_or_not() {
    for (name, tags) in [
        ("untagged.mp4", vec![]),
        (
            "bt709.mp4",
            vec![
                "-vf",
                "scale=out_color_matrix=bt709,setparams=colorspace=bt709:range=tv",
            ],
        ),
        (
            "bt601.mp4",
            vec![
                "-vf",
                "scale=out_color_matrix=bt601,setparams=colorspace=smpte170m:range=tv",
            ],
        ),
    ] {
        let mut e = Editor::new();
        let dir = e.dir.path().to_owned();
        let path = dir.join(name);
        let mut args = vec![
            "-hide_banner",
            "-v",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "color=c=lime:s=1280x720:r=30:d=1",
        ];
        args.extend_from_slice(&tags);
        args.extend_from_slice(&["-c:v", "libx264", "-pix_fmt", "yuv420p"]);
        run_tool("ffmpeg", &args, Some(&path), &[]);
        let source = mean_rgb(&plain_frame(&path, 0.5), CENTRE);
        let media = e.import(&path);
        let video = e.video;
        e.place(media, video, 0);
        let out = e.output("colors.mp4");
        e.export(&out, false);
        let exported = mean_rgb(&picture(&out, 0.5), CENTRE);
        for (a, b) in [
            (exported.0, source.0),
            (exported.1, source.1),
            (exported.2, source.2),
        ] {
            assert!(
                (a - b).abs() < 6.0,
                "{name}: exported {exported:?}, source {source:?}"
            );
        }
    }
}

#[test]
#[ignore = "needs external FFmpeg with libx264"]
fn encoders_are_found_in_the_real_ffmpeg() {
    FfmpegBackend::default()
        .check_export_encoders(&cutcap_media::Cancellation::default())
        .unwrap();
    let broken = FfmpegBackend::with_executables("no-such-ffmpeg.exe".into(), "x".into());
    assert!(
        broken
            .check_export_encoders(&cutcap_media::Cancellation::default())
            .is_err()
    );
}
