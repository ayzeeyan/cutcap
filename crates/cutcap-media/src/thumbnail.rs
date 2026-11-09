//! Timeline filmstrip thumbnails: small, low-priority, batched decodes.
//!
//! Policy: a thumbnail for source time `t` is the **first actual decoded frame
//! with PTS >= t** (the same ceiling policy as the Phase 1/3C still preview,
//! so a cell shows what the timeline still preview shows at its left edge).
//! Callers validate that the actual PTS lies inside the clip's source
//! interval before drawing it; nothing is inferred from a nominal frame rate.
//!
//! One FFmpeg process decodes up to [`MAX_BATCH`] samples of one source: each
//! sample is its own keyframe-seeked input (`-ss` before `-i`, `-copyts`),
//! selected by real PTS, scaled, and concatenated into one raw RGBA pipe.
//! Per-branch `showinfo@cutcap_<id>_<n>` records give each frame's PTS, and a
//! branch that reaches EOF before its sample simply produces no frame. The
//! process runs at below-normal priority with one decode thread so it never
//! competes with playback decoders.
use crate::{Cancellation, FfmpegBackend, MediaError, RgbaFrame, process};
use cutcap_contracts::{MediaSummary, RequestId, Rounding, SourceTime, Timebase, VideoMetadata};
use std::process::Command;

/// Samples per FFmpeg process. Small enough that a cancelled batch wastes little.
pub const MAX_BATCH: usize = 6;
pub const MAX_THUMBNAIL_WIDTH: u32 = 384;
pub const MAX_THUMBNAIL_HEIGHT: u32 = 216;
pub const MIN_THUMBNAIL_HEIGHT: u32 = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThumbnailRequest {
    /// Opaque caller identity, echoed in the result.
    pub token: u64,
    /// Requested source time in the selected stream's own timebase.
    pub position: SourceTime,
}

#[derive(Debug)]
pub struct Thumbnail {
    pub token: u64,
    pub requested: SourceTime,
    /// The decoded frame's original PTS (>= requested).
    pub actual: SourceTime,
    pub image: RgbaFrame,
    /// The FFmpeg process that decoded this batch (diagnostics).
    pub process_id: u32,
}

/// Thumbnail pixel size for a source at `height`: the display aspect of the
/// (rotated) source, as for the preview, never stretched.
pub fn thumbnail_size(metadata: &VideoMetadata, height: u32) -> Result<(u32, u32), MediaError> {
    let (w, h) = metadata
        .preview_dimensions()
        .map_err(|e| MediaError::Invalid(e.to_string()))?;
    let height = height.clamp(MIN_THUMBNAIL_HEIGHT, MAX_THUMBNAIL_HEIGHT);
    let width = (u64::from(height) * u64::from(w) + u64::from(h) / 2) / u64::from(h);
    let width = u32::try_from(width)
        .unwrap_or(MAX_THUMBNAIL_WIDTH)
        .clamp(1, MAX_THUMBNAIL_WIDTH);
    Ok((width, height))
}

/// Validates a sample position the same way as an isolated decode.
pub fn validate_position(metadata: &VideoMetadata, position: SourceTime) -> Result<(), MediaError> {
    if position.timebase != metadata.timebase {
        return Err(MediaError::Invalid(
            "thumbnail positions must use the selected stream's timebase".into(),
        ));
    }
    let delta = position
        .ticks
        .checked_sub(metadata.start_pts)
        .ok_or_else(|| MediaError::Invalid("timestamp overflow".into()))?;
    if delta < 0
        || position.ticks.unsigned_abs() > (1_u64 << 52)
        || i128::from(delta) * i128::from(metadata.timebase.numerator())
            > i128::from(cutcap_contracts::MAX_MEDIA_SECONDS)
                * i128::from(metadata.timebase.denominator())
    {
        return Err(MediaError::Invalid(
            "thumbnail position is outside the supported source range".into(),
        ));
    }
    Ok(())
}

/// The PTS showinfo reported for one branch, or `None` when it produced no frame.
fn branch_pts(stderr: &str, filter: &str) -> Result<Option<SourceTime>, MediaError> {
    let prefix = format!("[{filter} @ ");
    let mut timebase = None;
    for line in stderr.lines().filter(|l| l.starts_with(&prefix)) {
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
                .ok_or_else(|| MediaError::Invalid("thumbnail frame has no reliable PTS".into()))?;
            let timebase = timebase
                .ok_or_else(|| MediaError::Invalid("thumbnail frame timebase is missing".into()))?;
            return Ok(Some(SourceTime { ticks, timebase }));
        }
    }
    Ok(None)
}

impl FfmpegBackend {
    /// Decodes one batch of samples from one source. The outer error is a
    /// batch failure (process, timeout, cancellation); per-sample results
    /// distinguish a frame from `NoFrame` (EOF before the sample).
    pub fn extract_thumbnails(
        &self,
        media: &MediaSummary,
        height: u32,
        batch: &[ThumbnailRequest],
        cancellation: &Cancellation,
    ) -> Result<Vec<Result<Thumbnail, MediaError>>, MediaError> {
        if batch.is_empty() || batch.len() > MAX_BATCH {
            return Err(MediaError::Budget("thumbnail batch size"));
        }
        let metadata = crate::backend::video_of(media)?;
        metadata
            .validate()
            .map_err(|e| MediaError::Invalid(e.to_string()))?;
        for request in batch {
            validate_position(metadata, request.position)?;
        }
        let (width, height) = thumbnail_size(metadata, height)?;
        let frame_bytes = RgbaFrame::expected_bytes(width, height)?;
        let path = Self::input(&media.path)?;
        let clock = Timebase::new(1, 1_000_000).map_err(|_| MediaError::Invalid("clock".into()))?;
        let start_us = metadata
            .start()
            .rescale(clock, Rounding::Floor)
            .map_err(|e| MediaError::Invalid(e.to_string()))?
            .ticks;
        let id = RequestId::new().to_string().replace('-', "");
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
            "-filter_threads",
            "1",
            "-max_pixels",
            "8847360",
        ]);
        let mut graph = Vec::with_capacity(batch.len() + 1);
        for (n, request) in batch.iter().enumerate() {
            let target_us = request
                .position
                .rescale(clock, Rounding::Floor)
                .map_err(|e| MediaError::Invalid(e.to_string()))?
                .ticks;
            // Keyframe seek a little before the sample; real PTS selection follows.
            let preroll = target_us.saturating_sub(1_000_000).max(start_us);
            crate::backend::input_limits(&mut command);
            // `-threads` is a per-input decoder option: without it on every
            // input, inputs after the first decode with automatic threading and
            // a batch of 1080p seeks occupies several cores (measured ~2.7).
            command
                .args(["-threads", "1"])
                .args(["-noaccurate_seek", "-seek_timestamp", "1", "-ss"])
                .arg(crate::backend::format_micros(preroll))
                .arg("-i")
                .arg(&path);
            graph.push(format!(
                "[{n}:{stream}]select=gte(pts\\,{ticks}),scale={width}:{height}:flags=bilinear,setsar=1,format=rgb24,format=rgba,showinfo@cutcap_{id}_{n},trim=end_frame=1[v{n}]",
                stream = metadata.stream_index,
                ticks = request.position.ticks,
            ));
        }
        let inputs: String = (0..batch.len()).map(|n| format!("[v{n}]")).collect();
        graph.push(format!("{inputs}concat=n={}:v=1:a=0[out]", batch.len()));
        command.args(["-filter_complex", &graph.join(";")]).args([
            "-map",
            "[out]",
            "-an",
            "-sn",
            "-dn",
            "-fps_mode",
            "passthrough",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "rgba",
            "pipe:1",
        ]);
        let output = process::run_with(
            command,
            "Decode thumbnails",
            cancellation,
            self.decode_timeout,
            frame_bytes * batch.len(),
            1024 * 1024,
            process::Priority::BelowNormal,
        )?;
        let stderr = String::from_utf8_lossy(&output.stderr);
        let mut offset = 0;
        let mut results = Vec::with_capacity(batch.len());
        for (n, request) in batch.iter().enumerate() {
            match branch_pts(&stderr, &format!("showinfo@cutcap_{id}_{n}"))? {
                None => results.push(Err(MediaError::NoFrame)),
                Some(actual) => {
                    let end = offset + frame_bytes;
                    let pixels = output
                        .stdout
                        .get(offset..end)
                        .ok_or_else(|| MediaError::Invalid("thumbnail output is truncated".into()))?
                        .to_vec();
                    offset = end;
                    let threshold = request
                        .position
                        .rescale(actual.timebase, Rounding::Ceil)
                        .map_err(|e| MediaError::Invalid(e.to_string()))?;
                    results.push(if actual.ticks < threshold.ticks {
                        Err(MediaError::Invalid(
                            "decoder returned a thumbnail before the requested PTS".into(),
                        ))
                    } else {
                        RgbaFrame::new(width, height, width as usize * 4, pixels).map(|image| {
                            Thumbnail {
                                token: request.token,
                                requested: request.position,
                                actual,
                                image,
                                process_id: output.pid,
                            }
                        })
                    });
                }
            }
        }
        if offset != output.stdout.len() {
            return Err(MediaError::Invalid(
                "thumbnail output does not match its timestamp records".into(),
            ));
        }
        Ok(results)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn branch_records_require_the_exact_filter_and_report_empty_branches() {
        let log = "[showinfo@cutcap_ab_0 @ 0x1] config in time_base: 1/1000, frame_rate: 4/1\n\
                   [showinfo@cutcap_ab_0 @ 0x1] n:   0 pts:   1250 pts_time:1.25\n\
                   [showinfo@cutcap_ab_1 @ 0x2] config in time_base: 1/1000, frame_rate: 4/1\n\
                   title: [showinfo@cutcap_ab_1 @ 0x9] n: 0 pts: 999999\n";
        let first = branch_pts(log, "showinfo@cutcap_ab_0").unwrap().unwrap();
        assert_eq!(
            (first.ticks, first.timebase),
            (1250, Timebase::new(1, 1000).unwrap())
        );
        assert_eq!(
            branch_pts(log, "showinfo@cutcap_ab_1").unwrap(),
            None,
            "forged tag text is ignored"
        );
        assert_eq!(branch_pts(log, "showinfo@cutcap_ab_2").unwrap(), None);
    }

    #[test]
    fn sizes_follow_the_display_aspect_and_bounds() {
        let probe = br#"{"streams":[{"index":0,"codec_type":"video","codec_name":"h264","width":1920,"height":1080,"time_base":"1/15360","start_pts":0,"duration_ts":307200}],"format":{"format_name":"mov,mp4"}}"#;
        let meta = crate::parse_probe(probe).unwrap();
        assert_eq!(thumbnail_size(&meta, 60).unwrap(), (107, 60));
        assert_eq!(thumbnail_size(&meta, 4).unwrap(), (28, 16));
        assert_eq!(thumbnail_size(&meta, 1000).unwrap(), (384, 216));
        let rotated = br#"{"streams":[{"index":0,"codec_type":"video","codec_name":"h264","width":320,"height":180,"time_base":"1/12288","start_pts":0,"duration_ts":12288,"side_data_list":[{"side_data_type":"Display Matrix","rotation":-90}]}],"format":{"format_name":"mov,mp4"}}"#;
        let meta = crate::parse_probe(rotated).unwrap();
        let (w, h) = thumbnail_size(&meta, 64).unwrap();
        assert!(w < h, "rotated portrait source stays portrait: {w}x{h}");
        let bad = SourceTime {
            ticks: 0,
            timebase: Timebase::new(1, 1000).unwrap(),
        };
        assert!(validate_position(&meta, bad).is_err(), "wrong clock");
    }
}
