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
    context::{CGContext, CGLineCap, CGLineJoin, CGTextDrawingMode},
    data_provider::CGDataProvider,
    font::CGFont,
    geometry::CGPoint,
};
use core_text::{font::CTFont, string_attributes::kCTFontAttributeName};
use std::sync::Arc;

// Apple-silicon Metal supports 16384² 2D textures. We rasterize glyphs at
// their full on-screen footprint, so the cap only bites for absurdly large
// text — and when it does, `rasterize` scales the layout down to fit rather
// than cropping it (the compositor scales it back up, linear-filtered).
const MAX_BITMAP_DIM: u32 = 16384;
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
    /// Outline stroke width as a fraction of font_size. 0.0 = no stroke.
    /// When > 0, a second coverage mask is rendered from the stroked glyph
    /// outlines so the compositor can colour the outline separately.
    pub stroke_width: f32,
}

impl<'a> Default for RasterizeOptions<'a> {
    fn default() -> Self {
        Self {
            font_family: None,
            h_align: HAlign::Center,
            letter_spacing: 0.0,
            line_spacing: 1.2,
            stroke_width: 0.0,
        }
    }
}

/// Result of rasterizing a text string. `fill` is the glyph interior
/// coverage; `stroke` is the outline coverage, present only when
/// `RasterizeOptions::stroke_width > 0`. Both are R8 grayscale, same dims.
pub struct RasterizedText {
    /// R8 grayscale fill (interior) coverage (width * height bytes).
    pub fill: Vec<u8>,
    /// R8 grayscale outline coverage, or `None` when no stroke was requested.
    pub stroke: Option<Vec<u8>>,
    pub width: u32,
    pub height: u32,
    /// Font size the bitmap was actually rendered at. Equals the requested
    /// `font_size` unless the layout had to be scaled down to fit
    /// `MAX_BITMAP_DIM`; the caller divides requested / rendered to get the
    /// extra magnification the compositor must apply (linear-filtered).
    pub rendered_font_px: f32,
}

/// One CoreText glyph run: a contiguous span of glyph ids that all share the
/// same resolved font. A line is usually one run, but CoreText splits a
/// second run whenever it falls back to a different font for a character the
/// requested `ct_font` doesn't cover (e.g. a symbol outside Inter's
/// coverage) — `font` here is THAT run's own resolved font, not the line's,
/// so drawing stays correct across the split (BUG-107: drawing every run
/// with one shared base font maps a fallback run's glyph ids onto arbitrary
/// glyphs in the base font — mojibake).
struct GlyphRun {
    font: CTFont,
    glyphs: Vec<u16>,
    positions: Vec<CGPoint>,
}

/// One shaped line: its glyph runs (see [`GlyphRun`]) and the line width.
struct LineMeasure {
    runs: Vec<GlyphRun>,
    width: f32,
}

/// Output of shaping a whole string at one font size: the per-line glyph
/// runs (each carrying its own resolved font, BUG-107) and the metrics
/// needed to size and place the bitmap.
struct ShapeResult {
    line_measures: Vec<LineMeasure>,
    max_width: f32,
    ascent: f32,
    content_h: f32,
    line_height: f32,
}

/// Bitmap dimensions and stroke geometry derived from a [`ShapeResult`] at a
/// given font size. `over` > 1 means the content exceeds `MAX_BITMAP_DIM` and
/// the layout must be reshaped at `font_size / over` to fit.
struct BitmapDims {
    pad: u32,
    stroke_px: f32,
    want_stroke: bool,
    bitmap_w: u32,
    bitmap_h: u32,
    over: f32,
}

impl BitmapDims {
    fn compute(shaped: &ShapeResult, font_size: f32, opts: &RasterizeOptions) -> Self {
        // A stroke straddles the glyph edge — half its width spills outward.
        // Pad so the outline isn't clipped. No stroke → padding stays at
        // PADDING so the fill-only bitmap is byte-identical to before.
        let stroke_px = (opts.stroke_width * font_size).max(0.0);
        let want_stroke = stroke_px > 0.5;
        let pad = if want_stroke {
            PADDING + (stroke_px * 0.5).ceil() as u32
        } else {
            PADDING
        };
        let needed_w = shaped.max_width.ceil() + (pad * 2) as f32;
        let needed_h = shaped.content_h.ceil() + (pad * 2) as f32;
        let over = needed_w.max(needed_h) / MAX_BITMAP_DIM as f32;
        let bitmap_w = (needed_w as u32).min(MAX_BITMAP_DIM);
        let bitmap_h = (needed_h as u32).min(MAX_BITMAP_DIM);
        Self {
            pad,
            stroke_px,
            want_stroke,
            bitmap_w,
            bitmap_h,
            over,
        }
    }
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
        let cg_font =
            CGFont::from_data_provider(provider).expect("Failed to create CGFont from Inter TTF");

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

        let lines: Vec<&str> = trimmed.split('\n').collect();

        // Shape at the requested size. If the resulting bitmap would exceed the
        // texture cap, reshape once at a reduced size so the glyphs scale to
        // fit instead of being cropped — the compositor scales the bitmap back
        // up (linear-filtered) so the on-screen size still matches.
        let mut eff_font = font_size.max(1.0);
        let mut shaped = self.shape_at(&lines, eff_font, opts)?;
        let mut dims = BitmapDims::compute(&shaped, eff_font, opts);
        if dims.over > 1.0 {
            eff_font = (eff_font / dims.over).max(1.0);
            shaped = self.shape_at(&lines, eff_font, opts)?;
            dims = BitmapDims::compute(&shaped, eff_font, opts);
        }

        let BitmapDims {
            pad,
            stroke_px,
            want_stroke,
            bitmap_w,
            bitmap_h,
            ..
        } = dims;

        if bitmap_w == 0 || bitmap_h == 0 {
            return None;
        }

        // Local handles so the render_pass closure below reads exactly as it
        // did before — only their source (the ShapeResult) changed. `ct_font`
        // itself is no longer read here: each run now draws with its OWN
        // resolved font (BUG-107) rather than this shared line-level font.
        let line_measures = &shaped.line_measures;
        let max_width = shaped.max_width;
        let ascent = shaped.ascent;
        let line_height = shaped.line_height;

        let w = bitmap_w as usize;
        let h = bitmap_h as usize;

        // Render one coverage pass into a fresh R8 buffer. `stroke = None`
        // draws filled glyphs (interior coverage); `Some(width_px)` strokes
        // the glyph outlines at that line width. Geometry is identical across
        // passes, so the fill and stroke masks register exactly.
        let render_pass = |stroke: Option<f32>| -> Vec<u8> {
            let mut buf = vec![0u8; w * h];
            let color_space = CGColorSpace::create_device_gray();
            let ctx = CGContext::create_bitmap_context(
                Some(buf.as_mut_ptr() as *mut std::ffi::c_void),
                w,
                h,
                8,
                w,
                &color_space,
                0u32,
            );
            ctx.set_allows_font_smoothing(false);
            ctx.set_should_smooth_fonts(false);
            match stroke {
                None => {
                    ctx.set_rgb_fill_color(1.0, 1.0, 1.0, 1.0);
                    ctx.set_text_drawing_mode(CGTextDrawingMode::CGTextFill);
                }
                Some(width_px) => {
                    ctx.set_rgb_stroke_color(1.0, 1.0, 1.0, 1.0);
                    ctx.set_line_width(width_px as f64);
                    ctx.set_line_join(CGLineJoin::CGLineJoinRound);
                    ctx.set_line_cap(CGLineCap::CGLineCapRound);
                    ctx.set_text_drawing_mode(CGTextDrawingMode::CGTextStroke);
                }
            }

            // CG is y-up: line 0 (top visually) has the highest CG y.
            for (line_idx, measure) in line_measures.iter().enumerate() {
                if measure.runs.is_empty() {
                    continue;
                }

                // Horizontal alignment offset
                let align_offset = match opts.h_align {
                    HAlign::Left => 0.0,
                    HAlign::Center => ((max_width - measure.width) * 0.5).max(0.0),
                    HAlign::Right => (max_width - measure.width).max(0.0),
                };
                let origin_x = pad as f64 + align_offset as f64;

                // Lines from top: line 0 is at top of bitmap.
                // In CG coords, line 0 baseline = bitmap_h - pad - ascent
                // Each subsequent line shifts down by line_height.
                let baseline_y =
                    (bitmap_h as f32 - pad as f32 - ascent - line_idx as f32 * line_height) as f64;

                // Draw each run with its OWN resolved font (BUG-107) — a run
                // whose font differs from the line-level `ct_font` is a
                // CoreText fallback split, and its glyph ids only resolve
                // correctly against that run's own font.
                for run in &measure.runs {
                    let draw_positions: Vec<CGPoint> = run
                        .positions
                        .iter()
                        .map(|p| CGPoint::new(p.x + origin_x, p.y + baseline_y))
                        .collect();
                    run.font.draw_glyphs(&run.glyphs, &draw_positions, ctx.clone());
                }
            }

            buf
        };

        let fill = render_pass(None);
        let stroke = if want_stroke {
            Some(render_pass(Some(stroke_px)))
        } else {
            None
        };

        Some(RasterizedText {
            fill,
            stroke,
            width: bitmap_w,
            height: bitmap_h,
            rendered_font_px: eff_font,
        })
    }

    /// Resolve the font and shape every line at `font_size`, returning the
    /// glyph runs plus the metrics needed to size and place the bitmap.
    /// Returns `None` when no line produced any glyphs.
    fn shape_at(
        &mut self,
        lines: &[&str],
        font_size: f32,
        opts: &RasterizeOptions,
    ) -> Option<ShapeResult> {
        // Use system font by family name if provided, fall back to embedded
        // Inter. Cache the resolved CTFont to avoid repeated name lookups and
        // ensure the font is fully loaded before the first glyph draw.
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

        // Font-level metrics (consistent across lines).
        let sample_line = self.make_ct_line(&ct_font, "Hg");
        let sample_bounds = sample_line.get_typographic_bounds();
        let ascent = sample_bounds.ascent as f32;
        let descent = sample_bounds.descent.abs() as f32;
        let base_line_height = ascent + descent;
        let line_height = base_line_height * opts.line_spacing;

        let mut line_measures: Vec<LineMeasure> = Vec::with_capacity(lines.len());
        let mut max_width: f32 = 0.0;

        for line_text in lines {
            let line_text = line_text.trim_end();
            if line_text.is_empty() {
                line_measures.push(LineMeasure {
                    runs: Vec::new(),
                    width: 0.0,
                });
                continue;
            }

            if let Some(mut runs) = self.shape_line(&ct_font, line_text) {
                // Apply letter spacing: shift each glyph by its position in
                // the WHOLE line (not per-run) — a fallback-font run midway
                // through the line still spaces continuously with its
                // neighbours.
                let mut glyph_count = 0usize;
                if letter_spacing_px.abs() > 0.001 {
                    for run in runs.iter_mut() {
                        for pos in run.positions.iter_mut() {
                            pos.x += glyph_count as f64 * letter_spacing_px as f64;
                            glyph_count += 1;
                        }
                    }
                } else {
                    glyph_count = runs.iter().map(|r| r.glyphs.len()).sum();
                }
                // Compute line width from last glyph position + font metrics
                let ct_line = self.make_ct_line(&ct_font, line_text);
                let bounds = ct_line.get_typographic_bounds();
                let w = bounds.width as f32
                    + (glyph_count.saturating_sub(1)) as f32 * letter_spacing_px;
                max_width = max_width.max(w);
                line_measures.push(LineMeasure { runs, width: w });
            } else {
                line_measures.push(LineMeasure {
                    runs: Vec::new(),
                    width: 0.0,
                });
            }
        }

        if line_measures.iter().all(|m| m.runs.is_empty()) {
            return None;
        }

        // Total height: first line = base_line_height, each additional line
        // adds line_height. This way single-line text isn't padded by
        // line_spacing.
        let content_h = base_line_height + (lines.len().saturating_sub(1)) as f32 * line_height;

        Some(ShapeResult {
            line_measures,
            max_width,
            ascent,
            content_h,
            line_height,
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
            .unwrap_or_else(|()| core_text::font::new_from_CGFont(&self.cg_font, size));
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

    /// Shape text into its CoreText glyph runs (BUG-107: kept as separate
    /// runs, never flattened into one glyph-id list, because each run may
    /// carry its OWN resolved font — CoreText splits a run whenever it falls
    /// back from `ct_font` to cover a character `ct_font` lacks, and that
    /// run's glyph ids only resolve correctly against ITS font).
    fn shape_line(&self, ct_font: &CTFont, text: &str) -> Option<Vec<GlyphRun>> {
        let line = self.make_ct_line(ct_font, text);
        let runs = line.glyph_runs();

        let mut result: Vec<GlyphRun> = Vec::new();
        for run in runs.iter() {
            let count = run.glyph_count() as usize;
            if count == 0 {
                continue;
            }
            // The font CoreText actually shaped this run with — `kCTFontAttributeName`
            // on the run's own attribute dictionary, which differs from `ct_font`
            // exactly when this run is a fallback split. Falls back to `ct_font`
            // itself if the attribute is somehow absent, so a run always has a
            // font to draw with.
            let run_font: CTFont = run
                .attributes()
                .and_then(|attrs| {
                    // Safety: kCTFontAttributeName is a valid CoreText dictionary
                    // key (a static CFStringRef); `find` only reads it.
                    let key = unsafe { kCTFontAttributeName };
                    attrs.find(key).and_then(|v| v.downcast::<CTFont>())
                })
                .unwrap_or_else(|| ct_font.clone());
            let run_glyphs = run.glyphs().into_owned();
            let run_positions = run.positions().into_owned();
            result.push(GlyphRun {
                font: run_font,
                glyphs: run_glyphs,
                positions: run_positions,
            });
        }

        if result.is_empty() { None } else { Some(result) }
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
        assert_eq!(rt.fill.len(), (rt.width * rt.height) as usize);
        assert!(rt.fill.iter().any(|&p| p > 0));
        assert!(rt.stroke.is_none());
    }

    #[test]
    fn test_rasterize_stroke_produces_outline_mask() {
        let mut rasterizer = TextRasterizer::new();
        let opts = RasterizeOptions {
            stroke_width: 0.08,
            ..Default::default()
        };
        let rt = rasterizer.rasterize("O", 64.0, &opts).unwrap();
        let stroke = rt.stroke.expect("stroke mask present when stroke_width > 0");
        assert_eq!(stroke.len(), (rt.width * rt.height) as usize);
        assert!(stroke.iter().any(|&p| p > 0), "stroke mask should be lit");
    }

    #[test]
    fn test_rasterize_caps_oversized_to_fit() {
        let mut rasterizer = TextRasterizer::new();
        // A font size large enough that this string would blow past the
        // texture cap — the layout must scale down to fit, not crop.
        let rt = rasterizer
            .rasterize("WIDETEXTWIDETEXT", 24000.0, &RasterizeOptions::default())
            .unwrap();
        assert!(
            rt.width <= MAX_BITMAP_DIM && rt.height <= MAX_BITMAP_DIM,
            "bitmap {}x{} must fit within the cap",
            rt.width,
            rt.height
        );
        assert!(
            rt.rendered_font_px < 24000.0,
            "font should scale down to fit, got {}",
            rt.rendered_font_px
        );
        assert!(rt.fill.iter().any(|&p| p > 0), "scaled-down text still lit");
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
            .rasterize(
                "A\nB",
                64.0,
                &RasterizeOptions {
                    line_spacing: 1.0,
                    ..Default::default()
                },
            )
            .unwrap();
        let loose = rasterizer
            .rasterize(
                "A\nB",
                64.0,
                &RasterizeOptions {
                    line_spacing: 2.0,
                    ..Default::default()
                },
            )
            .unwrap();
        assert!(loose.height > tight.height);
    }

    #[test]
    fn test_h_align_left_vs_right() {
        let mut rasterizer = TextRasterizer::new();
        // Two lines of different widths — alignment should shift the shorter one
        let left = rasterizer
            .rasterize(
                "AAAA\nB",
                64.0,
                &RasterizeOptions {
                    h_align: HAlign::Left,
                    ..Default::default()
                },
            )
            .unwrap();
        let right = rasterizer
            .rasterize(
                "AAAA\nB",
                64.0,
                &RasterizeOptions {
                    h_align: HAlign::Right,
                    ..Default::default()
                },
            )
            .unwrap();
        // Bitmaps should be the same size (max_width determines width)
        assert_eq!(left.width, right.width);
        // But the pixel content should differ (B is in different position)
        assert_ne!(left.fill, right.fill);
    }
}
