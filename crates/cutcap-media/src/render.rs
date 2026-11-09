//! Phase 5A offline rendering through external FFmpeg.
//!
//! Three separate jobs share this module (PHASE_5A_EXPORT_CONTRACT.md):
//! - a bounded stream of finished output-size YUV 4:2:0 frames from one source,
//!   each with its real PTS, framed by nonce-tagged packet records as in
//!   `stream.rs`;
//! - the one encoder process: raw frames on standard input, mixed PCM from a
//!   scratch file, supervised by a watcher that can kill it while a write blocks;
//! - capability and output inspection, which is independent of the encoder.
//!
//! Nothing here knows about projects, clips or time on the timeline.
use crate::{
    backend::{format_micros, input_limits, video_of},
    process::{OwnedChild, Priority, ProcessBudget, run_with},
    stream::{Packet, line, packet},
    *,
};
use cutcap_contracts::{ExportPreset, MediaSummary, Rounding, SourceTime, Timebase, VideoMetadata};
use serde_json::Value;
use std::{
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{ChildStdin, Command, ExitStatus, Stdio},
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, TrySendError},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

/// CREATE_NO_WINDOW and BELOW_NORMAL_PRIORITY_CLASS: export never competes
/// with the editor's interface or its preview.
#[cfg(windows)]
const CHILD_FLAGS: u32 = 0x0800_0000 | 0x0000_4000;
/// An encoder that accepts no frame for this long has stalled.
const ENCODER_STALL: Duration = Duration::from_secs(120);
/// Flushing, the trailer and `+faststart` must finish within this.
const FINALIZE_LIMIT: Duration = Duration::from_secs(300);
const DIAGNOSTICS_LIMIT: usize = 64 * 1024;

fn invalid(message: impl Into<String>) -> MediaError {
    MediaError::Invalid(message.into())
}
fn decode_io(source: std::io::Error) -> MediaError {
    MediaError::Io {
        stage: "Render decode",
        source,
    }
}
fn encode_io(source: std::io::Error) -> MediaError {
    MediaError::Io {
        stage: "Encoder",
        source,
    }
}

/// One clip's source, decoded from `start` on (inclusive), as output frames.
#[derive(Clone)]
pub struct RenderRequest {
    pub ticket: PreviewTicket,
    pub media: MediaSummary,
    pub start: SourceTime,
    pub preset: ExportPreset,
}

/// A finished YUV 4:2:0 frame of exactly `preset.frame_bytes()` bytes.
pub struct RenderFrame {
    pub ticket: PreviewTicket,
    pub actual: SourceTime,
    pub planes: Vec<u8>,
    pub process_id: u32,
}

/// What one encoder process writes.
#[derive(Debug, Clone)]
pub struct EncodeSpec {
    pub output: PathBuf,
    /// Raw `f32le` interleaved PCM holding the whole mix.
    pub audio_pcm: PathBuf,
    pub preset: ExportPreset,
}

/// A running encoder. Frames go in one at a time, blocking on its input.
pub trait FrameEncoder: Send {
    fn write_frame(&mut self, planes: &[u8]) -> Result<(), MediaError>;
    /// Frames through the encoder so far, by its own report.
    fn encoded(&self) -> u64;
    /// Closes the input and waits for the file to be finished.
    fn finish(self: Box<Self>) -> Result<(), MediaError>;
}

/// What FFprobe found in a finished file.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct OutputReport {
    pub format: String,
    pub duration_s: f64,
    pub video_codec: String,
    pub width: u32,
    pub height: u32,
    pub pixel_format: String,
    pub frames: u64,
    pub frame_rate: String,
    pub color_space: String,
    pub audio_codec: String,
    pub sample_rate: u32,
    pub channels: u32,
    pub audio_duration_s: f64,
}

impl OutputReport {
    /// Why this file is not the export that was asked for, if it is not.
    pub fn mismatch(&self, frames: u64, preset: ExportPreset) -> Option<String> {
        let expected_s = frames as f64 / f64::from(preset.fps);
        let frame_s = 1.0 / f64::from(preset.fps);
        if !self.format.split(',').any(|name| name == "mp4") {
            return Some(format!("container is {}", self.format));
        }
        if self.video_codec != "h264" {
            return Some(format!("video codec is {}", self.video_codec));
        }
        if (self.width, self.height) != (preset.width, preset.height) {
            return Some(format!("picture is {}x{}", self.width, self.height));
        }
        if self.pixel_format != "yuv420p" {
            return Some(format!("pixel format is {}", self.pixel_format));
        }
        if self.frame_rate != format!("{}/1", preset.fps) {
            return Some(format!("frame rate is {}", self.frame_rate));
        }
        if self.frames != frames {
            return Some(format!("it has {} of {frames} frames", self.frames));
        }
        if self.audio_codec != "aac" {
            return Some(format!("audio codec is {}", self.audio_codec));
        }
        if self.sample_rate != preset.sample_rate || self.channels != u32::from(preset.channels) {
            return Some(format!(
                "audio is {} Hz, {} channels",
                self.sample_rate, self.channels
            ));
        }
        // AAC adds up to two 1024-sample frames of padding at the ends.
        let padding = 2.0 * 1024.0 / f64::from(preset.sample_rate);
        if (self.duration_s - expected_s).abs() > frame_s + padding {
            return Some(format!(
                "duration is {:.3} s, expected {expected_s:.3} s",
                self.duration_s
            ));
        }
        if (self.audio_duration_s - expected_s).abs() > padding + 0.001 {
            return Some(format!(
                "audio lasts {:.3} s, expected {expected_s:.3} s",
                self.audio_duration_s
            ));
        }
        None
    }
}

/// The source's YUV matrix as the scale filter names it. An untagged source
/// goes through FFmpeg's own default, exactly as the preview path does, so the
/// file looks like what the editor showed.
fn source_matrix(video: &VideoMetadata) -> &'static str {
    match video.color_space.as_deref() {
        Some("bt709") => "bt709",
        Some("smpte170m" | "bt470bg" | "bt601") => "bt601",
        Some("smpte240m") => "smpte240m",
        Some("fcc") => "fcc",
        _ => "auto",
    }
}

fn picture_filter(
    lower: i64,
    video: &VideoMetadata,
    preset: ExportPreset,
) -> Result<String, MediaError> {
    let fit = video
        .fit_within(preset.width, preset.height)
        .map_err(|e| invalid(e.to_string()))?;
    Ok(format!(
        "select=gte(pts\\,{lower}),\
         scale={}:{}:flags=bicubic+accurate_rnd:in_color_matrix={}:out_color_matrix=bt709:out_range=tv,\
         format=yuv420p,setsar=1,pad={}:{}:{}:{}:black",
        fit.width,
        fit.height,
        source_matrix(video),
        preset.width,
        preset.height,
        fit.pad_x,
        fit.pad_y
    ))
}

pub(crate) fn run_frames(
    backend: &FfmpegBackend,
    request: &RenderRequest,
    cancel: &Cancellation,
    emit: &mut (dyn FnMut(RenderFrame) -> Result<(), MediaError> + Send),
) -> Result<(), MediaError> {
    if cancel.is_cancelled() {
        return Err(MediaError::Cancelled);
    }
    let meta = video_of(&request.media)?;
    meta.validate().map_err(|e| invalid(e.to_string()))?;
    if request.media.id != request.ticket.media_id
        || request.start.timebase != meta.timebase
        || request.start.ticks.unsigned_abs() > 1 << 52
    {
        return Err(invalid("invalid render request"));
    }
    let lower = request.start.ticks;
    let frame_bytes = request.preset.frame_bytes();
    let path = FfmpegBackend::input(&request.media.path)?;
    let filter = picture_filter(lower, meta, request.preset)?;
    let micros = Timebase::new(1, 1_000_000).map_err(|_| invalid("clock"))?;
    let rescale = |time: SourceTime| {
        time.rescale(micros, Rounding::Floor)
            .map(|t| t.ticks)
            .map_err(|_| invalid("source clock overflow"))
    };
    let target = rescale(request.start)?;
    let origin = rescale(meta.start())?;
    let tag = format!("CUTCAP_V{}", request.ticket.request_id);
    let mut command = Command::new(&backend.ffmpeg);
    command.args([
        "-hide_banner",
        "-nostdin",
        "-nostats",
        "-v",
        "error",
        "-xerror",
        "-max_alloc",
        "67108864",
        "-copyts",
        "-threads",
        "4",
        "-filter_threads",
        "2",
        "-max_pixels",
        "8847360",
    ]);
    input_limits(&mut command);
    command
        .args(["-noaccurate_seek", "-seek_timestamp", "1", "-ss"])
        .arg(format_micros(target.saturating_sub(1_000_000).max(origin)))
        .arg("-i")
        .arg(path);
    command.args([
        "-map",
        &format!("0:{}", meta.stream_index),
        "-an",
        "-sn",
        "-dn",
        "-vf",
        &filter,
        "-fps_mode",
        "passthrough",
        "-enc_time_base:v",
        &format!(
            "{}/{}",
            meta.timebase.numerator(),
            meta.timebase.denominator()
        ),
        "-c:v",
        "rawvideo",
        "-pix_fmt",
        "yuv420p",
        "-stats_mux_pre",
        "pipe:2",
        "-stats_mux_pre_fmt",
        &format!("{tag} {{n}} {{pts}} {{tb}} {{size}}"),
        "-f",
        "rawvideo",
        "pipe:1",
    ]);
    command
        .env("AV_LOG_FORCE_NOCOLOR", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(CHILD_FLAGS);
    }
    let mut child = OwnedChild(command.spawn().map_err(decode_io)?);
    let _budget = ProcessBudget::attach(&child.0)?;
    let pid = child.0.id();
    let stdout = child.0.stdout.take().ok_or(MediaError::Worker)?;
    let stderr = child.0.stderr.take().ok_or(MediaError::Worker)?;
    let abort = Arc::new(AtomicBool::new(false));
    let delivering = Arc::new(AtomicBool::new(false));
    let progress = Arc::new(Mutex::new(Instant::now()));
    let touch = || {
        *progress.lock().unwrap_or_else(PoisonError::into_inner) = Instant::now();
    };
    thread::scope(|scope| {
        let (tx, rx) = mpsc::sync_channel::<Packet>(8);
        let (abort_out, delivering_out) = (abort.clone(), delivering.clone());
        let out = thread::Builder::new()
            .name("cutcap-render-reader".into())
            .spawn_scoped(scope, move || {
                let result = (|| {
                    let mut reader = BufReader::with_capacity(1 << 20, stdout);
                    let mut index = 0_u64;
                    let mut previous = None;
                    loop {
                        let record = loop {
                            if cancel.is_cancelled() || abort_out.load(Ordering::Acquire) {
                                return Err(MediaError::Cancelled);
                            }
                            match rx.recv_timeout(Duration::from_millis(20)) {
                                Ok(p) => break Some(p),
                                Err(mpsc::RecvTimeoutError::Timeout) => {}
                                Err(mpsc::RecvTimeoutError::Disconnected) => break None,
                            }
                        };
                        let Some(record) = record else {
                            let mut extra = [0_u8; 1];
                            if reader.read(&mut extra).map_err(decode_io)? != 0 {
                                return Err(invalid("frame bytes without a timestamp record"));
                            }
                            break;
                        };
                        if record.index != index
                            || record.size != frame_bytes
                            || record.pts.timebase != meta.timebase
                            || record.pts.ticks < lower
                            || previous.is_some_and(|pts| record.pts.ticks <= pts)
                            || record.pts.ticks.unsigned_abs() > 1 << 52
                        {
                            return Err(invalid(
                                "render frame mismatch or non-monotonic timestamp",
                            ));
                        }
                        let mut planes = Vec::new();
                        planes
                            .try_reserve_exact(frame_bytes)
                            .map_err(|_| MediaError::Budget("render frame allocation"))?;
                        planes.resize(frame_bytes, 0);
                        reader.read_exact(&mut planes).map_err(decode_io)?;
                        touch();
                        previous = Some(record.pts.ticks);
                        index += 1;
                        delivering_out.store(true, Ordering::Release);
                        let delivered = emit(RenderFrame {
                            ticket: request.ticket,
                            actual: record.pts,
                            planes,
                            process_id: pid,
                        });
                        touch();
                        delivering_out.store(false, Ordering::Release);
                        delivered?;
                    }
                    if index == 0 {
                        return Err(MediaError::NoFrame);
                    }
                    Ok(())
                })();
                if result.is_err() {
                    abort_out.store(true, Ordering::Release);
                }
                result
            })
            .map_err(decode_io)?;
        let abort_err = abort.clone();
        let tag = tag.as_str();
        let err = thread::Builder::new()
            .name("cutcap-render-records".into())
            .spawn_scoped(scope, move || {
                let result = (|| {
                    let mut reader = BufReader::new(stderr);
                    let mut diagnostics = Vec::new();
                    while let Some(bytes) = line(&mut reader, 1024)? {
                        if let Some(record) = packet(&bytes, tag)? {
                            let mut pending = record;
                            loop {
                                if cancel.is_cancelled() || abort_err.load(Ordering::Acquire) {
                                    return Err(MediaError::Cancelled);
                                }
                                match tx.try_send(pending) {
                                    Ok(()) => break,
                                    Err(TrySendError::Full(p)) => {
                                        pending = p;
                                        cancel.wait(Duration::from_millis(5));
                                    }
                                    Err(_) => return Err(invalid("extra frame record")),
                                }
                            }
                        } else if diagnostics.len() + bytes.len() <= DIAGNOSTICS_LIMIT {
                            diagnostics.extend_from_slice(&bytes);
                        }
                    }
                    Ok(diagnostics)
                })();
                if result.is_err() {
                    abort_err.store(true, Ordering::Release);
                }
                result
            })
            .map_err(decode_io)?;
        let mut stalled = false;
        let status = loop {
            if cancel.is_cancelled() || abort.load(Ordering::Acquire) {
                break None;
            }
            if !delivering.load(Ordering::Acquire)
                && progress
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .elapsed()
                    > backend.decode_timeout
            {
                stalled = true;
                break None;
            }
            match child.0.try_wait() {
                Ok(Some(status)) => break Some(status),
                Ok(None) => cancel.wait(Duration::from_millis(10)),
                Err(error) => {
                    abort.store(true, Ordering::Release);
                    drop(child);
                    let _ = out.join();
                    let _ = err.join();
                    return Err(decode_io(error));
                }
            }
        };
        let external_cancel = cancel.is_cancelled();
        if stalled || status.is_some_and(|s| !s.success()) {
            abort.store(true, Ordering::Release);
        }
        drop(child);
        let out_result = out.join().map_err(|_| MediaError::Worker)?;
        let err_result = err.join().map_err(|_| MediaError::Worker)?;
        if external_cancel {
            return Err(MediaError::Cancelled);
        }
        if stalled {
            return Err(MediaError::Timeout("Render decode"));
        }
        if let Some(status) = status.filter(|s| !s.success()) {
            return Err(MediaError::Process {
                stage: "Render decode",
                code: status.code(),
                detail: err_result
                    .ok()
                    .map(|bytes| String::from_utf8_lossy(&bytes).chars().take(2000).collect())
                    .unwrap_or_else(|| "decoder process failed".into()),
            });
        }
        match (out_result, err_result) {
            (Err(error), _) | (_, Err(error)) => Err(error),
            (Ok(()), Ok(_)) => status
                .map(|_| ())
                .ok_or(MediaError::Timeout("Render decode")),
        }
    })
}

struct EncoderShared {
    encoded: AtomicU64,
    stop: AtomicBool,
    stalled: AtomicBool,
    finishing: AtomicBool,
    last_accepted: Mutex<Instant>,
    exit: Mutex<Option<ExitStatus>>,
}

impl EncoderShared {
    fn accepted(&self) {
        *self
            .last_accepted
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Instant::now();
    }
}

struct FfmpegEncoder {
    child: Arc<Mutex<OwnedChild>>,
    stdin: Option<ChildStdin>,
    shared: Arc<EncoderShared>,
    watcher: Option<JoinHandle<()>>,
    reader: Option<JoinHandle<Vec<u8>>>,
    cancel: Cancellation,
    frame_bytes: usize,
    _budget: ProcessBudget,
}

/// Reads encoder status lines: `frame=` is progress, anything that is not a
/// `-progress` key is a diagnostic. Long lines are truncated, never fatal.
fn read_encoder_status(stderr: impl Read, shared: &EncoderShared) -> Vec<u8> {
    const KEYS: [&str; 12] = [
        "fps=",
        "stream_",
        "bitrate=",
        "total_size=",
        "out_time",
        "dup_frames=",
        "drop_frames=",
        "speed=",
        "progress=",
        "frame=",
        "avg_",
        "drop=",
    ];
    let mut reader = BufReader::new(stderr);
    let mut diagnostics = Vec::new();
    let mut bytes = Vec::with_capacity(256);
    loop {
        bytes.clear();
        match (&mut reader).take(4096).read_until(b'\n', &mut bytes) {
            Ok(0) | Err(_) => return diagnostics,
            Ok(_) => {}
        }
        let text = String::from_utf8_lossy(&bytes);
        let text = text.trim();
        if let Some(count) = text
            .strip_prefix("frame=")
            .and_then(|v| v.trim().parse::<u64>().ok())
        {
            shared.encoded.store(count, Ordering::Release);
        } else if !KEYS.iter().any(|key| text.starts_with(key))
            && diagnostics.len() + text.len() < DIAGNOSTICS_LIMIT
        {
            diagnostics.extend_from_slice(text.as_bytes());
            diagnostics.push(b'\n');
        }
    }
}

pub(crate) fn open_encoder(
    backend: &FfmpegBackend,
    spec: &EncodeSpec,
    cancel: &Cancellation,
) -> Result<Box<dyn FrameEncoder>, MediaError> {
    let preset = spec.preset;
    let mut command = Command::new(&backend.ffmpeg);
    command
        .args(["-hide_banner", "-nostdin", "-nostats", "-v", "error"])
        .args(["-progress", "pipe:2", "-stats_period", "0.25"])
        .args(["-protocol_whitelist", "file,pipe"])
        .args(["-f", "rawvideo", "-pix_fmt", "yuv420p"])
        .args([
            "-video_size",
            &format!("{}x{}", preset.width, preset.height),
        ])
        .args(["-framerate", &preset.fps.to_string()])
        .args(["-thread_queue_size", "4", "-i", "pipe:0"])
        .args(["-protocol_whitelist", "file,pipe"])
        .args(["-f", "f32le", "-ar", &preset.sample_rate.to_string()])
        .args(["-ch_layout", "stereo", "-i"])
        .arg(&spec.audio_pcm)
        .args(["-map", "0:v:0", "-map", "1:a:0"])
        .args(["-c:v", "libx264", "-preset", preset.speed])
        .args(["-crf", &preset.crf.to_string()])
        .args(["-pix_fmt", "yuv420p", "-fps_mode", "passthrough"])
        // The encoder takes its color tags from the frames, so set them there.
        .args([
            "-vf",
            "setparams=range=tv:colorspace=bt709:color_primaries=bt709:color_trc=bt709",
        ])
        .args(["-c:a", "aac", "-b:a"])
        .arg(format!("{}k", preset.audio_bitrate_kbps))
        .args(["-movflags", "+faststart", "-f", "mp4", "-y"])
        .arg(&spec.output)
        .env("AV_LOG_FORCE_NOCOLOR", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(CHILD_FLAGS);
    }
    let mut child = OwnedChild(command.spawn().map_err(encode_io)?);
    let budget = ProcessBudget::attach(&child.0)?;
    let stdin = child.0.stdin.take().ok_or(MediaError::Worker)?;
    let stderr = child.0.stderr.take().ok_or(MediaError::Worker)?;
    let shared = Arc::new(EncoderShared {
        encoded: AtomicU64::new(0),
        stop: AtomicBool::new(false),
        stalled: AtomicBool::new(false),
        finishing: AtomicBool::new(false),
        last_accepted: Mutex::new(Instant::now()),
        exit: Mutex::new(None),
    });
    let reader = {
        let shared = shared.clone();
        thread::Builder::new()
            .name("cutcap-encoder-status".into())
            .spawn(move || read_encoder_status(stderr, &shared))
            .map_err(encode_io)?
    };
    let child = Arc::new(Mutex::new(child));
    // The writer may be blocked in a pipe write, so another thread must be
    // able to end the process: that is what unblocks it.
    let watcher = {
        let (child, shared, cancel) = (child.clone(), shared.clone(), cancel.clone());
        thread::Builder::new()
            .name("cutcap-encoder-watch".into())
            .spawn(move || {
                let kill = || {
                    let mut child = child.lock().unwrap_or_else(PoisonError::into_inner);
                    let _ = child.0.kill();
                    *shared.exit.lock().unwrap_or_else(PoisonError::into_inner) =
                        child.0.wait().ok();
                };
                loop {
                    if shared.stop.load(Ordering::Acquire) {
                        return;
                    }
                    if cancel.is_cancelled() {
                        kill();
                        return;
                    }
                    let limit = if shared.finishing.load(Ordering::Acquire) {
                        FINALIZE_LIMIT
                    } else {
                        ENCODER_STALL
                    };
                    let idle = shared
                        .last_accepted
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .elapsed();
                    if idle > limit {
                        shared.stalled.store(true, Ordering::Release);
                        kill();
                        return;
                    }
                    let exited = child
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .0
                        .try_wait();
                    if let Ok(Some(status)) = exited {
                        *shared.exit.lock().unwrap_or_else(PoisonError::into_inner) = Some(status);
                        return;
                    }
                    cancel.wait(Duration::from_millis(20));
                }
            })
            .map_err(encode_io)?
    };
    Ok(Box::new(FfmpegEncoder {
        child,
        stdin: Some(stdin),
        shared,
        watcher: Some(watcher),
        reader: Some(reader),
        cancel: cancel.clone(),
        frame_bytes: preset.frame_bytes(),
        _budget: budget,
    }))
}

impl FfmpegEncoder {
    /// Waits for the process to end and turns how it ended into a result.
    fn conclude(&mut self) -> Result<(), MediaError> {
        if let Some(watcher) = self.watcher.take() {
            watcher.join().map_err(|_| MediaError::Worker)?;
        }
        let diagnostics = self
            .reader
            .take()
            .map(JoinHandle::join)
            .transpose()
            .map_err(|_| MediaError::Worker)?
            .unwrap_or_default();
        if self.cancel.is_cancelled() {
            return Err(MediaError::Cancelled);
        }
        if self.shared.stalled.load(Ordering::Acquire) {
            return Err(MediaError::Timeout("The encoder"));
        }
        let exit = *self
            .shared
            .exit
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        match exit {
            Some(status) if status.success() => Ok(()),
            status => Err(MediaError::Process {
                stage: "The encoder",
                code: status.and_then(|s| s.code()),
                detail: String::from_utf8_lossy(&diagnostics)
                    .chars()
                    .take(2000)
                    .collect(),
            }),
        }
    }
}

impl FrameEncoder for FfmpegEncoder {
    fn write_frame(&mut self, planes: &[u8]) -> Result<(), MediaError> {
        if planes.len() != self.frame_bytes {
            return Err(invalid("frame size does not match the output format"));
        }
        let stdin = self.stdin.as_mut().ok_or(MediaError::Worker)?;
        self.shared.accepted();
        if stdin.write_all(planes).is_ok() {
            self.shared.accepted();
            return Ok(());
        }
        // A broken pipe says only that the process is gone: find out why.
        self.stdin = None;
        self.conclude()?;
        Err(MediaError::Process {
            stage: "The encoder",
            code: None,
            detail: "The encoder stopped accepting frames.".into(),
        })
    }

    fn encoded(&self) -> u64 {
        self.shared.encoded.load(Ordering::Acquire)
    }

    fn finish(mut self: Box<Self>) -> Result<(), MediaError> {
        self.shared.accepted();
        self.shared.finishing.store(true, Ordering::Release);
        // Closing the input is what tells the encoder to flush.
        self.stdin = None;
        self.conclude()
    }
}

impl Drop for FfmpegEncoder {
    fn drop(&mut self) {
        self.stdin = None;
        self.shared.stop.store(true, Ordering::Release);
        if let Some(watcher) = self.watcher.take() {
            let _ = watcher.join();
        }
        // Owned here on every path, so the process cannot outlive the encoder.
        let _ = self
            .child
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .0
            .kill();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

/// Whether the external FFmpeg can encode this preset. `Err` names what is
/// missing. Listed encoders are parsed, not inferred from the build.
pub(crate) fn check_encoders(
    backend: &FfmpegBackend,
    cancel: &Cancellation,
) -> Result<(), MediaError> {
    let mut command = Command::new(&backend.ffmpeg);
    command.args(["-hide_banner", "-encoders"]);
    let output = run_with(
        command,
        "List encoders",
        cancel,
        Duration::from_secs(15),
        512 * 1024,
        64 * 1024,
        Priority::BelowNormal,
    )?;
    let listing = String::from_utf8_lossy(&output.stdout);
    let has = |kind: char, name: &str| {
        listing.lines().any(|row| {
            let mut fields = row.split_whitespace();
            fields.next().is_some_and(|flags| flags.starts_with(kind))
                && fields.next() == Some(name)
        })
    };
    if !has('V', "libx264") {
        return Err(MediaError::Unsupported(
            "this FFmpeg build has no libx264 H.264 encoder".into(),
        ));
    }
    if !has('A', "aac") {
        return Err(MediaError::Unsupported(
            "this FFmpeg build has no AAC encoder".into(),
        ));
    }
    Ok(())
}

pub(crate) fn inspect_output(
    backend: &FfmpegBackend,
    path: &Path,
    cancel: &Cancellation,
) -> Result<OutputReport, MediaError> {
    let mut command = Command::new(backend.ffprobe());
    command
        .args(["-v", "error", "-show_entries"])
        .arg(
            "stream=codec_type,codec_name,width,height,pix_fmt,nb_frames,avg_frame_rate,\
             sample_rate,channels,duration,color_space:format=format_name,duration",
        )
        .args(["-of", "json"])
        .arg(path);
    let output = run_with(
        command,
        "Inspect output",
        cancel,
        Duration::from_secs(30),
        256 * 1024,
        64 * 1024,
        Priority::BelowNormal,
    )?;
    parse_output_report(&output.stdout)
}

fn parse_output_report(bytes: &[u8]) -> Result<OutputReport, MediaError> {
    let value: Value =
        serde_json::from_slice(bytes).map_err(|_| invalid("unreadable output report"))?;
    let text = |v: &Value, key: &str| v.get(key).and_then(Value::as_str).unwrap_or("").to_owned();
    let number = |v: &Value, key: &str| text(v, key).parse::<f64>().unwrap_or(0.0);
    let streams = value
        .get("streams")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid("output report has no streams"))?;
    let of = |kind: &str| {
        streams
            .iter()
            .find(|s| s.get("codec_type").and_then(Value::as_str) == Some(kind))
    };
    let video = of("video").ok_or_else(|| invalid("output has no video stream"))?;
    let audio = of("audio").ok_or_else(|| invalid("output has no audio stream"))?;
    let unsigned = |v: &Value, key: &str| v.get(key).and_then(Value::as_u64).unwrap_or(0) as u32;
    let format = value.get("format").cloned().unwrap_or(Value::Null);
    Ok(OutputReport {
        format: text(&format, "format_name"),
        duration_s: number(&format, "duration"),
        video_codec: text(video, "codec_name"),
        width: unsigned(video, "width"),
        height: unsigned(video, "height"),
        pixel_format: text(video, "pix_fmt"),
        frames: text(video, "nb_frames").parse().unwrap_or(0),
        frame_rate: text(video, "avg_frame_rate"),
        color_space: text(video, "color_space"),
        audio_codec: text(audio, "codec_name"),
        sample_rate: text(audio, "sample_rate").parse().unwrap_or(0),
        channels: unsigned(audio, "channels"),
        audio_duration_s: number(audio, "duration"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report() -> OutputReport {
        OutputReport {
            format: "mov,mp4,m4a,3gp,3g2,mj2".into(),
            duration_s: 2.0,
            video_codec: "h264".into(),
            width: 1280,
            height: 720,
            pixel_format: "yuv420p".into(),
            frames: 60,
            frame_rate: "30/1".into(),
            color_space: "bt709".into(),
            audio_codec: "aac".into(),
            sample_rate: 48_000,
            channels: 2,
            audio_duration_s: 2.02,
        }
    }

    #[test]
    fn a_correct_file_passes_and_each_defect_is_named() {
        let preset = ExportPreset::STANDARD;
        assert_eq!(report().mismatch(60, preset), None);
        type Defect = fn(&mut OutputReport);
        let cases: [(Defect, &str); 8] = [
            (|r| r.video_codec = "hevc".into(), "video codec"),
            (|r| r.width = 640, "picture"),
            (|r| r.pixel_format = "yuv444p".into(), "pixel format"),
            (|r| r.frame_rate = "25/1".into(), "frame rate"),
            (|r| r.frames = 59, "frames"),
            (|r| r.audio_codec = "mp3".into(), "audio codec"),
            (|r| r.sample_rate = 44_100, "audio is"),
            (|r| r.duration_s = 3.0, "duration"),
        ];
        for (defect, expected) in cases {
            let mut broken = report();
            defect(&mut broken);
            let found = broken.mismatch(60, preset).unwrap();
            assert!(found.contains(expected), "{found}");
        }
    }

    #[test]
    fn output_reports_are_parsed_from_ffprobe_json() {
        let found = parse_output_report(
            br#"{"streams":[
              {"codec_type":"video","codec_name":"h264","width":1280,"height":720,"pix_fmt":"yuv420p",
               "nb_frames":"60","avg_frame_rate":"30/1","duration":"2.000000","color_space":"bt709"},
              {"codec_type":"audio","codec_name":"aac","sample_rate":"48000","channels":2,"duration":"2.021333"}],
              "format":{"format_name":"mov,mp4,m4a,3gp,3g2,mj2","duration":"2.021333"}}"#,
        )
        .unwrap();
        assert_eq!((found.width, found.frames, found.channels), (1280, 60, 2));
        assert_eq!(found.video_codec, "h264");
        assert!(parse_output_report(br#"{"streams":[]}"#).is_err());
        assert!(parse_output_report(b"not json").is_err());
    }

    #[test]
    fn tags_are_honoured_and_untagged_sources_match_the_preview_path() {
        let mut video: VideoMetadata =
            crate::parse_probe(&serde_json::to_vec(&crate::probe::tests::fixture()).unwrap())
                .unwrap();
        video.color_space = Some("bt709".into());
        assert_eq!(source_matrix(&video), "bt709");
        video.color_space = Some("smpte170m".into());
        assert_eq!(source_matrix(&video), "bt601");
        // The preview's scale filter has no matrix option: FFmpeg's default.
        for (width, height) in [(1920, 1080), (640, 480)] {
            (video.width, video.height) = (width, height);
            video.color_space = None;
            assert_eq!(source_matrix(&video), "auto");
            video.color_space = Some("unknown".into());
            assert_eq!(source_matrix(&video), "auto");
        }
    }
}
