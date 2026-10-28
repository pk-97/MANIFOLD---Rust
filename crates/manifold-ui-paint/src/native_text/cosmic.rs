//! The UI's fonts off the Mac: `manifold-text` (cosmic-text) shapes and
//! measures, swash rasterizes each glyph into a tight bitmap. Builds on the
//! Mac too so its tests run there.

use ahash::AHashMap;
use manifold_text::{BOLD, FontId, MEDIUM, REGULAR, TextEngine};
use manifold_ui::node::FontWeight;

use super::{GLYPH_PADDING, GlyphBitmap, GlyphFont, GlyphKey, ShapedRun};

fn weight_value(weight: FontWeight) -> u16 {
    match weight {
        FontWeight::Regular => REGULAR,
        FontWeight::Medium => MEDIUM,
        FontWeight::Bold => BOLD,
    }
}

pub(super) struct FontManager {
    engine: TextEngine,
    /// The embedded Inter face of each weight, in `FontWeight` order.
    inter: [Option<FontId>; 3],
    /// Every other face a run resolved to, by the key its glyphs carry.
    fonts: AHashMap<u64, FontId>,
}

impl FontManager {
    pub(super) fn new() -> Self {
        let engine = TextEngine::new();
        let inter = [FontWeight::Regular, FontWeight::Medium, FontWeight::Bold]
            .map(|weight| engine.face(None, weight_value(weight)));
        Self {
            engine,
            inter,
            fonts: AHashMap::new(),
        }
    }

    fn inter(&self, weight: FontWeight) -> Option<FontId> {
        self.inter[weight as usize]
    }

    /// The key a non-Inter face's glyphs carry, interned so `rasterize`
    /// finds the face again.
    fn intern(&mut self, font: FontId) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = ahash::AHasher::default();
        font.hash(&mut hasher);
        let key = hasher.finish();
        self.fonts.entry(key).or_insert(font);
        key
    }

    /// Shape `text` at `physical_size` px in the UI font at `weight`, or in
    /// the installed `family` (its interned id and name). Consecutive glyphs
    /// of one face form a run.
    pub(super) fn shape(
        &mut self,
        text: &str,
        family: Option<(u16, &str)>,
        weight: FontWeight,
        physical_size: f32,
    ) -> Vec<ShapedRun> {
        let line = self.engine.shape(
            text,
            family.map(|(_, name)| name),
            weight_value(weight),
            physical_size,
        );
        let inter = family.is_none().then(|| self.inter(weight)).flatten();
        let mut runs: Vec<ShapedRun> = Vec::new();
        for glyph in line.glyphs {
            let font = if Some(glyph.font) == inter {
                GlyphFont::Base(weight)
            } else {
                GlyphFont::Fallback(self.intern(glyph.font))
            };
            match runs.last_mut() {
                Some(run) if run.font == font => {
                    run.glyphs.push(glyph.glyph);
                    run.x.push(glyph.x);
                }
                _ => runs.push(ShapedRun {
                    font,
                    glyphs: vec![glyph.glyph],
                    x: vec![glyph.x],
                }),
            }
        }
        runs
    }

    /// `key`'s glyph, its ink bounds plus `GLYPH_PADDING` on every side. A
    /// blank glyph (a space) is padding only, so the atlas caches it too.
    pub(super) fn rasterize(&mut self, key: GlyphKey) -> GlyphBitmap {
        let pad = GLYPH_PADDING;
        let font = match key.font {
            GlyphFont::Base(weight) => self.inter(weight),
            GlyphFont::Fallback(hash) => self.fonts.get(&hash).copied(),
        };
        let size = key.size_x10 as f32 / 10.0;
        let Some(image) =
            font.and_then(|font| self.engine.rasterize(font, key.glyph_id, size, None))
        else {
            return GlyphBitmap {
                pixels: vec![0; (4 * pad * pad) as usize],
                width: 2 * pad,
                height: 2 * pad,
                bearing_x: 0.0,
                bearing_y: 0.0,
            };
        };
        let width = image.width + 2 * pad;
        let height = image.height + 2 * pad;
        let mut pixels = vec![0u8; (width * height) as usize];
        for row in 0..image.height {
            let src = (row * image.width) as usize;
            let dst = ((row + pad) * width + pad) as usize;
            pixels[dst..dst + image.width as usize]
                .copy_from_slice(&image.coverage[src..src + image.width as usize]);
        }
        GlyphBitmap {
            pixels,
            width,
            height,
            bearing_x: image.left as f32 - pad as f32,
            bearing_y: image.top as f32 + pad as f32,
        }
    }

    /// The advance width of `text` in the UI font at `font_size` px.
    pub(super) fn measure_width(&mut self, text: &str, font_size: f32, weight: FontWeight) -> f32 {
        self.engine
            .shape(text, None, weight_value(weight), font_size)
            .width
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inter_runs_key_as_base_and_other_faces_as_fallback() {
        let mut fm = FontManager::new();
        let runs = fm.shape("Hello", None, FontWeight::Bold, 24.0);
        assert_eq!(runs.len(), 1);
        assert!(runs[0].font == GlyphFont::Base(FontWeight::Bold));
        assert_eq!(runs[0].glyphs.len(), 5);
        assert!(runs[0].x.windows(2).all(|w| w[1] > w[0]));
        // A family drawn by name keys as its own face, even when it is Inter.
        let named = fm.shape("Hello", Some((0, "Inter")), FontWeight::Regular, 24.0);
        assert!(
            named
                .iter()
                .all(|r| matches!(r.font, GlyphFont::Fallback(_)))
        );
    }

    #[test]
    fn glyph_bitmaps_sit_on_the_baseline_and_blanks_are_padding() {
        let mut fm = FontManager::new();
        let runs = fm.shape("H x", None, FontWeight::Regular, 32.0);
        let key = |glyph_id| GlyphKey {
            glyph_id,
            size_x10: 320,
            font: GlyphFont::Base(FontWeight::Regular),
        };
        let h = fm.rasterize(key(runs[0].glyphs[0]));
        assert_eq!(h.pixels.len(), (h.width * h.height) as usize);
        // Inter's cap height is about 23 px at 32 px; the bitmap's top sits
        // that far above the baseline, plus the padding.
        assert!((23.0..27.0).contains(&h.bearing_y), "{}", h.bearing_y);
        assert!(h.pixels.contains(&255));
        // The padding rows and columns stay empty.
        assert!(h.pixels[..h.width as usize].iter().all(|&p| p == 0));
        let space = fm.rasterize(key(runs[0].glyphs[1]));
        assert_eq!(
            (space.width, space.height),
            (2 * GLYPH_PADDING, 2 * GLYPH_PADDING)
        );
        assert!(space.pixels.iter().all(|&p| p == 0));
    }

    #[test]
    fn measured_width_scales_with_size_and_weight() {
        let mut fm = FontManager::new();
        let regular = fm.measure_width("CHANNEL", 11.0, FontWeight::Regular);
        let bold = fm.measure_width("CHANNEL", 11.0, FontWeight::Bold);
        let large = fm.measure_width("CHANNEL", 22.0, FontWeight::Regular);
        assert!(regular > 30.0 && regular < 70.0, "{regular}");
        assert!(bold > regular);
        assert!((large / regular - 2.0).abs() < 0.02);
    }
}
