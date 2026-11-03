use crate::MediaError;
use cutcap_contracts::{MediaId, ProjectId, RequestId, SourceTime};

pub const MAX_PREVIEW_WIDTH: u32 = 1280;
pub const MAX_PREVIEW_HEIGHT: u32 = 720;
pub const MAX_FRAME_BYTES: usize = 1280 * 720 * 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PreviewTicket {
    pub project_id: ProjectId,
    pub media_id: MediaId,
    pub request_id: RequestId,
}

/// Packed RGBA8, straight/opaque alpha, row-major, top row first. Not serialized.
#[derive(Debug)]
pub struct RgbaFrame {
    width: u32,
    height: u32,
    stride: usize,
    pixels: Vec<u8>,
}

impl RgbaFrame {
    pub fn expected_bytes(width: u32, height: u32) -> Result<usize, MediaError> {
        if width == 0 || height == 0 || width > MAX_PREVIEW_WIDTH || height > MAX_PREVIEW_HEIGHT {
            return Err(MediaError::Budget("invalid preview dimensions"));
        }
        let bytes = (width as usize)
            .checked_mul(height as usize)
            .and_then(|n| n.checked_mul(4))
            .ok_or(MediaError::Budget("frame size overflow"))?;
        if bytes > MAX_FRAME_BYTES {
            return Err(MediaError::Budget("preview byte limit"));
        }
        Ok(bytes)
    }
    pub fn new(
        width: u32,
        height: u32,
        stride: usize,
        pixels: Vec<u8>,
    ) -> Result<Self, MediaError> {
        let expected = Self::expected_bytes(width, height)?;
        if stride != width as usize * 4
            || pixels.len() != expected
            || pixels.as_chunks::<4>().0.iter().any(|p| p[3] != 255)
        {
            return Err(MediaError::Invalid(
                "Invalid packed opaque RGBA buffer".into(),
            ));
        }
        Ok(Self {
            width,
            height,
            stride,
            pixels,
        })
    }
    pub fn width(&self) -> u32 {
        self.width
    }
    pub fn height(&self) -> u32 {
        self.height
    }
    pub fn stride(&self) -> usize {
        self.stride
    }
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }
}

#[derive(Debug)]
pub struct DecodedFrame {
    pub ticket: PreviewTicket,
    pub requested: SourceTime,
    /// Original PTS, parsed from showinfo with its explicit filter timebase.
    pub actual: SourceTime,
    pub image: RgbaFrame,
    pub decode_us: u64,
    pub process_id: u32,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_bad_dimensions_stride_length_and_alpha() {
        for (width, height) in [
            (0, 1),
            (1, 0),
            (1281, 720),
            (1280, 721),
            (u32::MAX, u32::MAX),
        ] {
            assert!(RgbaFrame::expected_bytes(width, height).is_err());
        }
        assert_eq!(
            RgbaFrame::expected_bytes(1280, 720).unwrap(),
            MAX_FRAME_BYTES
        );
        assert!(RgbaFrame::new(1, 1, 8, vec![0, 0, 0, 255]).is_err());
        assert!(RgbaFrame::new(1, 1, 4, vec![0, 0, 0]).is_err());
        assert!(RgbaFrame::new(1, 1, 4, vec![0, 0, 0, 255, 0]).is_err());
        assert!(RgbaFrame::new(1, 1, 4, vec![0, 0, 0, 0]).is_err());
        let valid = RgbaFrame::new(1, 1, 4, vec![255, 20, 0, 255]).unwrap();
        assert_eq!(valid.stride(), 4);
        assert_eq!(valid.pixels(), &[255, 20, 0, 255]);
    }
}
