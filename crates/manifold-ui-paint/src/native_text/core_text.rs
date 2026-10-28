//! The Mac's UI fonts: CoreText shapes and measures, CoreGraphics
//! rasterizes each glyph into a full-line-height bitmap.

use ahash::AHashMap;
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
    geometry::{CGPoint, CGSize},
};
use core_text::{
    font::CTFont, font_descriptor::kCTFontOrientationDefault, line::CTLine,
    string_attributes::kCTFontAttributeName,
};
use manifold_ui::node::FontWeight;
use std::sync::Arc;

use super::{GLYPH_PADDING, GlyphBitmap, GlyphFont, GlyphKey, ShapedRun};

/// Loads the three Inter weights and creates CTFont instances on demand.
pub(super) struct FontManager {
    regular: CGFont,
    medium: CGFont,
    bold: CGFont,
    /// (size_x10_physical, weight) → CTFont cache.
    cache: AHashMap<(u16, FontWeight), CTFont>,
    /// CoreText fallback fonts encountered during shaping (BUG-107), keyed
    /// by the hash `shape_line` computed from the font's PostScript name.
    /// Interned by `intern_fallback` at shape time so `rasterize` can look
    /// the exact font back up when it later rasterizes that run's glyph ids.
    fallback_fonts: AHashMap<u64, CTFont>,
    /// Installed fonts drawn by name (font-picker previews), keyed by
    /// (family id from `NativeTextRenderer::families`, size_x10 physical).
    family_fonts: AHashMap<(u16, u16), CTFont>,
}

impl FontManager {
    pub(super) fn new() -> Self {
        let regular = Self::load(manifold_foundation::fonts::INTER_REGULAR);
        let medium = Self::load(manifold_foundation::fonts::INTER_MEDIUM);
        let bold = Self::load(manifold_foundation::fonts::INTER_BOLD);
        Self {
            regular,
            medium,
            bold,
            cache: AHashMap::new(),
            fallback_fonts: AHashMap::new(),
            family_fonts: AHashMap::new(),
        }
    }

    /// Shape `text` at `physical_size` px in the UI font at `weight`, or in
    /// the installed `family` (its interned id and name).
    pub(super) fn shape(
        &mut self,
        text: &str,
        family: Option<(u16, &str)>,
        weight: FontWeight,
        physical_size: f32,
    ) -> Vec<ShapedRun> {
        // Clone (a cheap CF retain bump) so the borrow of `self` ends here:
        // `shape_line` interns any fallback fonts it discovers (BUG-107).
        let (ct_font, base_weight) = match family {
            Some((id, name)) => (self.get_family_font(id, name, physical_size), None),
            None => (
                self.get_ct_font(physical_size, weight).clone(),
                Some(weight),
            ),
        };
        shape_line(self, base_weight, &ct_font, text)
    }

    /// `key`'s glyph as an advance-wide, full-line-high bitmap with
    /// `GLYPH_PADDING` on every side, so every glyph of a size shares one
    /// baseline row.
    pub(super) fn rasterize(&mut self, key: GlyphKey) -> GlyphBitmap {
        let physical_size = key.size_x10 as f32 / 10.0;
        // BUG-107: rasterize with the SAME font this glyph id was shaped
        // against — `Base` is the fast common path (an embedded Inter
        // weight); `Fallback` looks up the exact CoreText-resolved font
        // `shape_line` interned, so a fallback run's glyph ids land on their
        // own glyph table instead of an arbitrary glyph in Inter's.
        let ct_font: CTFont = match key.font {
            GlyphFont::Base(weight) => self.get_ct_font(physical_size, weight).clone(),
            // Interned at whatever size first shaped it; the key's size wins.
            GlyphFont::Fallback(hash) => match self.fallback_fonts.get(&hash) {
                Some(f) => f.clone_with_font_size(physical_size as f64),
                // Shaping always interns a fallback font before any of its
                // glyphs reach the atlas this same frame, so this is
                // unreached in practice. Degrade to the base weight rather
                // than panic — one glyph mis-renders instead of the
                // renderer crashing.
                None => self.get_ct_font(physical_size, FontWeight::Regular).clone(),
            },
        };
        let ascent = ct_font.ascent() as f32;
        let descent = ct_font.descent().abs() as f32;

        let glyph_id = key.glyph_id;
        let mut advance_size = CGSize::new(0.0, 0.0);
        let advance_w = unsafe {
            ct_font.get_advances_for_glyphs(
                kCTFontOrientationDefault,
                &glyph_id,
                &mut advance_size,
                1,
            )
        } as f32;

        // Width = advance + horizontal padding; height = full line height +
        // vertical padding.
        let width = (advance_w.ceil() as u32).max(1) + 2 * GLYPH_PADDING;
        let height = (ascent + descent).ceil() as u32 + 2 * GLYPH_PADDING;
        GlyphBitmap {
            pixels: rasterize_glyph_bitmap(&ct_font, glyph_id, width, height, descent),
            width,
            height,
            // The left padding sits before the glyph origin.
            bearing_x: -(GLYPH_PADDING as f32),
            // Baseline (y-down) to the bitmap's top edge: ascent + padding.
            bearing_y: ascent + GLYPH_PADDING as f32,
        }
    }

    /// The typographic width of `text` in the UI font at `font_size` px.
    pub(super) fn measure_width(&mut self, text: &str, font_size: f32, weight: FontWeight) -> f32 {
        let ct_font = self.get_ct_font(font_size, weight);
        make_ct_line(ct_font, text).get_typographic_bounds().width as f32
    }

    /// An installed font by family name, resolved the same way the Text
    /// generator resolves it (`new_from_name`), so a preview shows what the
    /// generator will draw. An unknown name, or a symbol font (Wingdings,
    /// Zapf Dingbats, Emoji — its name would draw as pictures), falls back
    /// to Inter Regular so the name stays readable.
    fn get_family_font(&mut self, family_id: u16, name: &str, physical_size: f32) -> CTFont {
        use core_text::font_descriptor::{kCTFontClassMaskTrait, kCTFontSymbolicClass};
        let size_x10 = (physical_size * 10.0).round() as u16;
        if let Some(f) = self.family_fonts.get(&(family_id, size_x10)) {
            return f.clone();
        }
        let font = core_text::font::new_from_name(name, physical_size as f64)
            .ok()
            .filter(|f| f.symbolic_traits() & kCTFontClassMaskTrait != kCTFontSymbolicClass)
            .unwrap_or_else(|| {
                core_text::font::new_from_CGFont(&self.regular, physical_size as f64)
            });
        self.family_fonts
            .insert((family_id, size_x10), font.clone());
        font
    }

    /// Register a CoreText-resolved fallback font (a run's font that isn't
    /// one of the three embedded Inter weights) under a stable hash of its
    /// PostScript name, so `rasterize` can recover the same `CTFont` later.
    /// Idempotent — re-interning the same font is a cheap hash + a map
    /// lookup that already hits.
    fn intern_fallback(&mut self, font: &CTFont) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = ahash::AHasher::default();
        font.postscript_name().hash(&mut hasher);
        let key = hasher.finish();
        self.fallback_fonts
            .entry(key)
            .or_insert_with(|| font.clone());
        key
    }

    fn load(ttf_bytes: &'static [u8]) -> CGFont {
        let data: Vec<u8> = ttf_bytes.to_vec();
        let provider = CGDataProvider::from_buffer(Arc::new(data));
        CGFont::from_data_provider(provider).expect("Failed to create CGFont from Inter TTF")
    }

    /// Get or create a CTFont at the given physical size and weight.
    fn get_ct_font(&mut self, physical_size: f32, weight: FontWeight) -> &CTFont {
        let size_x10 = (physical_size * 10.0).round() as u16;
        self.cache.entry((size_x10, weight)).or_insert_with(|| {
            let cg_font = match weight {
                FontWeight::Regular => &self.regular,
                FontWeight::Medium => &self.medium,
                FontWeight::Bold => &self.bold,
            };
            core_text::font::new_from_CGFont(cg_font, physical_size as f64)
        })
    }
}

/// Rasterize a single glyph into a fresh grayscale buffer (R8).
///
/// Uses CG's native y-up coordinate system (NO Y-flip). The bitmap memory
/// is laid out top-row-first, and CG y=0 is at the bottom. So ascenders
/// (high CG y) end up in the top rows of memory — right-side-up glyphs.
///
/// `descent` is the font's descent (positive value, distance below baseline).
fn rasterize_glyph_bitmap(
    ct_font: &CTFont,
    glyph_id: u16,
    bitmap_w: u32,
    bitmap_h: u32,
    descent: f32,
) -> Vec<u8> {
    let w = bitmap_w as usize;
    let h = bitmap_h as usize;

    let mut pixels = vec![0u8; w * h];

    let color_space = CGColorSpace::create_device_gray();
    let ctx = CGContext::create_bitmap_context(
        Some(pixels.as_mut_ptr() as *mut std::ffi::c_void),
        w,
        h,
        8, // bits per component
        w, // bytes per row (1 byte per pixel)
        &color_space,
        0u32, // kCGImageAlphaNone
    );

    // NO Y-flip. CG native y-up: y=0 at bottom, y=h at top.
    // Bitmap memory: row 0 = top of image = CG y=h.
    //
    // Place baseline at y = descent + GLYPH_PADDING (from bottom).
    // Ascent extends upward, descent extends downward — both fit in the bitmap.

    ctx.set_rgb_fill_color(1.0, 1.0, 1.0, 1.0);
    ctx.set_allows_font_smoothing(false);
    ctx.set_should_smooth_fonts(false);

    let baseline_y = (descent + GLYPH_PADDING as f32) as f64;
    let origin_x = GLYPH_PADDING as f64;
    let position = CGPoint::new(origin_x, baseline_y);

    ct_font.draw_glyphs(&[glyph_id], &[position], ctx);

    pixels
}

/// Create a CTLine from a text string and a CTFont.
fn make_ct_line(ct_font: &CTFont, text: &str) -> CTLine {
    let cf_text = CFString::new(text);
    let mut attr_str = CFMutableAttributedString::new();
    attr_str.replace_str(&cf_text, CFRange::init(0, 0));
    let range = CFRange::init(0, cf_text.char_len());
    // Safety: kCTFontAttributeName is a valid CoreText attribute key (a static CFStringRef).
    unsafe {
        attr_str.set_attribute(range, kCTFontAttributeName, ct_font);
    }
    CTLine::new_with_attributed_string(attr_str.as_concrete_TypeRef())
}

/// Shape text into its CoreText glyph runs. A run whose resolved font
/// differs from `ct_font` is interned into `font_mgr` (`intern_fallback`) so
/// the glyph atlas can rasterize it with the right font later — `base_weight`
/// is `ct_font`'s own Inter weight, carried through for the common
/// non-fallback case so the atlas key stays the cheap `GlyphFont::Base`
/// variant. `None` means `ct_font` is not Inter (an installed font drawn by
/// name), so every run is interned. Runs stay separate (BUG-107) because
/// CoreText splits a run whenever it falls back to another font, and that
/// run's glyph ids only resolve against ITS font.
fn shape_line(
    font_mgr: &mut FontManager,
    base_weight: Option<FontWeight>,
    ct_font: &CTFont,
    text: &str,
) -> Vec<ShapedRun> {
    let line = make_ct_line(ct_font, text);
    let runs = line.glyph_runs();
    let base_ps_name = ct_font.postscript_name();

    let mut result: Vec<ShapedRun> = Vec::new();
    for run in runs.iter() {
        let count = run.glyph_count() as usize;
        if count == 0 {
            continue;
        }
        // The font CoreText actually shaped this run with — differs from
        // `ct_font` exactly when this run is a fallback split. Falls back
        // to `Base` if the attribute is somehow absent.
        let run_font = run.attributes().and_then(|attrs| {
            // Safety: kCTFontAttributeName is a valid CoreText dictionary
            // key (a static CFStringRef); `find` only reads it.
            let key = unsafe { kCTFontAttributeName };
            attrs.find(key).and_then(|v| v.downcast::<CTFont>())
        });
        let font = match (run_font, base_weight) {
            (Some(f), Some(w)) if f.postscript_name() == base_ps_name => GlyphFont::Base(w),
            (Some(f), _) => GlyphFont::Fallback(font_mgr.intern_fallback(&f)),
            (None, Some(w)) => GlyphFont::Base(w),
            (None, None) => GlyphFont::Fallback(font_mgr.intern_fallback(ct_font)),
        };
        result.push(ShapedRun {
            font,
            glyphs: run.glyphs().into_owned(),
            x: run.positions().iter().map(|p| p.x as f32).collect(),
        });
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_measurement() {
        let mut fm = FontManager::new();
        let ct_font = fm.get_ct_font(12.0, FontWeight::Regular);
        let bounds = make_ct_line(ct_font, "Hello").get_typographic_bounds();
        assert!(bounds.width > 0.0, "width {}", bounds.width);
        assert!(bounds.ascent > 0.0, "ascent {}", bounds.ascent);
        assert!(bounds.descent >= 0.0, "descent {}", bounds.descent);
        let height = bounds.ascent + bounds.descent;
        assert!(height >= 8.0, "line height {height}");
    }

    /// The font picker draws a name in its own installed font: shaping
    /// against that font must key every run to it (not Inter), and each
    /// size must rasterize at its own size.
    #[test]
    fn installed_font_shapes_as_its_own_font_at_each_size() {
        let mut fm = FontManager::new();
        let georgia = fm.get_family_font(0, "Georgia", 24.0);
        assert_eq!(georgia.family_name(), "Georgia", "Georgia ships with macOS");
        let runs = fm.shape("Hg", Some((0, "Georgia")), FontWeight::Regular, 24.0);
        assert!(!runs.is_empty());
        assert!(
            runs.iter()
                .all(|r| matches!(r.font, GlyphFont::Fallback(_)))
        );
        let font = runs[0].font;
        let glyph_id = runs[0].glyphs[0];
        let height = |fm: &mut FontManager, size_x10| {
            fm.rasterize(GlyphKey {
                glyph_id,
                size_x10,
                font,
            })
            .height
        };
        let (small, large) = (height(&mut fm, 120), height(&mut fm, 480));
        assert!(
            large > small * 3,
            "48px glyph ({large}) must be ~4x the 12px one ({small})"
        );
    }

    /// A symbol font's name would draw as pictures, so the picker shows it
    /// in Inter instead. Wingdings and Zapf Dingbats ship with macOS.
    #[test]
    fn symbol_fonts_preview_their_name_in_the_ui_font() {
        let mut fm = FontManager::new();
        for (id, name) in ["Wingdings", "Zapf Dingbats"].into_iter().enumerate() {
            let f = fm.get_family_font(id as u16, name, 24.0);
            assert!(
                f.family_name().starts_with("Inter"),
                "{name} resolved to {}",
                f.family_name()
            );
        }
        assert_eq!(
            fm.get_family_font(9, "Georgia", 24.0).family_name(),
            "Georgia"
        );
    }
}
