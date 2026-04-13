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

/// Horizontal text alignment within the rasterized bitmap.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum HAlign {
    Left,
    Center,
    Right,
}

impl HAlign {
    pub fn from_param(v: f32) -> Self {
        match v.round() as i32 {
            0 => Self::Left,
            2 => Self::Right,
            _ => Self::Center,
        }
    }
}

/// Styling options for text rasterization.
#[derive(Debug, Clone)]
pub struct RasterizeOptions<'a> {
    pub font_family: Option<&'a str>,
    pub h_align: HAlign,
    /// Extra spacing between glyphs as a fraction of font_size.
    /// 0.0 = default, positive = wider, negative = tighter.
    pub letter_spacing: f32,
    /// Line height multiplier. 1.0 = tight (ascent+descent only), 1.2 = default.
    pub line_spacing: f32,
}

impl<'a> Default for RasterizeOptions<'a> {
    fn default() -> Self {
        Self {
            font_family: None,
            h_align: HAlign::Center,
            letter_spacing: 0.0,
            line_spacing: 1.2,
        }
    }
}

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
    /// Cached CTFont for the currently selected font family.
    /// Avoids re-resolving the font name on every rasterize call and ensures the
    /// font is pre-warmed before the first render frame.
    cached_ct_font: Option<(String, f64, CTFont)>, // (family, size, font)
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

        // Prime the CoreText font database so that `new_from_name` resolves
        // system fonts without delay on the first render frame.
        let _ = core_text::font_manager::copy_available_font_family_names();

        Self {
            cg_font,
            cached_ct_font: None,
        }
    }

    /// Enumerate all installed font family names, sorted alphabetically.
    pub fn available_font_families() -> Vec<String> {
        let cf_names = core_text::font_manager::copy_available_font_family_names();
        let mut names: Vec<String> = cf_names.iter().map(|n| n.to_string()).collect();
        names.sort_unstable_by_key(|a| a.to_lowercase());
        names
    }

    /// Rasterize a text string at the given font size into an R8 grayscale bitmap.
    /// Supports multiline text (lines separated by `\n`).
    /// Returns `None` for empty or whitespace-only strings.
    pub fn rasterize(
        &mut self,
        text: &str,
        font_size: f32,
        opts: &RasterizeOptions,
    ) -> Option<RasterizedText> {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return None;
        }

        // Use system font by family name if provided, fall back to embedded Inter.
        // Cache the resolved CTFont to avoid repeated name lookups and ensure
        // the font is fully loaded before the first glyph draw.
        let ct_font = if let Some(family) = opts.font_family {
            if let Some((ref cached_fam, cached_size, ref cached_font)) = self.cached_ct_font {
                if cached_fam == family && (cached_size - font_size as f64).abs() < 0.5 {
                    cached_font.clone()
                } else {
                    self.resolve_and_cache_font(family, font_size as f64)
                }
            } else {
                self.resolve_and_cache_font(family, font_size as f64)
            }
        } else {
            core_text::font::new_from_CGFont(&self.cg_font, font_size as f64)
        };

        let letter_spacing_px = opts.letter_spacing * font_size;

        // Split into lines and shape each one.
        let lines: Vec<&str> = trimmed.split('\n').collect();

        struct LineMeasure {
            glyphs: Vec<u16>,
            positions: Vec<CGPoint>,
            width: f32,
        }
        let mut line_measures: Vec<LineMeasure> = Vec::with_capacity(lines.len());
        let mut max_width: f32 = 0.0;

        // Get font-level metrics (consistent across lines).
        let sample_line = self.make_ct_line(&ct_font, "Hg");
        let sample_bounds = sample_line.get_typographic_bounds();
        let ascent = sample_bounds.ascent as f32;
        let descent = sample_bounds.descent.abs() as f32;
        let base_line_height = ascent + descent;
        let line_height = base_line_height * opts.line_spacing;

        for line_text in &lines {
            let line_text = line_text.trim_end();
            if line_text.is_empty() {
                line_measures.push(LineMeasure {
                    glyphs: Vec::new(),
                    positions: Vec::new(),
                    width: 0.0,
                });
                continue;
            }

            if let Some((glyphs, mut positions)) = self.shape_line(&ct_font, line_text) {
                // Apply letter spacing: shift each glyph by index * spacing
                if letter_spacing_px.abs() > 0.001 {
                    for (i, pos) in positions.iter_mut().enumerate() {
                        pos.x += i as f64 * letter_spacing_px as f64;
                    }
                }
                // Compute line width from last glyph position + font metrics
                let ct_line = self.make_ct_line(&ct_font, line_text);
                let bounds = ct_line.get_typographic_bounds();
                let w = bounds.width as f32
                    + (glyphs.len().saturating_sub(1)) as f32 * letter_spacing_px;
                max_width = max_width.max(w);
                line_measures.push(LineMeasure { glyphs, positions, width: w });
            } else {
                line_measures.push(LineMeasure {
                    glyphs: Vec::new(),
                    positions: Vec::new(),
                    width: 0.0,
                });
            }
        }

        if line_measures.iter().all(|m| m.glyphs.is_empty()) {
            return None;
        }

        let num_lines = lines.len();
        // Total height: first line = base_line_height, each additional line adds
        // line_height. This way single-line text isn't padded by line_spacing.
        let content_h = base_line_height + (num_lines.saturating_sub(1)) as f32 * line_height;
        let bitmap_w =
            (max_width.ceil() as u32 + PADDING * 2).min(MAX_BITMAP_DIM);
        let bitmap_h =
            (content_h.ceil() as u32 + PADDING * 2).min(MAX_BITMAP_DIM);

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

        // CG is y-up: line 0 (top visually) has the highest CG y.
        for (line_idx, measure) in line_measures.iter().enumerate() {
            if measure.glyphs.is_empty() {
                continue;
            }

            // Horizontal alignment offset
            let align_offset = match opts.h_align {
                HAlign::Left => 0.0,
                HAlign::Center => ((max_width - measure.width) * 0.5).max(0.0),
                HAlign::Right => (max_width - measure.width).max(0.0),
            };
            let origin_x = PADDING as f64 + align_offset as f64;

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

    /// Pre-warm a font by family name so it's cached before the first render.
    /// Called from `set_string_params` when the font family changes.
    pub fn prewarm_font(&mut self, family: &str) {
        if family.is_empty() {
            return;
        }
        // Only prewarm if not already cached for this family.
        if let Some((ref cached_fam, _, _)) = self.cached_ct_font
            && cached_fam == family
        {
            return;
        }
        // Resolve at a reference size — the actual size will re-resolve if needed,
        // but this ensures CoreText has the font descriptor ready.
        let _ = self.resolve_and_cache_font(family, 64.0);
    }

    /// Resolve a font by family name, cache it, and return it.
    /// Falls back to embedded Inter if the name doesn't resolve.
    fn resolve_and_cache_font(&mut self, family: &str, size: f64) -> CTFont {
        let font = core_text::font::new_from_name(family, size)
            .unwrap_or_else(|()| {
                core_text::font::new_from_CGFont(&self.cg_font, size)
            });
        self.cached_ct_font = Some((family.to_string(), size, font.clone()));
        font
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
        let mut rasterizer = TextRasterizer::new();
        let result = rasterizer.rasterize("HELLO", 64.0, &RasterizeOptions::default());
        assert!(result.is_some());
        let rt = result.unwrap();
        assert!(rt.width > 0);
        assert!(rt.height > 0);
        assert_eq!(rt.pixels.len(), (rt.width * rt.height) as usize);
        assert!(rt.pixels.iter().any(|&p| p > 0));
    }

    #[test]
    fn test_rasterize_empty_returns_none() {
        let mut rasterizer = TextRasterizer::new();
        let opts = RasterizeOptions::default();
        assert!(rasterizer.rasterize("", 64.0, &opts).is_none());
        assert!(rasterizer.rasterize("   ", 64.0, &opts).is_none());
    }

    #[test]
    fn test_rasterize_with_system_font() {
        let mut rasterizer = TextRasterizer::new();
        let opts = RasterizeOptions {
            font_family: Some("Helvetica"),
            ..Default::default()
        };
        let result = rasterizer.rasterize("TEST", 64.0, &opts);
        assert!(result.is_some());
    }

    #[test]
    fn test_rasterize_unknown_font_falls_back() {
        let mut rasterizer = TextRasterizer::new();
        let opts = RasterizeOptions {
            font_family: Some("NonExistentFont12345"),
            ..Default::default()
        };
        let result = rasterizer.rasterize("TEST", 64.0, &opts);
        assert!(result.is_some()); // Falls back to Inter
    }

    #[test]
    fn test_available_font_families() {
        let families = TextRasterizer::available_font_families();
        assert!(!families.is_empty());
        // Helvetica should always be present on macOS
        assert!(families.iter().any(|f| f == "Helvetica"));
    }

    #[test]
    fn test_letter_spacing_wider() {
        let mut rasterizer = TextRasterizer::new();
        let narrow = rasterizer
            .rasterize("AB", 64.0, &RasterizeOptions::default())
            .unwrap();
        let wide = rasterizer
            .rasterize(
                "AB",
                64.0,
                &RasterizeOptions {
                    letter_spacing: 1.0,
                    ..Default::default()
                },
            )
            .unwrap();
        assert!(wide.width > narrow.width);
    }

    #[test]
    fn test_line_spacing_taller() {
        let mut rasterizer = TextRasterizer::new();
        let tight = rasterizer
            .rasterize("A\nB", 64.0, &RasterizeOptions {
                line_spacing: 1.0,
                ..Default::default()
            })
            .unwrap();
        let loose = rasterizer
            .rasterize("A\nB", 64.0, &RasterizeOptions {
                line_spacing: 2.0,
                ..Default::default()
            })
            .unwrap();
        assert!(loose.height > tight.height);
    }

    #[test]
    fn test_h_align_left_vs_right() {
        let mut rasterizer = TextRasterizer::new();
        // Two lines of different widths — alignment should shift the shorter one
        let left = rasterizer
            .rasterize("AAAA\nB", 64.0, &RasterizeOptions {
                h_align: HAlign::Left,
                ..Default::default()
            })
            .unwrap();
        let right = rasterizer
            .rasterize("AAAA\nB", 64.0, &RasterizeOptions {
                h_align: HAlign::Right,
                ..Default::default()
            })
            .unwrap();
        // Bitmaps should be the same size (max_width determines width)
        assert_eq!(left.width, right.width);
        // But the pixel content should differ (B is in different position)
        assert_ne!(left.pixels, right.pixels);
    }
}
