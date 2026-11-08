//! Probe metadata is a validated session cache, not decoded data or project edits.
use crate::{MediaId, SourceTime, Timebase};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const MAX_SOURCE_EDGE: u32 = 4096;
pub const MAX_SOURCE_PIXELS: u64 = 4096 * 2160;
pub const MAX_MEDIA_SECONDS: i64 = 24 * 60 * 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ratio {
    pub numerator: u32,
    pub denominator: u32,
}

impl Ratio {
    pub fn validate(self) -> Result<(), MetadataError> {
        if self.numerator == 0
            || self.denominator == 0
            || self.numerator > 1_000_000
            || self.denominator > 1_000_000
        {
            return Err(MetadataError::Invalid("invalid positive ratio"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VideoMetadata {
    pub codec: String,
    pub width: u32,
    pub height: u32,
    pub pixel_format: Option<String>,
    pub sample_aspect: Option<Ratio>,
    pub display_aspect: Option<Ratio>,
    /// Counterclockwise display-matrix angle reported by FFprobe; normalized.
    pub rotation: u16,
    pub average_rate: Option<Ratio>,
    pub nominal_rate: Option<Ratio>,
    pub timebase: Timebase,
    pub start_pts: i64,
    pub duration_ticks: Option<i64>,
    pub duration_is_estimate: bool,
    pub container: String,
    pub stream_index: u32,
    pub stream_id: Option<String>,
    pub audio_streams: Vec<u32>,
    /// Validated details of the decodable audio streams (Phase 4B). Absent in
    /// metadata saved before Phase 4B, which deserializes as empty.
    #[serde(default)]
    pub audio: Vec<AudioStreamMetadata>,
    pub color_space: Option<String>,
    pub color_transfer: Option<String>,
    pub color_range: Option<String>,
}

impl VideoMetadata {
    pub fn validate(&self) -> Result<(), MetadataError> {
        if self.width == 0
            || self.height == 0
            || self.width > MAX_SOURCE_EDGE
            || self.height > MAX_SOURCE_EDGE
            || u64::from(self.width) * u64::from(self.height) > MAX_SOURCE_PIXELS
        {
            return Err(MetadataError::Invalid(
                "source exceeds the 4096-edge / 4K pixel budget or has invalid dimensions",
            ));
        }
        if self.codec.is_empty() || self.container.is_empty() {
            return Err(MetadataError::Invalid("missing codec or container"));
        }
        for text in [
            Some(&self.codec),
            Some(&self.container),
            self.pixel_format.as_ref(),
            self.stream_id.as_ref(),
            self.color_space.as_ref(),
            self.color_transfer.as_ref(),
            self.color_range.as_ref(),
        ]
        .into_iter()
        .flatten()
        {
            if text.len() > 256 || text.chars().any(char::is_control) {
                return Err(MetadataError::Invalid("invalid metadata text"));
            }
        }
        for ratio in [
            self.sample_aspect,
            self.display_aspect,
            self.average_rate,
            self.nominal_rate,
        ]
        .into_iter()
        .flatten()
        {
            ratio.validate()?;
        }
        if !matches!(self.rotation, 0 | 90 | 180 | 270)
            || self.audio_streams.len() > 64
            || self.stream_index >= 64
            || self.audio_streams.iter().any(|i| *i >= 64)
        {
            return Err(MetadataError::Invalid(
                "unsupported orientation or stream identifiers",
            ));
        }
        // FFmpeg's select expression uses doubles; integers inside this bound are exact.
        const MAX_PTS: i64 = 1_i64 << 52;
        if self.start_pts.unsigned_abs() > MAX_PTS as u64
            || self.timebase.numerator() > 1_000_000_000
            || self.timebase.denominator() > 1_000_000_000
        {
            return Err(MetadataError::Invalid(
                "source clock exceeds the supported timestamp budget",
            ));
        }
        if let Some(duration) = self.duration_ticks {
            let seconds_numerator = i128::from(duration) * i128::from(self.timebase.numerator());
            if duration <= 0
                || seconds_numerator
                    > i128::from(MAX_MEDIA_SECONDS) * i128::from(self.timebase.denominator())
                || self
                    .start_pts
                    .checked_add(duration)
                    .is_none_or(|end| end.unsigned_abs() > MAX_PTS as u64)
            {
                return Err(MetadataError::Invalid(
                    "invalid duration or media longer than 24 hours",
                ));
            }
        }
        if self.audio.len() > self.audio_streams.len() {
            return Err(MetadataError::Invalid(
                "audio details exceed the audio streams",
            ));
        }
        for audio in &self.audio {
            audio.validate()?;
            if !self.audio_streams.contains(&audio.stream_index)
                || self
                    .audio
                    .iter()
                    .filter(|a| a.stream_index == audio.stream_index)
                    .count()
                    != 1
            {
                return Err(MetadataError::Invalid(
                    "audio details do not match the streams",
                ));
            }
        }
        if matches!(
            self.color_transfer.as_deref(),
            Some("smpte2084" | "arib-std-b67")
        ) || matches!(self.color_space.as_deref(), Some("bt2020nc" | "bt2020c"))
        {
            return Err(MetadataError::Invalid(
                "HDR / BT.2020 preview requires a color-managed adapter and is not supported yet",
            ));
        }
        Ok(())
    }

    pub fn start(&self) -> SourceTime {
        SourceTime {
            ticks: self.start_pts,
            timebase: self.timebase,
        }
    }

    /// The audio stream a linked placement uses: the first stream marked as
    /// the container's default, otherwise the lowest stream index. Only
    /// streams with validated details are eligible.
    pub fn primary_audio(&self) -> Option<&AudioStreamMetadata> {
        self.audio
            .iter()
            .filter(|a| a.default)
            .min_by_key(|a| a.stream_index)
            .or_else(|| self.audio.iter().min_by_key(|a| a.stream_index))
    }

    pub fn audio_stream(&self, index: u32) -> Option<&AudioStreamMetadata> {
        self.audio.iter().find(|a| a.stream_index == index)
    }

    /// Oriented square-pixel preview, bounded at 1280 x 720 (portrait included).
    pub fn preview_dimensions(&self) -> Result<(u32, u32), MetadataError> {
        self.validate()?;
        let sar = self.sample_aspect.unwrap_or(Ratio {
            numerator: 1,
            denominator: 1,
        });
        let (mut n, mut d) = if let Some(dar) = self.display_aspect {
            (u64::from(dar.numerator), u64::from(dar.denominator))
        } else {
            (
                u64::from(self.width) * u64::from(sar.numerator),
                u64::from(self.height) * u64::from(sar.denominator),
            )
        };
        if matches!(self.rotation, 90 | 270) {
            std::mem::swap(&mut n, &mut d);
        }
        let height = u64::from(if matches!(self.rotation, 90 | 270) {
            self.width
        } else {
            self.height
        })
        .min(720);
        if n * height >= d * 1280 {
            Ok((
                1280,
                u32::try_from((1280 * d / n).max(1))
                    .map_err(|_| MetadataError::Invalid("aspect overflow"))?,
            ))
        } else {
            Ok((
                u32::try_from((height * n / d).max(1))
                    .map_err(|_| MetadataError::Invalid("aspect overflow"))?,
                height as u32,
            ))
        }
    }
}

/// One decodable audio stream as FFprobe reported it. Audio and video keep
/// independent clocks: `timebase` and `start_pts` belong to this stream only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AudioStreamMetadata {
    pub stream_index: u32,
    pub stream_id: Option<String>,
    pub codec: String,
    pub timebase: Timebase,
    pub start_pts: i64,
    pub duration_ticks: Option<i64>,
    pub duration_is_estimate: bool,
    pub sample_rate: u32,
    pub channels: u16,
    pub channel_layout: Option<String>,
    pub sample_format: Option<String>,
    /// Encoder priming samples the decoder discards (for example AAC's 1024).
    pub initial_padding: Option<u32>,
    /// The container marks this stream as its default.
    pub default: bool,
}

/// Decoding limits for audio: rates FFmpeg's resampler accepts and at most
/// eight channels (downmixed to the output's two).
pub const MAX_AUDIO_RATE: u32 = 384_000;
pub const MAX_AUDIO_CHANNELS: u16 = 8;

impl AudioStreamMetadata {
    pub fn validate(&self) -> Result<(), MetadataError> {
        if self.stream_index >= 64
            || self.sample_rate == 0
            || self.sample_rate > MAX_AUDIO_RATE
            || self.channels == 0
            || self.channels > MAX_AUDIO_CHANNELS
        {
            return Err(MetadataError::Invalid(
                "unsupported audio stream index, rate or channel count",
            ));
        }
        if self.codec.is_empty() {
            return Err(MetadataError::Invalid("missing audio codec"));
        }
        for text in [
            Some(&self.codec),
            self.stream_id.as_ref(),
            self.channel_layout.as_ref(),
            self.sample_format.as_ref(),
        ]
        .into_iter()
        .flatten()
        {
            if text.len() > 256 || text.chars().any(char::is_control) {
                return Err(MetadataError::Invalid("invalid audio metadata text"));
            }
        }
        const MAX_PTS: i64 = 1_i64 << 52;
        if self.start_pts.unsigned_abs() > MAX_PTS as u64
            || self.timebase.numerator() > 1_000_000_000
            || self.timebase.denominator() > 1_000_000_000
        {
            return Err(MetadataError::Invalid(
                "audio clock exceeds the supported timestamp budget",
            ));
        }
        if let Some(duration) = self.duration_ticks {
            let seconds_numerator = i128::from(duration) * i128::from(self.timebase.numerator());
            if duration <= 0
                || seconds_numerator
                    > i128::from(MAX_MEDIA_SECONDS) * i128::from(self.timebase.denominator())
                || self
                    .start_pts
                    .checked_add(duration)
                    .is_none_or(|end| end.unsigned_abs() > MAX_PTS as u64)
            {
                return Err(MetadataError::Invalid(
                    "invalid audio duration or longer than 24 hours",
                ));
            }
        }
        Ok(())
    }

    pub fn start(&self) -> SourceTime {
        SourceTime {
            ticks: self.start_pts,
            timebase: self.timebase,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MetadataError {
    #[error("{0}")]
    Invalid(&'static str),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MediaSummary {
    pub id: MediaId,
    pub path: PathBuf,
    pub metadata: VideoMetadata,
}
