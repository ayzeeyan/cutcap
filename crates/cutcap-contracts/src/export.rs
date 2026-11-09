//! The Phase 5A export preset and exact picture fit. No I/O or codec types.
use crate::{MetadataError, VideoMetadata};

/// Longest timeline one export may render (four hours).
pub const MAX_EXPORT_SECONDS: u64 = 14_400;
/// How far ahead of an output instant a clip's first eligible frame may be
/// shown at the clip head (PHASE_5A_EXPORT_CONTRACT.md §5).
pub const HEAD_HOLD_MS: u32 = 100;

/// What one export produces. There is one preset; see the contract §2.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExportPreset {
    pub name: &'static str,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub sample_rate: u32,
    pub channels: u16,
    pub crf: u32,
    pub speed: &'static str,
    pub audio_bitrate_kbps: u32,
}

impl ExportPreset {
    pub const STANDARD: Self = Self {
        name: "CutCap Standard MP4",
        width: 1280,
        height: 720,
        fps: 30,
        sample_rate: 48_000,
        channels: 2,
        crf: 23,
        speed: "veryfast",
        audio_bitrate_kbps: 192,
    };

    /// Bytes of one 8-bit YUV 4:2:0 frame.
    pub const fn frame_bytes(self) -> usize {
        (self.width as usize) * (self.height as usize) * 3 / 2
    }

    /// Bytes of one interleaved `f32` sample frame.
    pub const fn sample_frame_bytes(self) -> u64 {
        self.channels as u64 * 4
    }

    /// One line for people: format, picture, video and audio settings.
    pub fn summary(self) -> String {
        format!(
            "MP4 · {}×{} · {} fps · H.264 CRF {} · AAC {} kbps stereo",
            self.width, self.height, self.fps, self.crf, self.audio_bitrate_kbps
        )
    }
}

/// Where a source's picture lands inside the output frame. All values are
/// even, so the 4:2:0 chroma planes stay aligned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PictureFit {
    pub width: u32,
    pub height: u32,
    pub pad_x: u32,
    pub pad_y: u32,
}

/// The even integer nearest to `numerator / denominator`, at least 2.
fn nearest_even(numerator: u64, denominator: u64) -> u32 {
    let halves = numerator.saturating_add(denominator) / denominator.saturating_mul(2).max(1);
    u32::try_from(halves.saturating_mul(2).max(2)).unwrap_or(u32::MAX)
}

impl VideoMetadata {
    /// The fitted picture for an output frame: the whole display picture,
    /// undistorted and centred, with the remainder left for black padding.
    pub fn fit_within(&self, frame_w: u32, frame_h: u32) -> Result<PictureFit, MetadataError> {
        self.validate()?;
        let (n, d) = self.oriented_aspect();
        let (w, h) = (u64::from(frame_w), u64::from(frame_h));
        let (width, height) = if n.saturating_mul(h) >= d.saturating_mul(w) {
            (frame_w, nearest_even(w.saturating_mul(d), n).min(frame_h))
        } else {
            (nearest_even(h.saturating_mul(n), d).min(frame_w), frame_h)
        };
        // Centred; the offsets stay even because both sizes are even.
        let centre = |space: u32, used: u32| (space - used) / 4 * 2;
        Ok(PictureFit {
            width,
            height,
            pad_x: centre(frame_w, width),
            pad_y: centre(frame_h, height),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Ratio, Timebase};

    fn video(width: u32, height: u32) -> VideoMetadata {
        VideoMetadata {
            codec: "h264".into(),
            width,
            height,
            pixel_format: Some("yuv420p".into()),
            sample_aspect: None,
            display_aspect: None,
            rotation: 0,
            average_rate: None,
            nominal_rate: None,
            timebase: Timebase::new(1, 15_360).unwrap(),
            start_pts: 0,
            duration_ticks: Some(15_360),
            duration_is_estimate: false,
            container: "mov,mp4".into(),
            stream_index: 0,
            stream_id: None,
            audio_streams: Vec::new(),
            audio: Vec::new(),
            color_space: None,
            color_transfer: None,
            color_range: None,
        }
    }

    fn fit(video: &VideoMetadata) -> (u32, u32, u32, u32) {
        let f = video.fit_within(1280, 720).unwrap();
        (f.width, f.height, f.pad_x, f.pad_y)
    }

    #[test]
    fn matching_aspect_fills_the_frame_at_any_resolution() {
        for (w, h) in [(1280, 720), (1920, 1080), (640, 360), (3840, 2160)] {
            assert_eq!(fit(&video(w, h)), (1280, 720, 0, 0));
        }
    }

    #[test]
    fn wider_pictures_letterbox_and_narrower_ones_pillarbox() {
        assert_eq!(fit(&video(1920, 800)), (1280, 534, 0, 92));
        assert_eq!(fit(&video(640, 480)), (960, 720, 160, 0));
        assert_eq!(fit(&video(1080, 1920)), (406, 720, 436, 0));
    }

    #[test]
    fn rotation_turns_the_picture_before_it_is_fitted() {
        let mut phone = video(1920, 1080);
        phone.rotation = 90;
        assert_eq!(fit(&phone), (406, 720, 436, 0));
        phone.rotation = 180;
        assert_eq!(fit(&phone), (1280, 720, 0, 0));
    }

    #[test]
    fn non_square_pixels_use_the_display_shape_not_the_stored_size() {
        // 720x576 anamorphic PAL displayed 16:9.
        let mut pal = video(720, 576);
        pal.sample_aspect = Some(Ratio {
            numerator: 64,
            denominator: 45,
        });
        assert_eq!(fit(&pal), (1280, 720, 0, 0));
        // The stored display aspect, when present, wins.
        pal.display_aspect = Some(Ratio {
            numerator: 4,
            denominator: 3,
        });
        assert_eq!(fit(&pal), (960, 720, 160, 0));
    }

    #[test]
    fn every_fit_is_even_centred_and_inside_the_frame() {
        for (w, h) in [(1279, 719), (333, 777), (1000, 3), (3, 1000), (1024, 1023)] {
            let f = video(w, h).fit_within(1280, 720).unwrap();
            assert_eq!(
                f.width % 2 + f.height % 2 + f.pad_x % 2 + f.pad_y % 2,
                0,
                "{w}x{h}"
            );
            assert!(f.width >= 2 && f.height >= 2);
            assert!(f.width + 2 * f.pad_x <= 1280 && f.height + 2 * f.pad_y <= 720);
            assert!(1280 - (f.width + 2 * f.pad_x) < 4 && 720 - (f.height + 2 * f.pad_y) < 4);
        }
    }
}
