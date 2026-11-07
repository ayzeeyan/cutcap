//! Phase 4B audio: real decoded PCM with FFmpeg's own sample timestamps.
//!
//! One FFmpeg process decodes one audio stream from a requested source sample
//! onward. Output is raw interleaved `f32` (`pcm_f32le`). Every packet is
//! announced on stderr by a nonce-tagged `-stats_mux_pre` record carrying its
//! PTS and byte size; with `-copyts` the PTS is the source clock expressed in
//! samples of the output rate (`1/rate`). Packets are framed by those sizes,
//! never by scanning PCM bytes. Samples before the requested start are
//! discarded by PTS; overlaps are trimmed; a forward jump is reported as a
//! gap (the consumer outputs silence there) — no sample is ever invented.
//!
//! Two uses share this decoder:
//! - playback: resampled to the output device rate, downmixed/upmixed to
//!   stereo, normal priority, back-pressured by the consumer;
//! - waveform analysis: native rate and channels (peaks must equal the
//!   stream's own decoded PCM), below-normal priority, one thread, bounded by
//!   an end sample.
use crate::{
    backend::{format_micros, input_limits},
    process::{OwnedChild, ProcessBudget},
    stream::{line, packet},
    *,
};
use cutcap_contracts::{AudioStreamMetadata, MAX_MEDIA_SECONDS, MediaSummary};
use std::{
    io::{BufReader, Read},
    process::{Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, TrySendError},
    },
    thread,
    time::{Duration, Instant},
};

/// Playback output: interleaved stereo `f32`.
pub const OUTPUT_CHANNELS: u16 = 2;
/// Output rates the playback decoder accepts.
pub const MIN_OUTPUT_RATE: u32 = 8_000;
pub const MAX_OUTPUT_RATE: u32 = 192_000;
/// The largest single PCM packet accepted (FFmpeg emits a few thousand samples).
const MAX_PACKET_BYTES: usize = 1 << 20;

/// What to decode: one audio stream of a verified source, from `start_sample`
/// on the source clock at `rate` (playback) or at the stream's native rate
/// (`rate: None`, analysis), until `end_sample` (exclusive) when given.
#[derive(Debug, Clone)]
pub struct PcmRequest {
    pub media: MediaSummary,
    pub stream_index: u32,
    pub start_sample: i64,
    pub end_sample: Option<i64>,
    /// `Some(rate)`: resample to `rate`, stereo. `None`: native rate/channels.
    pub rate: Option<u32>,
    /// Background analysis runs below normal priority with one thread.
    pub background: bool,
}

/// A run of decoded samples: `first_sample` is the source-clock position of
/// the first frame (at the request's rate); `samples` are interleaved.
#[derive(Debug, Clone, PartialEq)]
pub struct PcmChunk {
    pub first_sample: i64,
    pub channels: u16,
    pub samples: Vec<f32>,
    pub process_id: u32,
}

impl PcmChunk {
    pub fn frames(&self) -> usize {
        self.samples.len() / usize::from(self.channels.max(1))
    }
    pub fn end_sample(&self) -> i64 {
        self.first_sample + self.frames() as i64
    }
}

fn invalid(message: &str) -> MediaError {
    MediaError::Invalid(message.into())
}
fn io(source: std::io::Error) -> MediaError {
    MediaError::Io {
        stage: "Audio decode",
        source,
    }
}

/// The request's stream, rate and channel count, after validation.
fn resolve(request: &PcmRequest) -> Result<(&AudioStreamMetadata, u32, u16), MediaError> {
    let meta = &request.media.metadata;
    meta.validate().map_err(|e| invalid(&e.to_string()))?;
    let audio = meta
        .audio_stream(request.stream_index)
        .ok_or_else(|| MediaError::Unsupported("this audio stream is not decodable here".into()))?;
    let (rate, channels) = match request.rate {
        Some(rate) => {
            if !(MIN_OUTPUT_RATE..=MAX_OUTPUT_RATE).contains(&rate) {
                return Err(invalid("unsupported output sample rate"));
            }
            (rate, OUTPUT_CHANNELS)
        }
        None => (audio.sample_rate, audio.channels),
    };
    let limit = i128::from(MAX_MEDIA_SECONDS) * i128::from(rate);
    let bound = |s: i64| i128::from(s).abs() <= limit && s.unsigned_abs() <= 1 << 52;
    if !bound(request.start_sample)
        || request
            .end_sample
            .is_some_and(|e| !bound(e) || e <= request.start_sample)
    {
        return Err(invalid("invalid audio sample range"));
    }
    Ok((audio, rate, channels))
}

/// Decodes `request`, calling `emit` with contiguous runs in source order.
/// `emit` may block (back-pressure). Returns when the stream ends, the end
/// sample is reached, or on cancellation/failure.
pub(crate) fn run(
    backend: &FfmpegBackend,
    request: &PcmRequest,
    cancel: &Cancellation,
    emit: &mut (dyn FnMut(PcmChunk) -> Result<(), MediaError> + Send),
) -> Result<(), MediaError> {
    if cancel.is_cancelled() {
        return Err(MediaError::Cancelled);
    }
    let (audio, rate, channels) = resolve(request)?;
    let path = FfmpegBackend::input(&request.media.path)?;
    // Seek half a second early on the source clock, then discard by PTS.
    let micros = (i128::from(request.start_sample) * 1_000_000 / i128::from(rate)) as i64;
    let origin = audio
        .start()
        .rescale(
            cutcap_contracts::Timebase::new(1, 1_000_000).map_err(|_| invalid("clock"))?,
            cutcap_contracts::Rounding::Floor,
        )
        .map_err(|_| invalid("audio clock overflow"))?
        .ticks;
    let seek = micros.saturating_sub(500_000).max(origin.min(micros));
    let tag = format!(
        "CUTCAP_A{}",
        cutcap_contracts::RequestId::new()
            .to_string()
            .replace('-', "")
    );
    let mut command = Command::new(&backend.ffmpeg);
    command.args([
        "-hide_banner",
        "-nostdin",
        "-nostats",
        "-v",
        "error",
        "-max_alloc",
        "67108864",
        "-copyts",
        "-filter_threads",
        "1",
    ]);
    input_limits(&mut command);
    command
        .args(["-threads", if request.background { "1" } else { "2" }])
        .args(["-noaccurate_seek", "-seek_timestamp", "1", "-ss"])
        .arg(format_micros(seek))
        .arg("-i")
        .arg(path);
    let filter = match request.rate {
        Some(rate) => {
            format!("aresample={rate}:resampler=swr,aformat=sample_fmts=flt:channel_layouts=stereo")
        }
        None => "aformat=sample_fmts=flt".to_owned(),
    };
    command.args([
        "-map",
        &format!("0:{}", audio.stream_index),
        "-vn",
        "-sn",
        "-dn",
        "-af",
        &filter,
        "-c:a",
        "pcm_f32le",
        "-stats_mux_pre",
        "pipe:2",
        "-stats_mux_pre_fmt",
        &format!("{tag} {{n}} {{pts}} {{tb}} {{size}}"),
        "-f",
        "f32le",
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
        let below = if request.background { 0x0000_4000 } else { 0 };
        command.creation_flags(0x0800_0000 | below);
    }
    let mut child = OwnedChild(command.spawn().map_err(io)?);
    let _budget = ProcessBudget::attach(&child.0)?;
    let pid = child.0.id();
    let stdout = child.0.stdout.take().ok_or(MediaError::Worker)?;
    let stderr = child.0.stderr.take().ok_or(MediaError::Worker)?;
    let abort = Arc::new(AtomicBool::new(false));
    let finished = Arc::new(AtomicBool::new(false));
    let delivering = Arc::new(AtomicBool::new(false));
    let progress = Arc::new(Mutex::new(Instant::now()));
    let touch = || {
        if let Ok(mut p) = progress.lock() {
            *p = Instant::now();
        }
    };
    let frame_bytes = usize::from(channels) * 4;
    thread::scope(|scope| {
        let (tx, rx) = mpsc::sync_channel::<crate::stream::Packet>(16);
        let (abort_out, finished_out, delivering_out) =
            (abort.clone(), finished.clone(), delivering.clone());
        let out = thread::Builder::new()
            .name("cutcap-pcm-reader".into())
            .spawn_scoped(scope, move || {
                let result = (|| {
                    let mut reader = BufReader::new(stdout);
                    let mut index = 0_u64;
                    let mut expected: Option<i64> = None;
                    let mut delivered = false;
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
                            // No more records: stdout must be at EOF too.
                            let mut extra = [0u8; 1];
                            if reader.read(&mut extra).map_err(io)? != 0 {
                                return Err(invalid("PCM bytes without a timestamp record"));
                            }
                            break;
                        };
                        if record.index != index
                            || record.size == 0
                            || record.size > MAX_PACKET_BYTES
                            || record.size % frame_bytes != 0
                            || record.pts.timebase.numerator() != 1
                            || record.pts.timebase.denominator() != rate
                            || record.pts.ticks.unsigned_abs() > 1 << 52
                        {
                            return Err(invalid("PCM packet record mismatch"));
                        }
                        index += 1;
                        let mut bytes = vec![0u8; record.size];
                        reader.read_exact(&mut bytes).map_err(io)?;
                        touch();
                        let frames = (record.size / frame_bytes) as i64;
                        let mut first = record.pts.ticks;
                        let mut samples: Vec<f32> = bytes
                            .as_chunks::<4>()
                            .0
                            .iter()
                            .map(|b| f32::from_le_bytes(*b))
                            .collect();
                        // Overlap with what was already delivered: trim, never reorder.
                        if let Some(next) = expected {
                            if first + frames <= next {
                                continue;
                            }
                            if first < next {
                                let skip = (next - first) as usize * usize::from(channels);
                                samples.drain(..skip);
                                first = next;
                            }
                        }
                        // Before the requested start: discard by PTS.
                        let end = first + (samples.len() / usize::from(channels)) as i64;
                        if end <= request.start_sample {
                            expected = Some(end);
                            continue;
                        }
                        if first < request.start_sample {
                            let skip =
                                (request.start_sample - first) as usize * usize::from(channels);
                            samples.drain(..skip);
                            first = request.start_sample;
                        }
                        // Past the requested end: deliver up to it, then stop.
                        let mut done = false;
                        if let Some(stop) = request.end_sample {
                            if first >= stop {
                                finished_out.store(true, Ordering::Release);
                                break;
                            }
                            let keep = ((stop - first) as usize)
                                .min(samples.len() / usize::from(channels));
                            if keep * usize::from(channels) < samples.len() {
                                samples.truncate(keep * usize::from(channels));
                                done = true;
                            }
                        }
                        expected = Some(first + (samples.len() / usize::from(channels)) as i64);
                        delivering_out.store(true, Ordering::Release);
                        let result = emit(PcmChunk {
                            first_sample: first,
                            channels,
                            samples,
                            process_id: pid,
                        });
                        touch();
                        delivering_out.store(false, Ordering::Release);
                        result?;
                        delivered = true;
                        if done
                            || request
                                .end_sample
                                .is_some_and(|stop| expected >= Some(stop))
                        {
                            finished_out.store(true, Ordering::Release);
                            break;
                        }
                    }
                    if !delivered && !finished_out.load(Ordering::Acquire) {
                        return Err(MediaError::NoFrame);
                    }
                    Ok(())
                })();
                if result.is_err() {
                    abort_out.store(true, Ordering::Release);
                } else {
                    finished_out.store(true, Ordering::Release);
                }
                result
            })
            .map_err(io)?;
        let abort_err = abort.clone();
        let finished_err = finished.clone();
        let err = thread::Builder::new()
            .name("cutcap-pcm-records".into())
            .spawn_scoped(scope, move || {
                let result = (|| -> Result<Vec<u8>, MediaError> {
                    let mut reader = BufReader::new(stderr);
                    let mut diagnostics = Vec::new();
                    while let Some(bytes) = line(&mut reader, 1024)? {
                        if let Some(record) = packet(&bytes, &tag)? {
                            let mut pending = record;
                            loop {
                                if cancel.is_cancelled()
                                    || abort_err.load(Ordering::Acquire)
                                    || finished_err.load(Ordering::Acquire)
                                {
                                    return Ok(diagnostics);
                                }
                                match tx.try_send(pending) {
                                    Ok(()) => break,
                                    Err(TrySendError::Full(p)) => {
                                        pending = p;
                                        cancel.wait(Duration::from_millis(5));
                                    }
                                    Err(TrySendError::Disconnected(_)) => return Ok(diagnostics),
                                }
                            }
                        } else if diagnostics.len() + bytes.len() <= 65536 {
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
            .map_err(io)?;
        let mut stalled = false;
        let status = loop {
            if cancel.is_cancelled()
                || abort.load(Ordering::Acquire)
                || finished.load(Ordering::Acquire)
            {
                break None;
            }
            if !delivering.load(Ordering::Acquire)
                && progress
                    .lock()
                    .map_or(true, |p| p.elapsed() > backend.decode_timeout)
            {
                stalled = true;
                break None;
            }
            match child.0.try_wait() {
                Ok(Some(s)) => break Some(s),
                Ok(None) => cancel.wait(Duration::from_millis(10)),
                Err(error) => {
                    abort.store(true, Ordering::Release);
                    drop(child);
                    let _ = out.join();
                    let _ = err.join();
                    return Err(io(error));
                }
            }
        };
        let reached_end = finished.load(Ordering::Acquire) && status.is_none();
        if status.is_none() {
            // Cancelled, stalled, failed or the end sample was reached: stop the child.
            abort.store(true, Ordering::Release);
        }
        drop(child);
        let out_result = out.join().map_err(|_| MediaError::Worker)?;
        let err_result = err.join().map_err(|_| MediaError::Worker)?;
        if cancel.is_cancelled() {
            return Err(MediaError::Cancelled);
        }
        if stalled {
            return Err(MediaError::Timeout("Audio decode stalled"));
        }
        if let Some(status) = status
            && !status.success()
        {
            return Err(MediaError::Process {
                stage: "Audio decode",
                code: status.code(),
                detail: err_result
                    .ok()
                    .map(|b| String::from_utf8_lossy(&b).chars().take(2000).collect())
                    .unwrap_or_default(),
            });
        }
        match out_result {
            Err(MediaError::Cancelled) if reached_end => Ok(()),
            other => other,
        }
    })
}

/// Waveform resolution: one min/max pair per 256 native-rate frames on an
/// absolute sample grid (bucket `k` covers `[k*256, (k+1)*256)`), so every
/// zoom level and every clip of a source reuses the same buckets.
pub const PEAK_BUCKET_FRAMES: i64 = 256;
/// Buckets per analysis chunk (1024 buckets: about 5.5 s at 48 kHz).
pub const PEAK_CHUNK_BUCKETS: i64 = 1024;
pub const PEAK_CHUNK_FRAMES: i64 = PEAK_BUCKET_FRAMES * PEAK_CHUNK_BUCKETS;
/// At most this many consecutive chunks are analysed by one process.
pub const MAX_PEAK_CHUNKS: usize = 4;

/// Measured amplitude summary of one chunk: for each bucket the minimum and
/// maximum sample over all channels. A bucket with no decoded samples (before
/// the stream starts, past its end, or a real gap) has `min > max`.
#[derive(Debug, Clone, PartialEq)]
pub struct Peaks {
    pub chunk: i64,
    pub rate: u32,
    pub channels: u16,
    pub min: Vec<f32>,
    pub max: Vec<f32>,
    pub process_id: u32,
}

impl Peaks {
    fn empty(chunk: i64, rate: u32, channels: u16) -> Self {
        Self {
            chunk,
            rate,
            channels,
            min: vec![1.0; PEAK_CHUNK_BUCKETS as usize],
            max: vec![-1.0; PEAK_CHUNK_BUCKETS as usize],
            process_id: 0,
        }
    }
    /// Folds decoded native-rate samples into the buckets they belong to.
    pub fn accumulate(&mut self, chunk: &PcmChunk) {
        let base = self.chunk * PEAK_CHUNK_FRAMES;
        let channels = usize::from(chunk.channels.max(1));
        for (i, frame) in chunk.samples.chunks_exact(channels).enumerate() {
            let offset = chunk.first_sample + i as i64 - base;
            if !(0..PEAK_CHUNK_FRAMES).contains(&offset) {
                continue;
            }
            let bucket = (offset / PEAK_BUCKET_FRAMES) as usize;
            for &value in frame {
                if value.is_finite() {
                    let (lo, hi) = (&mut self.min[bucket], &mut self.max[bucket]);
                    if *lo > *hi {
                        (*lo, *hi) = (value, value);
                    } else {
                        *lo = lo.min(value);
                        *hi = hi.max(value);
                    }
                }
            }
        }
    }
}

/// Analyses `count` consecutive chunks of one audio stream at native rate.
pub(crate) fn peaks(
    backend: &FfmpegBackend,
    media: &MediaSummary,
    stream_index: u32,
    first_chunk: i64,
    count: usize,
    cancel: &Cancellation,
) -> Result<Vec<Peaks>, MediaError> {
    if count == 0 || count > MAX_PEAK_CHUNKS || first_chunk < 0 {
        return Err(invalid("invalid waveform chunk request"));
    }
    let audio = media
        .metadata
        .audio_stream(stream_index)
        .ok_or_else(|| MediaError::Unsupported("this audio stream is not decodable here".into()))?;
    let (rate, channels) = (audio.sample_rate, audio.channels);
    let mut out: Vec<Peaks> = (0..count as i64)
        .map(|k| Peaks::empty(first_chunk + k, rate, channels))
        .collect();
    let request = PcmRequest {
        media: media.clone(),
        stream_index,
        start_sample: first_chunk * PEAK_CHUNK_FRAMES,
        end_sample: Some((first_chunk + count as i64) * PEAK_CHUNK_FRAMES),
        rate: None,
        background: true,
    };
    let mut pid = 0;
    let result = run(backend, &request, cancel, &mut |chunk| {
        pid = chunk.process_id;
        for peaks in &mut out {
            peaks.accumulate(&chunk);
        }
        Ok(())
    });
    match result {
        // Past the end of the stream: the remaining buckets stay empty.
        Ok(()) | Err(MediaError::NoFrame) => {
            for peaks in &mut out {
                peaks.process_id = pid;
            }
            Ok(out)
        }
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn peaks_fold_every_channel_into_absolute_buckets_and_mark_empty_ones() {
        let mut p = Peaks::empty(1, 48_000, 2);
        // Frames 0..512 of chunk 1 (absolute sample PEAK_CHUNK_FRAMES..).
        let base = PEAK_CHUNK_FRAMES;
        let mut samples = Vec::new();
        for i in 0..512 {
            let left = if i == 300 { 0.9 } else { 0.1 };
            let right = if i == 10 { -0.75 } else { -0.05 };
            samples.extend([left, right]);
        }
        p.accumulate(&PcmChunk {
            first_sample: base,
            channels: 2,
            samples,
            process_id: 1,
        });
        assert_eq!(
            (p.min[0], p.max[0]),
            (-0.75, 0.1),
            "impulse in the right channel"
        );
        assert_eq!(
            (p.min[1], p.max[1]),
            (-0.05, 0.9),
            "impulse in the left channel"
        );
        assert!(p.min[2] > p.max[2], "no samples: empty bucket");
        // Samples outside the chunk are ignored.
        p.accumulate(&PcmChunk {
            first_sample: base - 10,
            channels: 1,
            samples: vec![5.0; 10],
            process_id: 1,
        });
        assert!(p.min[1023] > p.max[1023]);
        assert_eq!(p.max[0], 0.1);
    }
}
