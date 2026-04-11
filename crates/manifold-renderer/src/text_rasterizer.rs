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
    /// Supports multiline text (lines separated by `\n`).
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

        // Split into lines and shape each one.
        let lines: Vec<&str> = trimmed.split('\n').collect();

        // Measure each line to find max width and per-line metrics.
        struct LineMeasure {
            glyphs: Vec<u16>,
            positions: Vec<CGPoint>,
        }
        let mut line_measures: Vec<LineMeasure> = Vec::with_capacity(lines.len());
        let mut max_width: f32 = 0.0;

        // Get font-level metrics (consistent across lines).
        let sample_line = self.make_ct_line(&ct_font, "Hg");
        let sample_bounds = sample_line.get_typographic_bounds();
        let ascent = sample_bounds.ascent as f32;
        let descent = sample_bounds.descent.abs() as f32;
        let line_height = ascent + descent;

        for line_text in &lines {
            let line_text = line_text.trim_end();
            if line_text.is_empty() {
                line_measures.push(LineMeasure {
                    glyphs: Vec::new(),
                    positions: Vec::new(),
                });
                continue;
            }
            let ct_line = self.make_ct_line(&ct_font, line_text);
            let bounds = ct_line.get_typographic_bounds();
            let w = bounds.width as f32;
            max_width = max_width.max(w);

            if let Some((glyphs, positions)) = self.shape_line(&ct_font, line_text) {
                line_measures.push(LineMeasure { glyphs, positions });
            } else {
                line_measures.push(LineMeasure {
                    glyphs: Vec::new(),
                    positions: Vec::new(),
                });
            }
        }

        if line_measures.iter().all(|m| m.glyphs.is_empty()) {
            return None;
        }

        let num_lines = lines.len() as f32;
        let bitmap_w =
            (max_width.ceil() as u32 + PADDING * 2).min(MAX_BITMAP_DIM);
        let bitmap_h =
            ((line_height * num_lines).ceil() as u32 + PADDING * 2).min(MAX_BITMAP_DIM);

        if bitmap_w == 0 || bitmap_h == 0 {
            return None;
        }

        let w = bitmap_w as usize;
        let h = bitmap_h as usize;
        let mut pixels = vec![0u8; w * h];

        let color_space = CGColorSpace::create_device_gray();
        let ctx = CGContext::create_bitmap_context(
            Some(pixels.as_mut_ptr() as *mut std::ffi::c_void),
            w,
            h,
            8,
            w,
            &color_space,
            0u32,
        );

        ctx.set_rgb_fill_color(1.0, 1.0, 1.0, 1.0);
        ctx.set_allows_font_smoothing(false);
        ctx.set_should_smooth_fonts(false);

        let origin_x = PADDING as f64;

        // CG is y-up: line 0 (top visually) has the highest CG y.
        // Bottom of bitmap = CG y 0. Top of bitmap = CG y (h-1).
        for (line_idx, measure) in line_measures.iter().enumerate() {
            if measure.glyphs.is_empty() {
                continue;
            }
            // Lines from top: line 0 is at top of bitmap.
            // In CG coords, line 0 baseline = bitmap_h - PADDING - ascent
            // Each subsequent line shifts down by line_height.
            let baseline_y = (bitmap_h as f32 - PADDING as f32 - ascent
                - line_idx as f32 * line_height) as f64;

            let draw_positions: Vec<CGPoint> = measure
                .positions
                .iter()
                .map(|p| CGPoint::new(p.x + origin_x, p.y + baseline_y))
                .collect();

            ct_font.draw_glyphs(&measure.glyphs, &draw_positions, ctx.clone());
        }

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
