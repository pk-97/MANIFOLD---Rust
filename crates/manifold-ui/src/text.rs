use crate::node::{FontWeight, Vec2};

/// Abstract text measurement — implemented by the renderer.
/// Lives in manifold-ui (engine-agnostic) so panels can compute layout
/// without depending on the GPU backend.
pub trait TextMeasure {
    /// Measure the pixel dimensions of a text string at the given font size and weight.
    fn measure_text(&self, text: &str, font_size: u16, font_weight: FontWeight) -> Vec2;
}

/// Truncate text to fit within `max_width`, appending "..." if truncated.
/// Returns the original text if it fits.
/// Port of Unity BitmapText.TruncateWithEllipsis.
pub fn truncate_with_ellipsis(
    measurer: &dyn TextMeasure,
    text: &str,
    font_size: u16,
    weight: FontWeight,
    max_width: f32,
) -> String {
    if text.is_empty() {
        return text.to_string();
    }
    if max_width <= 0.0 {
        return String::new();
    }

    let size = measurer.measure_text(text, font_size, weight);
    if size.x <= max_width {
        return text.to_string();
    }

    let ellipsis = "...";
    let ellipsis_w = measurer.measure_text(ellipsis, font_size, weight).x;
    let target_w = max_width - ellipsis_w;
    if target_w <= 0.0 {
        return ellipsis.to_string();
    }

    // Progressive trim from end
    for len in (1..text.len()).rev() {
        // Ensure we don't split a multi-byte char
        if !text.is_char_boundary(len) {
            continue;
        }
        let sub = &text[..len];
        if measurer.measure_text(sub, font_size, weight).x <= target_w {
            return format!("{}{}", sub, ellipsis);
        }
    }
    ellipsis.to_string()
}

/// Always-on default measurer carried by every [`UITree`](crate::tree::UITree):
/// a weight-aware character-width heuristic, identical to `NativeTextRenderer`'s
/// `TextMeasure` impl. It needs no GPU and no font state, so a tree can always
/// answer `measure_text` at build time. The app upgrades the tree to a
/// CoreText-accurate measurer where precise size-to-content matters; this is the
/// baseline that keeps the build path measurement-capable even in tests and
/// before that upgrade lands.
pub struct HeuristicTextMeasure;

impl TextMeasure for HeuristicTextMeasure {
    fn measure_text(&self, text: &str, font_size: u16, font_weight: FontWeight) -> Vec2 {
        let em = font_size as f32;
        let avg_char_width = match font_weight {
            FontWeight::Bold => em * 0.56,
            FontWeight::Medium => em * 0.54,
            FontWeight::Regular => em * 0.52,
        };
        Vec2::new(text.chars().count() as f32 * avg_char_width, em)
    }
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
