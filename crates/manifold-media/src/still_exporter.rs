//! Still-frame export — encode a single composited frame to PNG or JPEG.
//!
//! The content thread reads back the final compositor output as tightly-packed
//! RGBA8 (see `manifold-renderer`'s `gpu_readback`). This module turns those raw
//! pixels into an image file. PNG is lossless and keeps the alpha channel; JPEG
//! is lossy and drops alpha to opaque RGB at a chosen quality (some platforms —
//! Spotify / distributor cover-art upload — require JPEG).
//!
//! Encoding + disk I/O are heavy enough (a 4000×4000 frame is ~64 MB of RGBA)
//! that the caller runs `save_still` off the 60 FPS content thread.
//!
//! Colour: the compositor stores *linear* colour in an Rgba16Float texture; the
//! display applies the sRGB transfer function at scanout (the surface is tagged
//! `ExtendedLinearSRGB` — see `manifold-app/edr_surface.rs`). So a faithful
//! still must apply the same sRGB encode itself — [`linear_f16_rgba_to_srgb8`]
//! does that, in float, before quantizing to 8-bit. On HDR displays the live
//! frame carries brighter-than-white (EDR) highlights; the optional rolloff
//! compresses those into SDR white instead of hard-clipping.

use std::io::BufWriter;
use std::path::Path;

use image::ImageEncoder;

/// Standard sRGB opto-electronic transfer function (linear → display-encoded),
/// applied per channel in float. Input is clamped to [0, 1] first.
pub(crate) fn linear_to_srgb(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    if x <= 0.0031308 {
        x * 12.92
    } else {
        1.055 * x.powf(1.0 / 2.4) - 0.055
    }
}

/// Soft highlight rolloff mapping linear [0, ∞) → [0, 1). Identity below the
/// knee, a smooth `tanh` shoulder above it — so brighter-than-white EDR
/// highlights compress gracefully into SDR white instead of hard-clipping.
/// Mirrors the EDR-passthrough soft-clip in the tonemap shader, targeting SDR
/// white (1.0) rather than the display peak.
fn highlight_rolloff(x: f32) -> f32 {
    const KNEE: f32 = 0.8;
    if x <= KNEE {
        x
    } else {
        KNEE + (1.0 - KNEE) * ((x - KNEE) / (1.0 - KNEE)).tanh()
    }
}

/// Convert IEEE 754 half-precision (f16) bits to f32.
fn f16_to_f32(bits: u16) -> f32 {
    let sign = ((bits >> 15) & 1) as u32;
    let exp = ((bits >> 10) & 0x1f) as u32;
    let frac = (bits & 0x3ff) as u32;
    if exp == 0 {
        if frac == 0 {
            f32::from_bits(sign << 31)
        } else {
            let val = (frac as f32) * (1.0 / 1024.0) * (1.0 / 16384.0); // 2^-14
            if sign == 1 { -val } else { val }
        }
    } else if exp == 31 {
        f32::from_bits((sign << 31) | (0xff << 23) | (frac << 13))
    } else {
        f32::from_bits((sign << 31) | ((exp + 112) << 23) | (frac << 13))
    }
}

/// Convert tightly-packed linear `Rgba16Float` pixels (8 bytes/px, little-endian
/// half floats, stride = `width * 8`) to sRGB-encoded `RGBA8` matching the
/// on-screen image. RGB is sRGB-encoded; alpha is passed through linearly (alpha
/// is not gamma). When `rolloff` is set, brighter-than-white highlights are
/// softly compressed into white before encoding. Returns `width * height * 4`
/// bytes, or an error if the input is too small.
pub fn linear_f16_rgba_to_srgb8(
    packed_f16: &[u8],
    width: u32,
    height: u32,
    rolloff: bool,
) -> Result<Vec<u8>, String> {
    let px = width as usize * height as usize;
    let expected = px * 8;
    if packed_f16.len() < expected {
        return Err(format!(
            "still export: f16 buffer too small ({} bytes, need {expected} for {width}×{height})",
            packed_f16.len(),
        ));
    }

    let mut out = vec![0u8; px * 4];
    for i in 0..px {
        let s = i * 8;
        for ch in 0..3 {
            let bits = u16::from_le_bytes([packed_f16[s + ch * 2], packed_f16[s + ch * 2 + 1]]);
            let mut v = f16_to_f32(bits);
            if rolloff {
                v = highlight_rolloff(v);
            }
            v = linear_to_srgb(v);
            out[i * 4 + ch] = (v * 255.0).round().clamp(0.0, 255.0) as u8;
        }
        // Alpha: linear, no gamma / rolloff.
        let a = f16_to_f32(u16::from_le_bytes([packed_f16[s + 6], packed_f16[s + 7]]));
        out[i * 4 + 3] = (a.clamp(0.0, 1.0) * 255.0).round() as u8;
    }
    Ok(out)
}

/// Output container for a still-frame export.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StillFormat {
    /// Lossless, keeps alpha. Best for transparency / archival.
    Png,
    /// Lossy, opaque RGB at `quality` (1..=100). Smaller files; required by some
    /// platforms for cover-art upload.
    Jpeg { quality: u8 },
}

impl StillFormat {
    /// File extension (no leading dot) for this format.
    pub fn extension(self) -> &'static str {
        match self {
            StillFormat::Png => "png",
            StillFormat::Jpeg { .. } => "jpg",
        }
    }
}

/// Encode tightly-packed RGBA8 pixels (`width * height * 4` bytes, top-down,
/// stride = `width * 4`) to `path` in the requested format. Returns an error
/// string on a too-small buffer, an encode failure, or an I/O failure.
pub fn save_still(
    pixels: &[u8],
    width: u32,
    height: u32,
    path: &Path,
    format: StillFormat,
) -> Result<(), String> {
    let expected = width as usize * height as usize * 4;
    if pixels.len() < expected {
        return Err(format!(
            "still export: pixel buffer too small ({} bytes, need {expected} for {width}×{height})",
            pixels.len(),
        ));
    }
    let pixels = &pixels[..expected];

    let file = std::fs::File::create(path)
        .map_err(|e| format!("still export: cannot create {}: {e}", path.display()))?;
    let writer = BufWriter::new(file);

    match format {
        StillFormat::Png => image::codecs::png::PngEncoder::new(writer)
            .write_image(pixels, width, height, image::ExtendedColorType::Rgba8)
            .map_err(|e| format!("still export: PNG encode failed: {e}")),
        StillFormat::Jpeg { quality } => {
            // JPEG has no alpha — drop it to opaque RGB.
            let mut rgb = Vec::with_capacity(width as usize * height as usize * 3);
            for px in pixels.chunks_exact(4) {
                rgb.extend_from_slice(&px[..3]);
            }
            image::codecs::jpeg::JpegEncoder::new_with_quality(writer, quality.clamp(1, 100))
                .encode(&rgb, width, height, image::ExtendedColorType::Rgb8)
                .map_err(|e| format!("still export: JPEG encode failed: {e}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid_rgba(w: u32, h: u32, c: [u8; 4]) -> Vec<u8> {
        c.iter().copied().cycle().take((w * h * 4) as usize).collect()
    }

    #[test]
    fn png_roundtrips_pixels() {
        let dir = std::env::temp_dir();
        let path = dir.join("manifold_still_test.png");
        let px = solid_rgba(4, 3, [10, 20, 30, 255]);
        save_still(&px, 4, 3, &path, StillFormat::Png).unwrap();

        let decoded = image::open(&path).unwrap().to_rgba8();
        assert_eq!(decoded.dimensions(), (4, 3));
        assert_eq!(decoded.get_pixel(0, 0).0, [10, 20, 30, 255]);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn jpeg_writes_decodable_file() {
        let dir = std::env::temp_dir();
        let path = dir.join("manifold_still_test.jpg");
        let px = solid_rgba(8, 8, [200, 100, 50, 255]);
        save_still(&px, 8, 8, &path, StillFormat::Jpeg { quality: 90 }).unwrap();

        let decoded = image::open(&path).unwrap().to_rgb8();
        assert_eq!(decoded.dimensions(), (8, 8));
        // Lossy — just sanity-check the channel ordering survived.
        let p = decoded.get_pixel(0, 0).0;
        assert!(p[0] > p[1] && p[1] > p[2], "expected R>G>B, got {p:?}");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn rejects_short_buffer() {
        let px = vec![0u8; 10];
        let err = save_still(&px, 4, 4, Path::new("/tmp/nope.png"), StillFormat::Png).unwrap_err();
        assert!(err.contains("too small"), "{err}");
    }

    // f16 bit patterns for exact test values.
    const F16_0: u16 = 0x0000; // 0.0
    const F16_HALF: u16 = 0x3800; // 0.5
    const F16_1: u16 = 0x3C00; // 1.0
    const F16_1_0625: u16 = 0x3C40; // 1.0625 (just above white)
    const F16_2: u16 = 0x4000; // 2.0

    fn px_f16(r: u16, g: u16, b: u16, a: u16) -> Vec<u8> {
        let mut v = Vec::with_capacity(8);
        for c in [r, g, b, a] {
            v.extend_from_slice(&c.to_le_bytes());
        }
        v
    }

    #[test]
    fn srgb_encodes_linear_midgray() {
        // Linear 0.5 → sRGB ≈ 0.735 → ~188. Black stays 0, white stays 255.
        let buf = px_f16(F16_HALF, F16_0, F16_1, F16_1);
        let out = linear_f16_rgba_to_srgb8(&buf, 1, 1, false).unwrap();
        assert!((185..=190).contains(&out[0]), "midgray={}", out[0]);
        assert_eq!(out[1], 0);
        assert_eq!(out[2], 255);
        assert_eq!(out[3], 255); // alpha linear
    }

    fn r(buf: &[u8], rolloff: bool) -> u8 {
        linear_f16_rgba_to_srgb8(buf, 1, 1, rolloff).unwrap()[0]
    }

    #[test]
    fn faithful_clip_saturates_at_and_above_white() {
        // rolloff=false (faithful): 1.0 and everything above clamp to pure white.
        // Over-white detail is lost — the on-screen / screenshot behaviour.
        let white = px_f16(F16_1, F16_1, F16_1, F16_1);
        let over = px_f16(F16_1_0625, F16_1_0625, F16_1_0625, F16_1);
        let bright = px_f16(F16_2, F16_2, F16_2, F16_1);
        assert_eq!(r(&white, false), 255);
        assert_eq!(r(&over, false), 255);
        assert_eq!(r(&bright, false), 255);
    }

    #[test]
    fn rolloff_keeps_overwhite_detail_at_the_cost_of_pure_white() {
        // rolloff=true: pure white drops slightly below 255 so over-white values
        // stay distinguishable (monotonic) instead of all clamping to white.
        let white = px_f16(F16_1, F16_1, F16_1, F16_1);
        let over = px_f16(F16_1_0625, F16_1_0625, F16_1_0625, F16_1);
        let r_white = r(&white, true);
        let r_over = r(&over, true);
        assert!(r_white < 255, "pure white should compress: {r_white}");
        assert!(r_over > r_white, "over-white kept distinct: {r_over} vs {r_white}");
        // Below the knee, rolloff is identity — midtones are untouched.
        let mid = px_f16(F16_HALF, F16_HALF, F16_HALF, F16_1);
        assert_eq!(r(&mid, true), r(&mid, false));
    }

    #[test]
    fn f16_conversion_rejects_short_buffer() {
        let err = linear_f16_rgba_to_srgb8(&[0u8; 4], 1, 1, true).unwrap_err();
        assert!(err.contains("too small"), "{err}");
    }
}
