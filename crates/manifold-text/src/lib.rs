//! The text engine off the Mac. cosmic-text shapes a line over the embedded
//! Inter and the system's installed fonts, falling back per character to a
//! font that has it; swash rasterizes one glyph's coverage, filled or
//! stroked. The UI's glyph atlas and the Text generator both draw through
//! it on Linux; the Mac draws with CoreText.
//!
//! The font database loads once per process (a scan of the system's font
//! folders); each engine clones it.

use std::sync::{Arc, OnceLock};

use cosmic_text::{Attrs, Buffer, Family, FontSystem, Metrics, Shaping, Weight, fontdb};
use swash::scale::image::Content;
use swash::scale::{Render, ScaleContext, Source, StrikeWith};
use swash::zeno::{Cap, Fill, Format, Join, Stroke, Style};

pub use cosmic_text::fontdb::ID as FontId;

/// The embedded font's family name: the UI font and the generator's default.
pub const INTER: &str = "Inter";
pub const REGULAR: u16 = 400;
pub const MEDIUM: u16 = 500;
pub const BOLD: u16 = 700;

/// One shaped glyph: the font it resolved to (a fallback font for a
/// character the requested one lacks), its id in that font, and its pen
/// position in pixels from the line's start on the baseline (y down).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ShapedGlyph {
    pub font: FontId,
    pub glyph: u16,
    pub x: f32,
    pub y: f32,
}

/// One shaped line and its advance width in pixels.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ShapedLine {
    pub glyphs: Vec<ShapedGlyph>,
    pub width: f32,
}

/// A glyph's coverage, one byte per pixel, rows top first. `left` is the
/// pixels from the pen position to the bitmap's left edge, `top` the pixels
/// from the baseline up to its top edge.
#[derive(Clone, Debug)]
pub struct GlyphImage {
    pub left: i32,
    pub top: i32,
    pub width: u32,
    pub height: u32,
    pub coverage: Vec<u8>,
}

fn database() -> &'static fontdb::Database {
    static DB: OnceLock<fontdb::Database> = OnceLock::new();
    DB.get_or_init(|| {
        let mut db = fontdb::Database::new();
        db.load_system_fonts();
        for bytes in [
            manifold_foundation::fonts::INTER_REGULAR,
            manifold_foundation::fonts::INTER_MEDIUM,
            manifold_foundation::fonts::INTER_BOLD,
        ] {
            db.load_font_source(fontdb::Source::Binary(Arc::new(bytes)));
        }
        db
    })
}

/// `LANG`'s language tag (`de_DE.UTF-8` → `de-DE`), which steers the
/// fallback order for scripts several fonts cover.
fn locale() -> String {
    std::env::var("LANG")
        .ok()
        .and_then(|lang| lang.split('.').next().map(|tag| tag.replace('_', "-")))
        .filter(|tag| !tag.is_empty() && tag != "C" && tag != "POSIX")
        .unwrap_or_else(|| "en-US".to_string())
}

fn has_family(db: &fontdb::Database, name: &str) -> bool {
    db.faces().any(|face| {
        face.families
            .iter()
            .any(|(n, _)| n.eq_ignore_ascii_case(name))
    })
}

/// Shapes and rasterizes text. One per thread that draws text.
pub struct TextEngine {
    fonts: FontSystem,
    scale: ScaleContext,
}

impl Default for TextEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl TextEngine {
    pub fn new() -> Self {
        Self {
            fonts: FontSystem::new_with_locale_and_db(locale(), database().clone()),
            scale: ScaleContext::new(),
        }
    }

    /// Installed family names and Inter, sorted case-insensitively.
    pub fn families() -> Vec<String> {
        let mut names: Vec<String> = database()
            .faces()
            .filter_map(|face| face.families.first().map(|(name, _)| name.clone()))
            .collect();
        names.sort_unstable_by_key(|name| name.to_lowercase());
        names.dedup();
        names
    }

    /// `family` when it is installed, else Inter, as the generator and the
    /// font picker have always resolved an unknown name.
    fn family<'a>(&self, family: Option<&'a str>) -> &'a str {
        family
            .filter(|name| has_family(self.fonts.db(), name))
            .unwrap_or(INTER)
    }

    /// The face shaping starts from for `family` (`None` = Inter) at
    /// `weight`, before any per-character fallback.
    pub fn face(&self, family: Option<&str>, weight: u16) -> Option<FontId> {
        self.fonts.db().query(&fontdb::Query {
            families: &[Family::Name(self.family(family))],
            weight: Weight(weight),
            ..Default::default()
        })
    }

    /// Shape one line of `text` (no line breaks) in `family` (`None` =
    /// Inter) at `weight` and `size_px`.
    pub fn shape(
        &mut self,
        text: &str,
        family: Option<&str>,
        weight: u16,
        size_px: f32,
    ) -> ShapedLine {
        if text.is_empty() || size_px <= 0.0 {
            return ShapedLine::default();
        }
        let family = self.family(family);
        let attrs = Attrs::new()
            .family(Family::Name(family))
            .weight(Weight(weight));
        let mut buffer = Buffer::new_empty(Metrics::new(size_px, size_px));
        let mut buffer = buffer.borrow_with(&mut self.fonts);
        buffer.set_size(None, None);
        buffer.set_text(text, &attrs, Shaping::Advanced, None);
        buffer.shape_until_scroll(false);
        let mut line = ShapedLine::default();
        for run in buffer.layout_runs() {
            line.width = line.width.max(run.line_w);
            line.glyphs.extend(run.glyphs.iter().map(|g| ShapedGlyph {
                font: g.font_id,
                glyph: g.glyph_id,
                x: g.x + g.x_offset * g.font_size,
                y: g.y - g.y_offset * g.font_size,
            }));
        }
        line
    }

    /// Ascent and descent in pixels (both positive) of the face `family`
    /// resolves to at `weight` and `size_px`.
    pub fn line_metrics(&mut self, family: Option<&str>, weight: u16, size_px: f32) -> (f32, f32) {
        let Some(font) = self
            .face(family, weight)
            .and_then(|id| self.fonts.get_font(id, Weight(weight)))
        else {
            return (size_px * 0.8, size_px * 0.2);
        };
        let metrics = font.as_swash().metrics(&[]).scale(size_px);
        (metrics.ascent.abs(), metrics.descent.abs())
    }

    /// Glyph `glyph` of `font` at `size_px`: filled, or its outline stroked
    /// `stroke_px` wide with round joins and caps. `None` for a glyph with
    /// nothing to draw (a space) or a font that no longer loads.
    pub fn rasterize(
        &mut self,
        font: FontId,
        glyph: u16,
        size_px: f32,
        stroke_px: Option<f32>,
    ) -> Option<GlyphImage> {
        let font = self.fonts.get_font(font, Weight(REGULAR))?;
        let mut scaler = self.scale.builder(font.as_swash()).size(size_px).build();
        let image = match stroke_px {
            None => Render::new(&[
                Source::ColorOutline(0),
                Source::ColorBitmap(StrikeWith::BestFit),
                Source::Outline,
            ])
            .format(Format::Alpha)
            .style(Style::Fill(Fill::NonZero))
            .render(&mut scaler, glyph)?,
            Some(width) => {
                let mut stroke = Stroke::new(width);
                stroke.join(Join::Round).cap(Cap::Round);
                Render::new(&[Source::Outline])
                    .format(Format::Alpha)
                    .style(Style::Stroke(stroke))
                    .render(&mut scaler, glyph)?
            }
        };
        if image.placement.width == 0 || image.placement.height == 0 {
            return None;
        }
        let coverage = match image.content {
            Content::Mask => image.data,
            // Colour glyphs (emoji) and subpixel masks: their alpha.
            Content::Color | Content::SubpixelMask => {
                image.data.chunks_exact(4).map(|px| px[3]).collect()
            }
        };
        Some(GlyphImage {
            left: image.placement.left,
            top: image.placement.top,
            width: image.placement.width,
            height: image.placement.height,
            coverage,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inter_shapes_in_its_own_face_and_scales_with_size() {
        let mut engine = TextEngine::new();
        let inter = engine.face(None, REGULAR).expect("Inter is embedded");
        let small = engine.shape("Hello", None, REGULAR, 20.0);
        let large = engine.shape("Hello", None, REGULAR, 40.0);
        assert_eq!(small.glyphs.len(), 5);
        assert!(small.glyphs.iter().all(|g| g.font == inter && g.y == 0.0));
        assert!(small.glyphs.windows(2).all(|w| w[1].x > w[0].x));
        // Inter's "Hello" is about 2.4 em wide.
        assert!((40.0..60.0).contains(&small.width), "{}", small.width);
        assert!((large.width / small.width - 2.0).abs() < 0.02);
        let bold = engine.shape("Hello", None, BOLD, 20.0);
        assert_ne!(bold.glyphs[0].font, inter);
        assert!(bold.width > small.width);
    }

    #[test]
    fn unknown_family_falls_back_to_inter() {
        let mut engine = TextEngine::new();
        assert_eq!(
            engine.shape("Hi", Some("No Such Font 123"), REGULAR, 16.0),
            engine.shape("Hi", None, REGULAR, 16.0)
        );
        let families = TextEngine::families();
        assert!(families.iter().any(|f| f == INTER));
        assert!(
            families
                .windows(2)
                .all(|w| w[0].to_lowercase() <= w[1].to_lowercase())
        );
    }

    #[test]
    fn a_character_inter_lacks_shapes_in_a_fallback_face() {
        let mut engine = TextEngine::new();
        let inter = engine.face(None, REGULAR).expect("Inter is embedded");
        let line = engine.shape("A\u{6F22}", None, REGULAR, 24.0);
        assert_eq!(line.glyphs[0].font, inter);
        // Only where some installed font covers the character.
        if line.glyphs.len() == 2 && line.glyphs[1].glyph != 0 {
            assert_ne!(line.glyphs[1].font, inter);
        }
    }

    #[test]
    fn glyphs_rasterize_filled_and_stroked_above_the_baseline() {
        let mut engine = TextEngine::new();
        let inter = engine.face(None, REGULAR).expect("Inter is embedded");
        let h = engine.shape("H", None, REGULAR, 32.0).glyphs[0];
        let fill = engine
            .rasterize(inter, h.glyph, 32.0, None)
            .expect("H draws");
        // Inter's cap height is 0.727 em: about 23 px at 32 px.
        assert!((22..=25).contains(&fill.top), "top {}", fill.top);
        assert!((22..=26).contains(&fill.height), "height {}", fill.height);
        assert_eq!(fill.coverage.len(), (fill.width * fill.height) as usize);
        assert!(fill.coverage.contains(&255));
        let stroke = engine
            .rasterize(inter, h.glyph, 32.0, Some(4.0))
            .expect("H strokes");
        // Half the stroke spills outside the outline on each side.
        assert!(
            stroke.width >= fill.width + 3,
            "{} vs {}",
            stroke.width,
            fill.width
        );
        assert!(stroke.top > fill.top);
        let space = engine.shape(" ", None, REGULAR, 32.0).glyphs[0];
        assert!(engine.rasterize(inter, space.glyph, 32.0, None).is_none());
        let (ascent, descent) = engine.line_metrics(None, REGULAR, 32.0);
        assert!((28.0..34.0).contains(&ascent) && (6.0..10.0).contains(&descent));
    }
}
