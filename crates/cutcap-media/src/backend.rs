use crate::{
    Cancellation, DecodedFrame, MediaError, PreviewTicket, RgbaFrame, parse_probe, process,
};
use cutcap_contracts::{MediaSummary, Rounding, SourceTime, Timebase};
use std::{
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

#[derive(Clone)]
pub struct FfmpegBackend {
    pub(crate) ffmpeg: PathBuf,
    ffprobe: PathBuf,
    pub probe_timeout: Duration,
    pub decode_timeout: Duration,
}

#[derive(Clone)]
pub struct DecodeRequest {
    pub ticket: PreviewTicket,
    pub media: MediaSummary,
    pub position: SourceTime,
}

pub trait MediaBackend: Send + Sync + 'static {
    fn probe(&self, path: &Path, cancellation: &Cancellation) -> Result<ProbeResult, MediaError>;
    fn decode(
        &self,
        request: &DecodeRequest,
        cancellation: &Cancellation,
    ) -> Result<DecodedFrame, MediaError>;
    /// Emit complete timestamped frames until EOF/cancellation. Called only on
    /// the persistent worker; the callback applies bounded backpressure.
    fn stream(
        &self,
        _request: &DecodeRequest,
        _cancellation: &Cancellation,
        _emit: &mut (dyn FnMut(DecodedFrame) -> Result<(), MediaError> + Send),
    ) -> Result<(), MediaError> {
        Err(MediaError::Unsupported(
            "continuous decoding is unavailable for this backend".into(),
        ))
    }
    /// Continuous interval mode: retain the last eligible real PTS <= target,
    /// then successors. `minimum` is the clip's inclusive source bound.
    fn stream_interval(
        &self,
        _request: &DecodeRequest,
        _minimum: SourceTime,
        _cancellation: &Cancellation,
        _emit: &mut (dyn FnMut(DecodedFrame) -> Result<(), MediaError> + Send),
    ) -> Result<(), MediaError> {
        Err(MediaError::Unsupported(
            "interval decoding is unavailable for this backend".into(),
        ))
    }
}

pub struct ProbeResult {
    pub path: PathBuf,
    pub metadata: cutcap_contracts::VideoMetadata,
    pub probe_us: u64,
}

impl Default for FfmpegBackend {
    fn default() -> Self {
        Self {
            ffmpeg: std::env::var_os("CUTCAP_FFMPEG")
                .map(PathBuf::from)
                .unwrap_or_else(|| "ffmpeg.exe".into()),
            ffprobe: std::env::var_os("CUTCAP_FFPROBE")
                .map(PathBuf::from)
                .unwrap_or_else(|| "ffprobe.exe".into()),
            probe_timeout: Duration::from_secs(15),
            decode_timeout: Duration::from_secs(20),
        }
    }
}

impl FfmpegBackend {
    pub fn with_executables(ffmpeg: PathBuf, ffprobe: PathBuf) -> Self {
        Self {
            ffmpeg,
            ffprobe,
            ..Self::default()
        }
    }
    pub(crate) fn input(path: &Path) -> Result<PathBuf, MediaError> {
        let raw = path.as_os_str().to_string_lossy();
        if raw.len() > 32_768 || raw.starts_with("\\\\") && !raw.starts_with("\\\\?\\") {
            return Err(MediaError::Unsupported(
                "use a local regular file, not a network/device path".into(),
            ));
        }
        let path = std::fs::canonicalize(path).map_err(|source| MediaError::Io {
            stage: "Open local media",
            source,
        })?;
        if path
            .as_os_str()
            .to_string_lossy()
            .starts_with("\\\\?\\UNC\\")
        {
            return Err(MediaError::Unsupported(
                "network media paths are unavailable".into(),
            ));
        }
        let metadata = std::fs::metadata(&path).map_err(|source| MediaError::Io {
            stage: "Read media file",
            source,
        })?;
        if !metadata.is_file() || metadata.len() == 0 || metadata.len() > 32 * 1024 * 1024 * 1024 {
            return Err(MediaError::Budget(
                "media must be a nonempty regular file of at most 32 GiB",
            ));
        }
        Ok(path)
    }
}

pub(crate) fn input_limits(command: &mut Command) {
    command.args([
        "-protocol_whitelist",
        "file,pipe",
        "-probesize",
        "8388608",
        "-analyzeduration",
        "5000000",
        "-max_streams",
        "64",
    ]);
}

impl MediaBackend for FfmpegBackend {
    fn stream_interval(
        &self,
        request: &DecodeRequest,
        minimum: SourceTime,
        cancel: &Cancellation,
        emit: &mut (dyn FnMut(DecodedFrame) -> Result<(), MediaError> + Send),
    ) -> Result<(), MediaError> {
        crate::stream::run_policy(self, request, Some(minimum), cancel, emit)
    }
    fn stream(
        &self,
        request: &DecodeRequest,
        cancel: &Cancellation,
        emit: &mut (dyn FnMut(DecodedFrame) -> Result<(), MediaError> + Send),
    ) -> Result<(), MediaError> {
        crate::stream::run(self, request, cancel, emit)
    }
    fn probe(&self, path: &Path, cancellation: &Cancellation) -> Result<ProbeResult, MediaError> {
        if cancellation.is_cancelled() {
            return Err(MediaError::Cancelled);
        }
        let path = Self::input(path)?;
        let mut command = Command::new(&self.ffprobe);
        command.args(["-v", "error", "-max_alloc", "67108864"]);
        input_limits(&mut command);
        command.args(["-show_entries", "stream=index,id,codec_type,codec_name,width,height,pix_fmt,sample_aspect_ratio,display_aspect_ratio,r_frame_rate,avg_frame_rate,time_base,start_pts,start_time,duration_ts,duration,color_space,color_transfer,color_range:stream_disposition=attached_pic:stream_tags=rotate:stream_side_data=rotation,displaymatrix:format=format_name,duration,start_time", "-of", "json", "-i"]).arg(&path);
        let output = process::run(
            command,
            "Probe media",
            cancellation,
            self.probe_timeout,
            crate::probe::MAX_PROBE_BYTES,
            64 * 1024,
        )?;
        let metadata = parse_probe(&output.stdout)?;
        if !metadata
            .container
            .split(',')
            .any(|name| matches!(name, "mov" | "mp4" | "matroska" | "webm" | "avi" | "mpegts"))
        {
            return Err(MediaError::Unsupported(format!(
                "container {} is outside the Phase 1 video set",
                metadata.container
            )));
        }
        Ok(ProbeResult {
            path,
            metadata,
            probe_us: output.elapsed_us,
        })
    }

    fn decode(
        &self,
        request: &DecodeRequest,
        cancellation: &Cancellation,
    ) -> Result<DecodedFrame, MediaError> {
        request
            .media
            .metadata
            .validate()
            .map_err(|e| MediaError::Invalid(e.to_string()))?;
        if request.ticket.media_id != request.media.id {
            return Err(MediaError::Invalid("decode media identity mismatch".into()));
        }
        let metadata = &request.media.metadata;
        if request.position.timebase != metadata.timebase {
            return Err(MediaError::Invalid(
                "request must use the selected stream's timebase".into(),
            ));
        }
        let delta = request
            .position
            .ticks
            .checked_sub(metadata.start_pts)
            .ok_or_else(|| MediaError::Invalid("timestamp overflow".into()))?;
        if delta < 0
            || request.position.ticks.unsigned_abs() > (1_u64 << 52)
            || i128::from(delta) * i128::from(metadata.timebase.numerator())
                > i128::from(cutcap_contracts::MAX_MEDIA_SECONDS)
                    * i128::from(metadata.timebase.denominator())
        {
            return Err(MediaError::Invalid(
                "source position is outside the supported range".into(),
            ));
        }
        let path = Self::input(&request.media.path)?;
        let (width, height) = metadata
            .preview_dimensions()
            .map_err(|e| MediaError::Invalid(e.to_string()))?;
        let expected = RgbaFrame::expected_bytes(width, height)?;
        let clock = Timebase::new(1, 1_000_000).map_err(|_| MediaError::Invalid("clock".into()))?;
        let target_us = request
            .position
            .rescale(clock, Rounding::Floor)
            .map_err(|e| MediaError::Invalid(e.to_string()))?
            .ticks;
        let start_us = metadata
            .start()
            .rescale(clock, Rounding::Floor)
            .map_err(|e| MediaError::Invalid(e.to_string()))?
            .ticks;
        let preroll_us = target_us.saturating_sub(1_000_000).max(start_us);
        let mut command = Command::new(&self.ffmpeg);
        command.args([
            "-hide_banner",
            "-nostdin",
            "-nostats",
            "-v",
            "info",
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
        // Timestamp seeking is absolute; copyts preserves original source PTS.
        command
            .args([
                "-noaccurate_seek",
                "-seek_timestamp",
                "1",
                "-ss",
                &format_micros(preroll_us),
                "-i",
            ])
            .arg(path);
        // The unpredictable request identity also prevents untrusted input tags
        // printed by FFmpeg from impersonating this filter's timestamp records.
        let timestamp_filter = format!("showinfo@cutcap_{}", request.ticket.request_id);
        let filter = format!(
            "select=gte(pts\\,{}),scale={width}:{height}:flags=bilinear,setsar=1,format=rgb24,format=rgba,{timestamp_filter}",
            request.position.ticks
        );
        command.args([
            "-map",
            &format!("0:{}", metadata.stream_index),
            "-an",
            "-sn",
            "-dn",
            "-vf",
            &filter,
            "-frames:v",
            "1",
            "-fps_mode",
            "passthrough",
            "-threads:v",
            "1",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "rgba",
            "pipe:1",
        ]);
        let output = process::run(
            command,
            "Decode frame",
            cancellation,
            self.decode_timeout,
            expected,
            1024 * 1024,
        )?;
        if output.stdout.is_empty() {
            return Err(MediaError::NoFrame);
        }
        let actual = parse_frame_pts(&output.stderr, &timestamp_filter)?;
        let threshold = request
            .position
            .rescale(actual.timebase, Rounding::Ceil)
            .map_err(|e| MediaError::Invalid(e.to_string()))?;
        if actual.ticks < threshold.ticks {
            return Err(MediaError::Invalid(
                "decoder returned a frame before the requested PTS".into(),
            ));
        }
        Ok(DecodedFrame {
            ticket: request.ticket,
            requested: request.position,
            actual,
            image: RgbaFrame::new(width, height, width as usize * 4, output.stdout)?,
            decode_us: output.elapsed_us,
            process_id: output.pid,
        })
    }
}

pub(crate) fn format_micros(value: i64) -> String {
    let abs = value.unsigned_abs();
    format!(
        "{}{whole}.{fraction:06}",
        if value < 0 { "-" } else { "" },
        whole = abs / 1_000_000,
        fraction = abs % 1_000_000
    )
}

fn parse_frame_pts(stderr: &[u8], filter_name: &str) -> Result<SourceTime, MediaError> {
    let mut timebase = None;
    let prefix = format!("[{filter_name} @ ");
    for line in String::from_utf8_lossy(stderr)
        .lines()
        .filter(|line| line.starts_with(&prefix))
    {
        if let Some((_, rest)) = line.split_once("config in time_base:")
            && let Some(raw) = rest.split(',').next()
            && let Some((n, d)) = raw.trim().split_once('/')
        {
            timebase = n
                .parse::<u32>()
                .ok()
                .zip(d.parse::<u32>().ok())
                .and_then(|(n, d)| Timebase::new(n, d).ok());
        }
        if line.contains(" n:")
            && let Some((_, rest)) = line.split_once(" pts:")
        {
            let ticks = rest
                .split_whitespace()
                .next()
                .and_then(|v| v.parse().ok())
                .ok_or_else(|| MediaError::Invalid("decoded frame has no reliable PTS".into()))?;
            return Ok(SourceTime {
                ticks,
                timebase: timebase.ok_or_else(|| {
                    MediaError::Invalid("decoded frame timebase is missing".into())
                })?,
            });
        }
    }
    Err(MediaError::Invalid(
        "FFmpeg did not report the decoded frame PTS".into(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn timestamp_records_require_the_exact_request_filter_prefix() {
        let filter = "showinfo@cutcap_unique_request";
        let forged=b"    title: [Parsed_showinfo_0 @ 1] config in time_base: 1/1, frame_rate: 1/1\n    : [Parsed_showinfo_0 @ 1] n: 0 pts: 999999\n";
        assert!(parse_frame_pts(forged, filter).is_err());
        let mut bytes = forged.to_vec();
        bytes.extend_from_slice(b"[showinfo@cutcap_unique_request @ aabb] config in time_base: 1/90000, frame_rate: 30/1\n[showinfo@cutcap_other_request @ aabb] n: 0 pts: 999999\n[showinfo@cutcap_unique_request @ aabb] n: 0 pts: -90 pts_time: -0.001\n");
        assert_eq!(
            parse_frame_pts(&bytes, filter).unwrap(),
            SourceTime {
                ticks: -90,
                timebase: Timebase::new(1, 90000).unwrap()
            }
        );
        assert!(
            parse_frame_pts(
                b"[showinfo@cutcap_unique_request @ aabb] n: 0 pts: NOPTS\n",
                filter
            )
            .is_err()
        );
    }
}
