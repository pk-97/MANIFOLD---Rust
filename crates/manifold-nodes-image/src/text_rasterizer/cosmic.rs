//! The rasterizer off the Mac: shapes a string with `manifold-text`
//! (cosmic-text shaping, swash raster) and rasterizes it into an R8 grayscale
//! pixel buffer. Same layout, sizing and masks as the CoreText rasterizer.

use manifold_text::{REGULAR, ShapedGlyph, TextEngine};

use super::{HAlign, RasterizeOptions, RasterizedText};

// Apple-silicon Metal supports 16384² 2D textures. We rasterize glyphs at
// their full on-screen footprint, so the cap only bites for absurdly large
// text — and when it does, `rasterize` scales the layout down to fit rather
// than cropping it (the compositor scales it back up, linear-filtered).
const MAX_BITMAP_DIM: u32 = 16384;
const PADDING: u32 = 4;

/// One shaped line: its glyphs (each carrying its own resolved font, so a
/// fallback face mid-line draws correctly) and the line width.
struct LineMeasure {
    glyphs: Vec<ShapedGlyph>,
    width: f32,
}

/// Output of shaping a whole string at one font size: the per-line glyphs and
/// the metrics needed to size and place the bitmap.
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

/// Rasterizes text strings into R8 grayscale bitmaps via `manifold-text`.
pub struct TextRasterizer {
    engine: TextEngine,
}

impl Default for TextRasterizer {
    fn default() -> Self {
        Self::new()
    }
}

impl TextRasterizer {
    pub fn new() -> Self {
        Self {
            engine: TextEngine::new(),
        }
    }

    /// Enumerate all installed font family names, sorted alphabetically.
    pub fn available_font_families() -> Vec<String> {
        TextEngine::families()
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

        let fill = self.render_pass(&shaped, eff_font, opts, pad, bitmap_w, bitmap_h, None);
        let stroke = if want_stroke {
            Some(self.render_pass(
                &shaped,
                eff_font,
                opts,
                pad,
                bitmap_w,
                bitmap_h,
                Some(stroke_px),
            ))
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

    /// Render one coverage pass into a fresh R8 buffer. `stroke = None` draws
    /// filled glyphs (interior coverage); `Some(width_px)` strokes the glyph
    /// outlines at that line width. Geometry is identical across passes, so
    /// the fill and stroke masks register exactly.
    #[allow(clippy::too_many_arguments)]
    fn render_pass(
        &mut self,
        shaped: &ShapeResult,
        font_size: f32,
        opts: &RasterizeOptions,
        pad: u32,
        bitmap_w: u32,
        bitmap_h: u32,
        stroke: Option<f32>,
    ) -> Vec<u8> {
        let w = bitmap_w as i32;
        let h = bitmap_h as i32;
        let mut buf = vec![0u8; bitmap_w as usize * bitmap_h as usize];

        for (line_idx, measure) in shaped.line_measures.iter().enumerate() {
            if measure.glyphs.is_empty() {
                continue;
            }

            // Horizontal alignment offset
            let align_offset = match opts.h_align {
                HAlign::Left => 0.0,
                HAlign::Center => ((shaped.max_width - measure.width) * 0.5).max(0.0),
                HAlign::Right => (shaped.max_width - measure.width).max(0.0),
            };
            let origin_x = pad as f32 + align_offset;

            // Bitmap rows run top-first: line 0 sits at the top and each
            // subsequent baseline moves down by line_height.
            let baseline = pad as f32 + shaped.ascent + line_idx as f32 * shaped.line_height;

            for glyph in &measure.glyphs {
                let Some(image) = self
                    .engine
                    .rasterize(glyph.font, glyph.glyph, font_size, stroke)
                else {
                    continue;
                };
                let x0 = (origin_x + glyph.x).round() as i32 + image.left;
                let y0 = (baseline + glyph.y).round() as i32 - image.top;
                let iw = image.width as i32;
                let ih = image.height as i32;
                for row in 0..ih {
                    let y = y0 + row;
                    if y < 0 || y >= h {
                        continue;
                    }
                    for col in 0..iw {
                        let x = x0 + col;
                        if x < 0 || x >= w {
                            continue;
                        }
                        let src = image.coverage[(row * iw + col) as usize] as u32;
                        if src == 0 {
                            continue;
                        }
                        let dst = &mut buf[(y * w + x) as usize];
                        *dst = (src + *dst as u32 * (255 - src) / 255) as u8;
                    }
                }
            }
        }

        buf
    }

    /// Shape every line at `font_size`, returning the glyphs plus the metrics
    /// needed to size and place the bitmap. Returns `None` when no line
    /// produced any glyphs.
    fn shape_at(
        &mut self,
        lines: &[&str],
        font_size: f32,
        opts: &RasterizeOptions,
    ) -> Option<ShapeResult> {
        // An unknown or absent family resolves to the embedded Inter inside
        // the engine.
        let family = opts.font_family;
        let letter_spacing_px = opts.letter_spacing * font_size;

        // Font-level metrics (consistent across lines).
        let (ascent, descent) = self.engine.line_metrics(family, REGULAR, font_size);
        let base_line_height = ascent + descent;
        let line_height = base_line_height * opts.line_spacing;

        let mut line_measures: Vec<LineMeasure> = Vec::with_capacity(lines.len());
        let mut max_width: f32 = 0.0;

        for line_text in lines {
            let line_text = line_text.trim_end();
            if line_text.is_empty() {
                line_measures.push(LineMeasure {
                    glyphs: Vec::new(),
                    width: 0.0,
                });
                continue;
            }

            let mut line = self.engine.shape(line_text, family, REGULAR, font_size);
            if line.glyphs.is_empty() {
                line_measures.push(LineMeasure {
                    glyphs: Vec::new(),
                    width: 0.0,
                });
                continue;
            }

            // Letter spacing: shift each glyph by its index in the WHOLE line,
            // so a fallback-font glyph midway through still spaces
            // continuously with its neighbours.
            let glyph_count = line.glyphs.len();
            if letter_spacing_px.abs() > 0.001 {
                for (i, glyph) in line.glyphs.iter_mut().enumerate() {
                    glyph.x += i as f32 * letter_spacing_px;
                }
            }
            let w = line.width + glyph_count.saturating_sub(1) as f32 * letter_spacing_px;
            max_width = max_width.max(w);
            line_measures.push(LineMeasure {
                glyphs: line.glyphs,
                width: w,
            });
        }

        if line_measures.iter().all(|m| m.glyphs.is_empty()) {
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

    /// Pre-warm a font by family name so its face is loaded before the first
    /// render. Called from `set_string_params` when the font family changes.
    pub fn prewarm_font(&mut self, family: &str) {
        if family.is_empty() {
            return;
        }
        let _ = self.engine.shape("Hg", Some(family), REGULAR, 64.0);
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
        let stroke = rt
            .stroke
            .expect("stroke mask present when stroke_width > 0");
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
        let families = TextRasterizer::available_font_families();
        let family = families
            .iter()
            .find(|f| f.as_str() != "Inter")
            .map(String::as_str)
            .unwrap_or("Inter");
        let opts = RasterizeOptions {
            font_family: Some(family),
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
