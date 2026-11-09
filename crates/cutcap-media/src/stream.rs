//! A framed PAM image stream paired with indexed, nonce-tagged mux records.
use crate::{
    backend::{format_micros, input_limits},
    process::OwnedChild,
    *,
};
use cutcap_contracts::{MAX_MEDIA_SECONDS, Rounding, SourceTime, Timebase};
use std::{
    io::{BufRead, BufReader, Read},
    process::{Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, TrySendError},
    },
    thread,
    time::{Duration, Instant},
};

fn invalid(message: &str) -> MediaError {
    MediaError::Invalid(message.into())
}
fn io(source: std::io::Error) -> MediaError {
    MediaError::Io {
        stage: "Continuous decode",
        source,
    }
}
pub(crate) fn line(reader: &mut impl BufRead, limit: usize) -> Result<Option<Vec<u8>>, MediaError> {
    let mut bytes = Vec::new();
    let count = reader
        .take((limit + 1) as u64)
        .read_until(b'\n', &mut bytes)
        .map_err(io)?;
    if count == 0 {
        return Ok(None);
    }
    if count > limit || bytes.last() != Some(&b'\n') {
        return Err(invalid("truncated or oversized framed-stream header"));
    }
    Ok(Some(bytes))
}
#[derive(Debug)]
pub(crate) struct Packet {
    pub(crate) index: u64,
    pub(crate) pts: SourceTime,
    pub(crate) size: usize,
}
pub(crate) fn packet(bytes: &[u8], tag: &str) -> Result<Option<Packet>, MediaError> {
    let text = std::str::from_utf8(bytes).map_err(|_| invalid("invalid packet record UTF-8"))?;
    let Some(rest) = text.strip_prefix(tag).and_then(|s| s.strip_prefix(' ')) else {
        return Ok(None);
    };
    let fields: Vec<_> = rest.split_whitespace().collect();
    let [index, pts, clock, size] = fields.as_slice() else {
        return Err(invalid("malformed packet timestamp record"));
    };
    let (n, d) = clock
        .split_once('/')
        .ok_or_else(|| invalid("missing packet timebase"))?;
    let timebase = Timebase::new(
        n.parse().map_err(|_| invalid("packet clock"))?,
        d.parse().map_err(|_| invalid("packet clock"))?,
    )
    .map_err(|_| invalid("packet clock"))?;
    Ok(Some(Packet {
        index: index.parse().map_err(|_| invalid("packet index"))?,
        pts: SourceTime {
            ticks: pts
                .parse()
                .map_err(|_| invalid("packet has no integer PTS"))?,
            timebase,
        },
        size: size.parse().map_err(|_| invalid("packet size"))?,
    }))
}
/// Read one complete image; never scan binary pixels for delimiter-like bytes.
fn image(
    reader: &mut impl BufRead,
    expected: (u32, u32),
) -> Result<Option<(RgbaFrame, usize)>, MediaError> {
    let Some(first) = line(reader, 128)? else {
        return Ok(None);
    };
    if first != b"P7\n" {
        return Err(invalid("expected PAM P7 packet"));
    }
    let mut header = first.len();
    let mut fields = std::collections::BTreeMap::new();
    loop {
        let bytes = line(reader, 128)?.ok_or_else(|| invalid("truncated PAM header"))?;
        header += bytes.len();
        if header > 512 {
            return Err(MediaError::Budget("PAM header exceeds 512 bytes"));
        }
        if bytes == b"ENDHDR\n" {
            break;
        }
        let text = std::str::from_utf8(&bytes)
            .map_err(|_| invalid("invalid PAM header"))?
            .trim_end();
        let (k, v) = text.split_once(' ').ok_or_else(|| invalid("PAM field"))?;
        if fields.insert(k.to_owned(), v.to_owned()).is_some() {
            return Err(invalid("duplicate PAM field"));
        }
    }
    let number = |key| {
        fields
            .get(key)
            .and_then(|s| s.parse::<u32>().ok())
            .ok_or_else(|| invalid("PAM dimensions"))
    };
    let (width, height) = (number("WIDTH")?, number("HEIGHT")?);
    if (width, height) != expected
        || number("DEPTH")? != 4
        || number("MAXVAL")? != 255
        || fields.get("TUPLTYPE").map(String::as_str) != Some("RGB_ALPHA")
        || fields.len() != 5
    {
        return Err(invalid("unsupported or mismatched PAM format"));
    }
    let size = RgbaFrame::expected_bytes(width, height)?;
    let mut pixels = Vec::new();
    pixels
        .try_reserve_exact(size)
        .map_err(|_| MediaError::Budget("frame allocation"))?;
    pixels.resize(size, 0);
    reader.read_exact(&mut pixels).map_err(io)?;
    Ok(Some((
        RgbaFrame::new(width, height, width as usize * 4, pixels)?,
        header + size,
    )))
}

pub(crate) fn run(
    backend: &FfmpegBackend,
    request: &DecodeRequest,
    cancel: &Cancellation,
    emit: &mut (dyn FnMut(DecodedFrame) -> Result<(), MediaError> + Send),
) -> Result<(), MediaError> {
    run_policy(backend, request, None, cancel, emit)
}

pub(crate) fn run_policy(
    backend: &FfmpegBackend,
    request: &DecodeRequest,
    minimum: Option<SourceTime>,
    cancel: &Cancellation,
    emit: &mut (dyn FnMut(DecodedFrame) -> Result<(), MediaError> + Send),
) -> Result<(), MediaError> {
    if cancel.is_cancelled() {
        return Err(MediaError::Cancelled);
    }
    let meta = crate::backend::video_of(&request.media)?;
    meta.validate().map_err(|e| invalid(&e.to_string()))?;
    let lower = minimum.unwrap_or(request.position);
    if lower.timebase != meta.timebase
        || lower.ticks > request.position.ticks
        || lower.ticks < meta.start_pts
        || lower.ticks.unsigned_abs() > 1_u64 << 52
    {
        return Err(invalid("invalid interval source bound"));
    }
    let delta = request
        .position
        .ticks
        .checked_sub(meta.start_pts)
        .ok_or_else(|| invalid("source timestamp overflow"))?;
    if request.media.id != request.ticket.media_id
        || request.position.timebase != meta.timebase
        || delta < 0
        || request.position.ticks.unsigned_abs() > 1_u64 << 52
        || i128::from(delta) * i128::from(meta.timebase.numerator())
            > i128::from(MAX_MEDIA_SECONDS) * i128::from(meta.timebase.denominator())
    {
        return Err(invalid("invalid continuous source request"));
    }
    let path = FfmpegBackend::input(&request.media.path)?;
    let dims = meta
        .preview_dimensions()
        .map_err(|e| invalid(&e.to_string()))?;
    let micros = Timebase::new(1, 1_000_000).map_err(|_| invalid("clock"))?;
    let target = request
        .position
        .rescale(micros, Rounding::Floor)
        .map_err(|_| invalid("source clock overflow"))?
        .ticks;
    let origin = meta
        .start()
        .rescale(micros, Rounding::Floor)
        .map_err(|_| invalid("source clock overflow"))?
        .ticks;
    let tag = format!("CUTCAP_{}", request.ticket.request_id);
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
        "2",
        "-filter_threads",
        "1",
        "-max_pixels",
        "8847360",
    ]);
    input_limits(&mut command);
    command
        .args([
            "-noaccurate_seek",
            "-seek_timestamp",
            "1",
            "-ss",
            &format_micros(target.saturating_sub(1_000_000).max(origin)),
            "-i",
        ])
        .arg(path);
    command.args([
        "-map",
        &format!("0:{}", meta.stream_index),
        "-an",
        "-sn",
        "-dn",
        "-vf",
        &format!(
            "select=gte(pts\\,{}),scale={}:{}:flags=bilinear,setsar=1,format=rgb24,format=rgba",
            lower.ticks, dims.0, dims.1
        ),
        "-fps_mode",
        "passthrough",
        "-enc_time_base:v",
        &format!(
            "{}/{}",
            meta.timebase.numerator(),
            meta.timebase.denominator()
        ),
        "-c:v",
        "pam",
        "-threads:v",
        "1",
        "-stats_mux_pre",
        "pipe:2",
        "-stats_mux_pre_fmt",
        &format!("{tag} {{n}} {{pts}} {{tb}} {{size}}"),
        "-thread_queue_size",
        "1",
        "-f",
        "image2pipe",
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
        command.creation_flags(0x0800_0000);
    }
    let started = Instant::now();
    let mut child = OwnedChild(command.spawn().map_err(io)?);
    let _budget = crate::process::ProcessBudget::attach(&child.0)?;
    let pid = child.0.id();
    let stdout = child.0.stdout.take().ok_or(MediaError::Worker)?;
    let stderr = child.0.stderr.take().ok_or(MediaError::Worker)?;
    let abort = Arc::new(AtomicBool::new(false));
    let delivering = Arc::new(AtomicBool::new(false));
    let progress = Arc::new(Mutex::new(Instant::now()));
    let touch = || {
        if let Ok(mut p) = progress.lock() {
            *p = Instant::now();
        }
    };
    thread::scope(|scope| {
        let (tx, rx) = mpsc::sync_channel::<Packet>(8);
        let abort_out = abort.clone();
        let delivering_out = delivering.clone();
        let out = thread::Builder::new()
            .name("cutcap-pam-reader".into())
            .spawn_scoped(scope, move || {
                let result = (|| {
                    let mut reader = BufReader::new(stdout);
                    let mut index = 0_u64;
                    let mut previous = None;
                    let mut last = Instant::now();
                    let mut preroll = None;
                    let mut entered = minimum.is_none();
                    while let Some((image, bytes)) = image(&mut reader, dims)? {
                        touch();
                        let record = loop {
                            if cancel.is_cancelled() || abort_out.load(Ordering::Acquire) {
                                return Err(MediaError::Cancelled);
                            }
                            match rx.recv_timeout(Duration::from_millis(20)) {
                                Ok(p) => break p,
                                Err(mpsc::RecvTimeoutError::Timeout) => {}
                                Err(_) => return Err(invalid("image has no timestamp packet")),
                            }
                        };
                        if record.index != index
                            || record.size != bytes
                            || record.pts.timebase != meta.timebase
                            || record.pts.ticks < lower.ticks
                            || previous.is_some_and(|pts| record.pts.ticks <= pts)
                            || record.pts.ticks.unsigned_abs() > 1_u64 << 52
                        {
                            return Err(invalid(
                                "PAM/timestamp packet mismatch or non-monotonic PTS",
                            ));
                        }
                        previous = Some(record.pts.ticks);
                        index = index
                            .checked_add(1)
                            .ok_or_else(|| invalid("packet count overflow"))?;
                        let frame = DecodedFrame {
                            ticket: request.ticket,
                            requested: request.position,
                            actual: record.pts,
                            image,
                            decode_us: last.elapsed().as_micros().min(u128::from(u64::MAX)) as u64,
                            process_id: pid,
                        };
                        // Coalesce preroll with one owned frame, retaining real PTS.
                        if !entered && frame.actual.ticks <= request.position.ticks {
                            preroll = Some(frame);
                            last = Instant::now();
                            continue;
                        }
                        delivering_out.store(true, Ordering::Release);
                        let result = (|| {
                            if !entered {
                                entered = true;
                                if let Some(previous) = preroll.take() {
                                    emit(previous)?;
                                }
                            }
                            emit(frame)
                        })();
                        touch();
                        delivering_out.store(false, Ordering::Release);
                        result?;
                        last = Instant::now();
                    }
                    if let Some(frame) = preroll.take() {
                        delivering_out.store(true, Ordering::Release);
                        let result = emit(frame);
                        touch();
                        delivering_out.store(false, Ordering::Release);
                        result?;
                    }
                    loop {
                        match rx.recv_timeout(Duration::from_millis(20)) {
                            Ok(_) => return Err(invalid("timestamp packet has no PAM image")),
                            Err(mpsc::RecvTimeoutError::Disconnected) => break,
                            Err(_) => {
                                if cancel.is_cancelled() || abort_out.load(Ordering::Acquire) {
                                    return Err(MediaError::Cancelled);
                                }
                            }
                        }
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
            .map_err(io)?;
        let abort_err = abort.clone();
        let err = thread::Builder::new()
            .name("cutcap-packet-reader".into())
            .spawn_scoped(scope, move || {
                let result = (|| {
                    let mut reader = BufReader::new(stderr);
                    let mut diagnostics = Vec::new();
                    let mut count = 0_u64;
                    while let Some(bytes) = line(&mut reader, 1024)? {
                        if let Some(record) = packet(&bytes, &tag)? {
                            if record.index != count {
                                return Err(invalid("packet record sequence"));
                            }
                            count += 1;
                            let mut pending = record;
                            loop {
                                if cancel.is_cancelled() || abort_err.load(Ordering::Acquire) {
                                    return Err(MediaError::Cancelled);
                                }
                                match tx.try_send(pending) {
                                    Ok(()) => break,
                                    Err(TrySendError::Full(p)) => {
                                        pending = p;
                                        cancel.wait(Duration::from_millis(20));
                                    }
                                    Err(_) => return Err(invalid("extra packet record")),
                                }
                            }
                        } else {
                            if diagnostics.len() + bytes.len() > 65536 {
                                return Err(MediaError::Budget("continuous decoder error log"));
                            }
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
            if cancel.is_cancelled() || abort.load(Ordering::Acquire) {
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
                Ok(None) => cancel.wait(Duration::from_millis(20)),
                Err(error) => {
                    cancel.cancel();
                    drop(child);
                    let _ = out.join();
                    let _ = err.join();
                    return Err(io(error));
                }
            }
        };
        let external_cancel = cancel.is_cancelled();
        let internal_failure =
            stalled || abort.load(Ordering::Acquire) || status.is_some_and(|s| !s.success());
        if internal_failure {
            cancel.cancel();
        }
        drop(child);
        let out_result = out.join().map_err(|_| MediaError::Worker)?;
        let err_result = err.join().map_err(|_| MediaError::Worker)?;
        if external_cancel {
            return Err(MediaError::Cancelled);
        }
        if stalled {
            return Err(MediaError::Timeout("Continuous decode stalled"));
        }
        if status.is_some_and(|s| !s.success()) {
            return Err(MediaError::Process {
                stage: "Continuous decode",
                code: status.and_then(|s| s.code()),
                detail: err_result
                    .ok()
                    .map(|bytes| String::from_utf8_lossy(&bytes).chars().take(2000).collect())
                    .unwrap_or_else(|| "decoder process failed".into()),
            });
        }
        // Preserve the parser's typed error instead of a secondary broken-pipe error.
        let diagnostics = match (out_result, err_result) {
            (Err(error), _) if !matches!(error, MediaError::Cancelled) => return Err(error),
            (_, Err(error)) => return Err(error),
            (Err(error), _) => return Err(error),
            (Ok(()), Ok(bytes)) => bytes,
        };
        let Some(status) = status else {
            return Err(MediaError::Timeout("Continuous decode stalled"));
        };
        if !status.success() {
            return Err(MediaError::Process {
                stage: "Continuous decode",
                code: status.code(),
                detail: String::from_utf8_lossy(&diagnostics)
                    .chars()
                    .take(2000)
                    .collect(),
            });
        }
        let _elapsed = started.elapsed();
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn framing_rejects_bad_headers_and_timestamp_records() {
        assert!(image(&mut &b"P7\nWIDTH 999999\nENDHDR\n"[..], (1, 1)).is_err());
        assert!(packet(b"CUTCAP_x 0 NOPTS 1/1000 4\n", "CUTCAP_x").is_err());
        assert!(
            packet(b"title: CUTCAP_x 0 99 1/1 4\n", "CUTCAP_x")
                .unwrap()
                .is_none()
        );
        let mut bytes =
            b"P7\nWIDTH 1\nHEIGHT 1\nDEPTH 4\nMAXVAL 255\nTUPLTYPE RGB_ALPHA\nENDHDR\n".to_vec();
        bytes.extend([255, 0, 0, 255]);
        assert_eq!(
            image(&mut bytes.as_slice(), (1, 1)).unwrap().unwrap().1,
            bytes.len()
        );
    }
}
