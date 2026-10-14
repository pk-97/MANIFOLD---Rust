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

    // Longest prefix of whole chars (at least one, shorter than the text)
    // that fits. Prefix width grows with length, so a binary search over char
    // counts finds the same prefix the old end-trim loop did in O(log n)
    // measures (`truncate_matches_linear_reference` proves it).
    let char_start = |k: usize| text.char_indices().nth(k).map_or(text.len(), |(i, _)| i);
    let fits = |k: usize| measurer.measure_text(&text[..char_start(k)], font_size, weight).x <= target_w;
    let (mut lo, mut hi) = (0, text.chars().count());
    while hi - lo > 1 {
        let mid = lo + (hi - lo) / 2;
        if fits(mid) {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    if lo == 0 {
        return ellipsis.to_string();
    }
    format!("{}{}", &text[..char_start(lo)], ellipsis)
}

/// Greedy word wrap of `text` into lines that each measure within `max_width`.
/// A word wider than `max_width` on its own is split at character boundaries,
/// so every returned line fits. Empty text yields no lines.
pub fn wrap_to_width(
    measurer: &dyn TextMeasure,
    text: &str,
    font_size: u16,
    weight: FontWeight,
    max_width: f32,
) -> Vec<String> {
    let fits = |s: &str| measurer.measure_text(s, font_size, weight).x <= max_width;
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        let candidate = if line.is_empty() { word.to_string() } else { format!("{line} {word}") };
        if fits(&candidate) {
            line = candidate;
            continue;
        }
        if !line.is_empty() {
            lines.push(std::mem::take(&mut line));
        }
        if fits(word) {
            line = word.to_string();
            continue;
        }
        for ch in word.chars() {
            line.push(ch);
            if !fits(&line) && line.chars().count() > 1 {
                line.pop();
                lines.push(std::mem::take(&mut line));
                line.push(ch);
            }
        }
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

/// Always-on default measurer carried by every [`UITree`](crate::tree::UITree):
/// a weight-aware character-width heuristic. It needs no GPU and no font
/// state, so a tree can always answer `measure_text`, in tests included. The
/// app installs `CachedTextMeasure` on every tree it draws.
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Monospaced test measurer: 0.6 em per byte.
    struct MonoTextMeasure;

    impl TextMeasure for MonoTextMeasure {
        fn measure_text(&self, text: &str, font_size: u16, _font_weight: FontWeight) -> Vec2 {
            let char_width = font_size as f32 * 0.6;
            let width = text.len() as f32 * char_width;
            let height = font_size as f32;
            Vec2::new(width, height)
        }
    }

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

    /// The pre-binary-search `truncate_with_ellipsis`, kept verbatim as the
    /// reference the new one must match.
    fn truncate_linear_reference(
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
        if measurer.measure_text(text, font_size, weight).x <= max_width {
            return text.to_string();
        }
        let ellipsis = "...";
        let target_w = max_width - measurer.measure_text(ellipsis, font_size, weight).x;
        if target_w <= 0.0 {
            return ellipsis.to_string();
        }
        for len in (1..text.len()).rev() {
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

    const TRUNCATE_CASES: &[&str] = &[
        "",
        "a",
        "é",
        "Hi",
        "Layer 1",
        "Very Long Layer Name That Will Not Fit",
        "Bloom → Mirror → Feedback",
        "naïve café déjà vu",
        "日本語のレイヤー名",
        "🎛️ Macro 🎚️ Knob",
        "export_final_master_v12_FINAL(2).mov",
        "WWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWW",
        "iiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiii",
    ];

    #[test]
    fn truncate_matches_linear_reference() {
        let measurers: [&dyn TextMeasure; 2] = [&HeuristicTextMeasure, &MonoTextMeasure];
        for m in measurers {
            for text in TRUNCATE_CASES {
                for size in [9_u16, 11, 13] {
                    for weight in [FontWeight::Regular, FontWeight::Medium, FontWeight::Bold] {
                        for tenth in 0..=4000 {
                            let w = tenth as f32 * 0.1 - 1.0;
                            assert_eq!(
                                truncate_with_ellipsis(m, text, size, weight, w),
                                truncate_linear_reference(m, text, size, weight, w),
                                "{text:?} size {size} {weight:?} width {w}"
                            );
                        }
                    }
                }
            }
        }
    }
}
