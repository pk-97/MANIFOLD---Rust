//! Standalone text rasterizer: shapes a string and rasterizes it into an R8
//! grayscale pixel buffer. Used by the Text generator to produce a CPU-side
//! bitmap that gets uploaded to a GPU texture. Deliberately separate from
//! `native_text.rs` (which is the UI-thread glyph atlas system).
//!
//! CoreText on macOS, `manifold-text` (cosmic-text) elsewhere. The cosmic
//! rasterizer builds on the Mac too so its tests run there.

#[cfg(target_os = "macos")]
mod core_text;
#[cfg(target_os = "macos")]
pub use core_text::TextRasterizer;

#[cfg_attr(target_os = "macos", allow(dead_code))]
mod cosmic;
#[cfg(not(target_os = "macos"))]
pub use cosmic::TextRasterizer;

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
