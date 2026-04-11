//! Standalone text rasterizer using CoreText/CoreGraphics.
//!
//! Shapes a string via CoreText and rasterizes it into an R8 grayscale pixel
//! buffer. Used by the Text generator to produce a CPU-side bitmap that gets
//! uploaded to a GPU texture. Deliberately separate from `native_text.rs`
//! (which is the UI-thread glyph atlas system).

use core_foundation::{
    attributed_string::CFMutableAttributedString,
    base::{CFRange, TCFType},
    string::CFString,
};
use core_graphics::{
    color_space::CGColorSpace,
    context::CGContext,
    data_provider::CGDataProvider,
    font::CGFont,
    geometry::CGPoint,
};
use core_text::{
    font::CTFont,
    string_attributes::kCTFontAttributeName,
};
use std::sync::Arc;

const MAX_BITMAP_DIM: u32 = 4096;
const PADDING: u32 = 4;

/// Result of rasterizing a text string.
pub struct RasterizedText {
    /// R8 grayscale pixel data (width * height bytes).
    pub pixels: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// Rasterizes text strings into R8 grayscale bitmaps via CoreText.
pub struct TextRasterizer {
    cg_font: CGFont,
}

// Safety: CGFont is a Core Foundation type with thread-safe reference counting.
// TextRasterizer is only accessed from a single content thread (Generator: Send).
unsafe impl Send for TextRasterizer {}

impl Default for TextRasterizer {
    fn default() -> Self {
        Self::new()
    }
}

impl TextRasterizer {
    pub fn new() -> Self {
        let ttf_bytes: &'static [u8] = include_bytes!("../assets/fonts/Inter-Regular.ttf");
        let data: Vec<u8> = ttf_bytes.to_vec();
        let provider = CGDataProvider::from_buffer(Arc::new(data));
        let cg_font = CGFont::from_data_provider(provider)
            .expect("Failed to create CGFont from Inter TTF");
        Self { cg_font }
    }

    /// Enumerate all installed font family names, sorted alphabetically.
    pub fn available_font_families() -> Vec<String> {
        let cf_names = core_text::font_manager::copy_available_font_family_names();
        let mut names: Vec<String> = cf_names.iter().map(|n| n.to_string()).collect();
        names.sort_unstable_by_key(|a| a.to_lowercase());
        names
    }

    /// Rasterize a text string at the given font size into an R8 grayscale bitmap.
    /// If `font_family` is provided, attempts to use that system font; falls back
    /// to the embedded Inter font if not found.
    /// Returns `None` for empty or whitespace-only strings.
    pub fn rasterize(
        &self,
        text: &str,
        font_size: f32,
        font_family: Option<&str>,
    ) -> Option<RasterizedText> {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return None;
        }

        // Use system font by family name if provided, fall back to embedded Inter.
        let ct_font = if let Some(family) = font_family {
            core_text::font::new_from_name(family, font_size as f64)
                .unwrap_or_else(|()| {
                    core_text::font::new_from_CGFont(&self.cg_font, font_size as f64)
                })
        } else {
            core_text::font::new_from_CGFont(&self.cg_font, font_size as f64)
        };

        // Shape text to get glyph IDs and positions
        let (glyph_ids, positions) = self.shape_line(&ct_font, trimmed)?;

        // Get typographic bounds for the full line
        let line = self.make_ct_line(&ct_font, trimmed);
        let bounds = line.get_typographic_bounds();

        let ascent = bounds.ascent as f32;
        let descent = bounds.descent.abs() as f32; // descent is negative in CoreText
        let bitmap_w = ((bounds.width as f32).ceil() as u32 + PADDING * 2).min(MAX_BITMAP_DIM);
        let bitmap_h = ((ascent + descent).ceil() as u32 + PADDING * 2).min(MAX_BITMAP_DIM);

        if bitmap_w == 0 || bitmap_h == 0 {
            return None;
        }

        // Rasterize into an 8-bit grayscale bitmap
        let w = bitmap_w as usize;
        let h = bitmap_h as usize;
        let mut pixels = vec![0u8; w * h];

        let color_space = CGColorSpace::create_device_gray();
        let ctx = CGContext::create_bitmap_context(
            Some(pixels.as_mut_ptr() as *mut std::ffi::c_void),
            w,
            h,
            8,  // bits per component
            w,  // bytes per row (1 byte per pixel, R8)
            &color_space,
            0u32, // kCGImageAlphaNone
        );

        ctx.set_rgb_fill_color(1.0, 1.0, 1.0, 1.0);
        ctx.set_allows_font_smoothing(false);
        ctx.set_should_smooth_fonts(false);

        // CG y-up: y=0 at bottom. Place baseline so descent fits below.
        let baseline_y = (descent + PADDING as f32) as f64;
        let origin_x = PADDING as f64;

        // Offset all glyph positions by the baseline origin
        let draw_positions: Vec<CGPoint> = positions
            .iter()
            .map(|p| CGPoint::new(p.x + origin_x, p.y + baseline_y))
            .collect();

        ct_font.draw_glyphs(&glyph_ids, &draw_positions, ctx);

        Some(RasterizedText {
            pixels,
            width: bitmap_w,
            height: bitmap_h,
        })
    }

    /// Create a CTLine from a text string and a CTFont.
    fn make_ct_line(&self, ct_font: &CTFont, text: &str) -> core_text::line::CTLine {
        let cf_text = CFString::new(text);
        let mut attr_str = CFMutableAttributedString::new();
        attr_str.replace_str(&cf_text, CFRange::init(0, 0));
        let range = CFRange::init(0, cf_text.char_len());
        unsafe {
            attr_str.set_attribute(range, kCTFontAttributeName, ct_font);
        }
        core_text::line::CTLine::new_with_attributed_string(attr_str.as_concrete_TypeRef())
    }

    /// Shape text into (glyph_ids, positions_in_line).
    fn shape_line(&self, ct_font: &CTFont, text: &str) -> Option<(Vec<u16>, Vec<CGPoint>)> {
        let line = self.make_ct_line(ct_font, text);
        let runs = line.glyph_runs();

        let mut glyph_ids: Vec<u16> = Vec::new();
        let mut positions: Vec<CGPoint> = Vec::new();

        for run in runs.iter() {
            let count = run.glyph_count() as usize;
            if count == 0 {
                continue;
            }
            let run_glyphs = run.glyphs();
            let run_positions = run.positions();
            glyph_ids.extend_from_slice(&run_glyphs);
            positions.extend_from_slice(&run_positions);
        }

        if glyph_ids.is_empty() {
            None
        } else {
            Some((glyph_ids, positions))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rasterize_hello() {
        let rasterizer = TextRasterizer::new();
        let result = rasterizer.rasterize("HELLO", 64.0, None);
        assert!(result.is_some());
        let rt = result.unwrap();
        assert!(rt.width > 0);
        assert!(rt.height > 0);
        assert_eq!(rt.pixels.len(), (rt.width * rt.height) as usize);
        assert!(rt.pixels.iter().any(|&p| p > 0));
    }

    #[test]
    fn test_rasterize_empty_returns_none() {
        let rasterizer = TextRasterizer::new();
        assert!(rasterizer.rasterize("", 64.0, None).is_none());
        assert!(rasterizer.rasterize("   ", 64.0, None).is_none());
    }

    #[test]
    fn test_rasterize_with_system_font() {
        let rasterizer = TextRasterizer::new();
        // Helvetica is always available on macOS
        let result = rasterizer.rasterize("TEST", 64.0, Some("Helvetica"));
        assert!(result.is_some());
    }

    #[test]
    fn test_rasterize_unknown_font_falls_back() {
        let rasterizer = TextRasterizer::new();
        let result = rasterizer.rasterize("TEST", 64.0, Some("NonExistentFont12345"));
        assert!(result.is_some()); // Falls back to Inter
    }

    #[test]
    fn test_available_font_families() {
        let families = TextRasterizer::available_font_families();
        assert!(!families.is_empty());
        // Helvetica should always be present on macOS
        assert!(families.iter().any(|f| f == "Helvetica"));
    }
}
