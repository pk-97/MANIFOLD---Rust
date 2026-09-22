//! CPU boundary for UI display captures.
//!
//! UI render targets are linear. This module validates the GPU readback shape,
//! converts it once to display encoded RGBA8, and owns the PNG metadata needed
//! by capture and snapshot tooling. Keeping the conversion here prevents each
//! caller from growing a slightly different channel, alpha, or gamma path.

use std::fmt;
use std::io;
use std::path::Path;

use half::f16;
use manifold_gpu::GpuTextureFormat;

/// How alpha in a linear UI readback is represented at the capture boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AlphaInterpretation {
    /// The source is opaque; retain linear RGB and emit opaque pixels.
    Opaque,
    /// RGB is premultiplied by alpha; restore straight RGB for a transparent PNG.
    Premultiplied,
    /// RGB is straight alpha; preserve it and the linear alpha channel.
    Straight,
    /// RGB is premultiplied by alpha; composite over black and emit opaque pixels.
    PremultipliedOverBlack,
    /// RGB is straight alpha; composite over black and emit opaque pixels.
    StraightOverBlack,
}

/// Errors raised while validating or writing a display capture.
#[derive(Debug)]
pub enum DisplayCaptureError {
    UnsupportedFormat(GpuTextureFormat),
    InvalidDimensions { width: u32, height: u32 },
    InvalidLength { expected: usize, actual: usize },
    Io(io::Error),
    Png(String),
}

impl fmt::Display for DisplayCaptureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedFormat(format) => {
                write!(f, "unsupported UI capture format: {format:?}")
            }
            Self::InvalidDimensions { width, height } => {
                write!(f, "invalid UI capture dimensions: {width}x{height}")
            }
            Self::InvalidLength { expected, actual } => {
                write!(
                    f,
                    "invalid UI capture length: expected {expected} bytes, got {actual}"
                )
            }
            Self::Io(error) => write!(f, "UI capture I/O failed: {error}"),
            Self::Png(error) => write!(f, "UI capture PNG encoding failed: {error}"),
        }
    }
}

impl std::error::Error for DisplayCaptureError {}

impl From<io::Error> for DisplayCaptureError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// Validated linear GPU bytes for a UI capture.
#[derive(Debug)]
pub struct LinearUiReadback<'a> {
    bytes: &'a [u8],
    width: u32,
    height: u32,
    format: GpuTextureFormat,
    alpha: AlphaInterpretation,
}

impl<'a> LinearUiReadback<'a> {
    /// Validate a tightly packed readback from a supported UI target.
    pub fn from_bytes(
        bytes: &'a [u8],
        width: u32,
        height: u32,
        format: GpuTextureFormat,
        alpha: AlphaInterpretation,
    ) -> Result<Self, DisplayCaptureError> {
        let bytes_per_pixel = match format {
            GpuTextureFormat::Bgra8Unorm | GpuTextureFormat::Rgba16Float => {
                format.bytes_per_pixel() as usize
            }
            _ => return Err(DisplayCaptureError::UnsupportedFormat(format)),
        };
        if width == 0 || height == 0 {
            return Err(DisplayCaptureError::InvalidDimensions { width, height });
        }
        let expected = (width as usize)
            .checked_mul(height as usize)
            .and_then(|pixels| pixels.checked_mul(bytes_per_pixel))
            .ok_or(DisplayCaptureError::InvalidLength {
                expected: usize::MAX,
                actual: bytes.len(),
            })?;
        if bytes.len() != expected {
            return Err(DisplayCaptureError::InvalidLength {
                expected,
                actual: bytes.len(),
            });
        }
        Ok(Self {
            bytes,
            width,
            height,
            format,
            alpha,
        })
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    /// Convert linear RGB and linear alpha to display-encoded RGBA8 bytes.
    pub fn to_srgb_rgba8(&self) -> SrgbRgba8 {
        let mut out = Vec::with_capacity(self.width as usize * self.height as usize * 4);
        let bpp = self.format.bytes_per_pixel() as usize;
        for pixel in self.bytes.chunks_exact(bpp) {
            let (mut r, mut g, mut b, a) = self.decode_pixel(pixel);
            let alpha = sanitize_unit(a);
            match self.alpha {
                AlphaInterpretation::Opaque => {
                    out.push(crate::headless_readback::linear_to_srgb8(r));
                    out.push(crate::headless_readback::linear_to_srgb8(g));
                    out.push(crate::headless_readback::linear_to_srgb8(b));
                    out.push(u8::MAX);
                }
                AlphaInterpretation::Premultiplied => {
                    if alpha > 0.0 {
                        r /= alpha;
                        g /= alpha;
                        b /= alpha;
                    } else {
                        r = 0.0;
                        g = 0.0;
                        b = 0.0;
                    }
                    out.push(crate::headless_readback::linear_to_srgb8(r));
                    out.push(crate::headless_readback::linear_to_srgb8(g));
                    out.push(crate::headless_readback::linear_to_srgb8(b));
                    out.push(linear_alpha_to_u8(alpha));
                }
                AlphaInterpretation::PremultipliedOverBlack => {
                    out.push(crate::headless_readback::linear_to_srgb8(r));
                    out.push(crate::headless_readback::linear_to_srgb8(g));
                    out.push(crate::headless_readback::linear_to_srgb8(b));
                    out.push(u8::MAX);
                }
                AlphaInterpretation::StraightOverBlack => {
                    r *= alpha;
                    g *= alpha;
                    b *= alpha;
                    out.push(crate::headless_readback::linear_to_srgb8(r));
                    out.push(crate::headless_readback::linear_to_srgb8(g));
                    out.push(crate::headless_readback::linear_to_srgb8(b));
                    out.push(u8::MAX);
                }
                AlphaInterpretation::Straight => {
                    out.push(crate::headless_readback::linear_to_srgb8(r));
                    out.push(crate::headless_readback::linear_to_srgb8(g));
                    out.push(crate::headless_readback::linear_to_srgb8(b));
                    out.push(linear_alpha_to_u8(alpha));
                }
            }
        }
        SrgbRgba8::new(out, self.width, self.height)
    }

    fn decode_pixel(&self, pixel: &[u8]) -> (f32, f32, f32, f32) {
        match self.format {
            GpuTextureFormat::Bgra8Unorm => {
                let b = f32::from(pixel[0]) / 255.0;
                let g = f32::from(pixel[1]) / 255.0;
                let r = f32::from(pixel[2]) / 255.0;
                let a = f32::from(pixel[3]) / 255.0;
                (r, g, b, a)
            }
            GpuTextureFormat::Rgba16Float => {
                let r = f16::from_bits(u16::from_le_bytes([pixel[0], pixel[1]])).to_f32();
                let g = f16::from_bits(u16::from_le_bytes([pixel[2], pixel[3]])).to_f32();
                let b = f16::from_bits(u16::from_le_bytes([pixel[4], pixel[5]])).to_f32();
                let a = f16::from_bits(u16::from_le_bytes([pixel[6], pixel[7]])).to_f32();
                (
                    sanitize_linear(r),
                    sanitize_linear(g),
                    sanitize_linear(b),
                    sanitize_linear(a),
                )
            }
            _ => unreachable!("format validated by LinearUiReadback::from_bytes"),
        }
    }
}

fn sanitize_linear(value: f32) -> f32 {
    if value.is_finite() {
        value.max(0.0)
    } else {
        0.0
    }
}

fn sanitize_unit(value: f32) -> f32 {
    sanitize_linear(value).min(1.0)
}

fn linear_alpha_to_u8(alpha: f32) -> u8 {
    (sanitize_unit(alpha) * 255.0).round() as u8
}

/// Owned display-encoded RGBA8 pixels. The only constructors are conversion
/// and encoded filmstrip assembly, so callers cannot label arbitrary linear
/// bytes as already encoded.
pub struct SrgbRgba8 {
    bytes: Vec<u8>,
    width: u32,
    height: u32,
}

impl SrgbRgba8 {
    fn new(bytes: Vec<u8>, width: u32, height: u32) -> Self {
        debug_assert_eq!(bytes.len(), (width * height * 4) as usize);
        Self {
            bytes,
            width,
            height,
        }
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Assemble already encoded tiles into a contact sheet. Conversion must
    /// happen before this operation so each tile remains in display space.
    pub fn assemble_filmstrip(
        tiles: &[SrgbRgba8],
        tile_w: u32,
        tile_h: u32,
        cols: u32,
    ) -> Result<Self, DisplayCaptureError> {
        if tiles.is_empty() || cols == 0 || tile_w == 0 || tile_h == 0 {
            return Err(DisplayCaptureError::InvalidDimensions {
                width: tile_w,
                height: tile_h,
            });
        }
        if tiles
            .iter()
            .any(|tile| tile.width != tile_w || tile.height != tile_h)
        {
            return Err(DisplayCaptureError::InvalidLength {
                expected: tile_w as usize * tile_h as usize * 4,
                actual: 0,
            });
        }
        let rows = (tiles.len() as u32).div_ceil(cols);
        let width = tile_w
            .checked_mul(cols)
            .ok_or(DisplayCaptureError::InvalidDimensions {
                width: tile_w,
                height: tile_h,
            })?;
        let height = tile_h
            .checked_mul(rows)
            .ok_or(DisplayCaptureError::InvalidDimensions {
                width: tile_w,
                height: tile_h,
            })?;
        let mut bytes = vec![0; (width as usize) * (height as usize) * 4];
        for pixel in bytes.chunks_exact_mut(4) {
            pixel[3] = 255;
        }
        let tile_row_bytes = tile_w as usize * 4;
        let sheet_row_bytes = width as usize * 4;
        for (index, tile) in tiles.iter().enumerate() {
            let index = index as u32;
            let x = index % cols;
            let y = index / cols;
            for row in 0..tile_h as usize {
                let source = row * tile_row_bytes;
                let target = ((y as usize * tile_h as usize + row) * sheet_row_bytes)
                    + x as usize * tile_row_bytes;
                bytes[target..target + tile_row_bytes]
                    .copy_from_slice(&tile.bytes[source..source + tile_row_bytes]);
            }
        }
        Ok(Self::new(bytes, width, height))
    }

    /// Encode RGBA8 pixels as a PNG carrying an explicit sRGB rendering-intent
    /// chunk.
    pub fn encode_png(&self) -> Result<Vec<u8>, DisplayCaptureError> {
        let mut png = Vec::new();
        let mut encoder = png::Encoder::new(&mut png, self.width, self.height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_source_srgb(png::SrgbRenderingIntent::Perceptual);
        let mut writer = encoder
            .write_header()
            .map_err(|error| DisplayCaptureError::Png(error.to_string()))?;
        writer
            .write_image_data(&self.bytes)
            .map_err(|error| DisplayCaptureError::Png(error.to_string()))?;
        drop(writer);
        Ok(png)
    }

    pub fn write_png(&self, path: &Path) -> Result<(), DisplayCaptureError> {
        std::fs::write(path, self.encode_png()?)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bgra8(pixel: [u8; 4]) -> Vec<u8> {
        pixel.to_vec()
    }

    fn rgba16(pixel: [f32; 4]) -> Vec<u8> {
        pixel
            .into_iter()
            .flat_map(|value| f16::from_f32(value).to_bits().to_le_bytes())
            .collect()
    }

    #[test]
    fn linear_grey_216_maps_to_srgb_midpoint() {
        let raw = rgba16([0.216, 0.216, 0.216, 1.0]);
        let encoded = LinearUiReadback::from_bytes(
            &raw,
            1,
            1,
            GpuTextureFormat::Rgba16Float,
            AlphaInterpretation::Straight,
        )
        .unwrap()
        .to_srgb_rgba8();
        assert!((126..=130).contains(&encoded.as_bytes()[0]));
    }

    #[test]
    fn bgra_channels_are_reordered() {
        let encoded = LinearUiReadback::from_bytes(
            &bgra8([0, 64, 128, 255]),
            1,
            1,
            GpuTextureFormat::Bgra8Unorm,
            AlphaInterpretation::Straight,
        )
        .unwrap()
        .to_srgb_rgba8();
        assert_eq!(&encoded.as_bytes()[..4], &[188, 137, 0, 255]);
    }

    #[test]
    fn premultiplied_alpha_is_unpremultiplied_before_encoding() {
        let raw = rgba16([0.1, 0.05, 0.0, 0.5]);
        let encoded = LinearUiReadback::from_bytes(
            &raw,
            1,
            1,
            GpuTextureFormat::Rgba16Float,
            AlphaInterpretation::Premultiplied,
        )
        .unwrap()
        .to_srgb_rgba8();
        // Straight red is approximately 0.2 linear: sRGB 0.4845 rounds to 124.
        assert_eq!(&encoded.as_bytes()[..4], &[124, 89, 0, 128]);
    }

    #[test]
    fn low_f16_values_remain_distinguishable() {
        let raw = [
            rgba16([0.0005, 0.0, 0.0, 1.0]),
            rgba16([0.001, 0.0, 0.0, 1.0]),
        ]
        .concat();
        let encoded = LinearUiReadback::from_bytes(
            &raw,
            2,
            1,
            GpuTextureFormat::Rgba16Float,
            AlphaInterpretation::Straight,
        )
        .unwrap()
        .to_srgb_rgba8();
        assert_ne!(encoded.as_bytes()[0], encoded.as_bytes()[4]);
    }

    #[test]
    fn alpha_is_linear_not_gamma_encoded() {
        let raw = rgba16([0.0, 0.0, 0.0, 0.216]);
        let encoded = LinearUiReadback::from_bytes(
            &raw,
            1,
            1,
            GpuTextureFormat::Rgba16Float,
            AlphaInterpretation::Straight,
        )
        .unwrap()
        .to_srgb_rgba8();
        assert_eq!(encoded.as_bytes()[3], 55);
    }

    #[test]
    fn opaque_capture_composites_over_black() {
        let raw = rgba16([0.8, 0.4, 0.0, 0.5]);
        let encoded = LinearUiReadback::from_bytes(
            &raw,
            1,
            1,
            GpuTextureFormat::Rgba16Float,
            AlphaInterpretation::StraightOverBlack,
        )
        .unwrap()
        .to_srgb_rgba8();
        assert_eq!(&encoded.as_bytes()[..4], &[170, 124, 0, 255]);
    }

    #[test]
    fn premultiplied_capture_and_filmstrip_padding_are_opaque() {
        let raw = rgba16([0.1, 0.05, 0.0, 0.5]);
        let tile = LinearUiReadback::from_bytes(
            &raw,
            1,
            1,
            GpuTextureFormat::Rgba16Float,
            AlphaInterpretation::PremultipliedOverBlack,
        )
        .unwrap()
        .to_srgb_rgba8();
        assert_eq!(tile.as_bytes(), &[89, 63, 0, 255]);
        let sheet = SrgbRgba8::assemble_filmstrip(&[tile], 1, 1, 2).unwrap();
        assert_eq!(sheet.as_bytes(), &[89, 63, 0, 255, 0, 0, 0, 255]);
    }

    #[test]
    fn malformed_lengths_and_formats_are_rejected() {
        let error = LinearUiReadback::from_bytes(
            &[0; 3],
            1,
            1,
            GpuTextureFormat::Bgra8Unorm,
            AlphaInterpretation::Straight,
        )
        .unwrap_err();
        assert!(matches!(error, DisplayCaptureError::InvalidLength { .. }));
        let error = LinearUiReadback::from_bytes(
            &[0; 4],
            1,
            1,
            GpuTextureFormat::Rgba8Unorm,
            AlphaInterpretation::Straight,
        )
        .unwrap_err();
        assert!(matches!(
            error,
            DisplayCaptureError::UnsupportedFormat(GpuTextureFormat::Rgba8Unorm)
        ));
    }

    #[test]
    fn png_round_trip_has_explicit_srgb_chunk() {
        let raw = rgba16([0.216, 0.1, 0.0, 1.0]);
        let encoded = LinearUiReadback::from_bytes(
            &raw,
            1,
            1,
            GpuTextureFormat::Rgba16Float,
            AlphaInterpretation::Straight,
        )
        .unwrap()
        .to_srgb_rgba8();
        let png = encoded.encode_png().unwrap();
        assert!(png.windows(4).any(|chunk| chunk == b"sRGB"));
        let decoded = image::load_from_memory(&png).unwrap().into_rgba8();
        assert_eq!(decoded.as_raw(), encoded.as_bytes());
    }
}
