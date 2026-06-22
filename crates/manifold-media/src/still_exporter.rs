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

use std::io::BufWriter;
use std::path::Path;

use image::ImageEncoder;

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
}
