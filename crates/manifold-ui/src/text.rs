use crate::node::{FontWeight, Vec2};

/// Abstract text measurement — implemented by the renderer.
/// Lives in manifold-ui (engine-agnostic) so panels can compute layout
/// without depending on wgpu/glyphon.
pub trait TextMeasure {
    /// Measure the pixel dimensions of a text string at the given font size and weight.
    fn measure_text(&self, text: &str, font_size: u16, font_weight: FontWeight) -> Vec2;
}

/// Stub implementation for tests — assumes monospaced 8px-wide characters.
pub struct MonoTextMeasure;

impl TextMeasure for MonoTextMeasure {
    fn measure_text(&self, text: &str, font_size: u16, _font_weight: FontWeight) -> Vec2 {
        let char_width = font_size as f32 * 0.6;
        let width = text.len() as f32 * char_width;
        let height = font_size as f32;
        Vec2::new(width, height)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mono_measure() {
        let m = MonoTextMeasure;
        let size = m.measure_text("Hello", 10, FontWeight::Regular);
        assert_eq!(size.x, 30.0); // 5 chars * 6px
        assert_eq!(size.y, 10.0);
    }

    #[test]
    fn empty_text() {
        let m = MonoTextMeasure;
        let size = m.measure_text("", 14, FontWeight::Bold);
        assert_eq!(size.x, 0.0);
        assert_eq!(size.y, 14.0);
    }
}
