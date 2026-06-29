//! Styling context — a small immutable palette that flows DOWN a build subtree.
//!
//! The problem this fixes: colour is hand-threaded today. To give the modulation
//! drawer its source identity (orange Trigger / magenta LFO / green Audio / purple
//! Ableton) the *same one fact* — "this subtree belongs to source X" — has to be
//! passed into five separate places: the container fill, the accent spine, every
//! option button, every slider, and the text. Add a source and you re-thread all
//! five.
//!
//! A [`Theme`] carries that one fact once. A parent picks an accent
//! ([`Theme::with_accent`]) and derives a tinted surface ([`Theme::tinted`]); the
//! controls inside read their colours *from* the theme
//! ([`Theme::option_style`], [`Theme::slider_colors`], [`Theme::label_style`])
//! instead of taking explicit `Color32` args. Threading one `Theme` replaces
//! threading a palette.
//!
//! This generalises [`super::components::ChipSurface`] — which already resolves a
//! single control's fill/text/border from a context value (`Neutral` vs
//! `Tonal(c)`) — from one control carrying one value to a *subtree* carrying a
//! small palette. It is build-time resolution (every node still gets a fully
//! resolved [`UIStyle`]); the "inheritance" is a parent deriving a child theme and
//! passing it down, not a paint-time cascade. Scoped to the inspector / drawer
//! today (the first consumer), but nothing here is drawer-specific — any surface
//! can build `Theme::INSPECTOR.with_accent(..)` or define its own base.

use crate::color;
use crate::node::{Color32, TextAlign, UIStyle};
use crate::slider::SliderColors;

/// The resolved palette for a build subtree.
///
/// - `accent` — the subtree's identity colour (the CSS `--acc`). Slider fills,
///   selected options, and the accent spine resolve to this.
/// - `surface` — the panel fill this subtree sits on. A derived theme
///   ([`Theme::tinted`]) sets this to a dark tint of the accent.
/// - `text` — the body text colour that reads on `surface`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Theme {
    pub accent: Color32,
    pub surface: Color32,
    pub text: Color32,
}

impl Theme {
    /// The inspector's base theme — the one accent ([`color::INSPECTOR_ACCENT`]),
    /// the panel surface, off-white text. A drawer derives from this:
    /// `Theme::INSPECTOR.with_accent(SOURCE).tinted()`.
    pub const INSPECTOR: Theme = Theme {
        accent: color::INSPECTOR_ACCENT,
        surface: color::BG_1,
        text: color::TEXT_NORMAL,
    };

    /// Re-key the accent, leaving the surface and text untouched. This is the
    /// `--acc` override: a parent keyed to its source (envelope orange, driver
    /// teal, audio green, Ableton purple) hands the derived theme to its children.
    pub fn with_accent(self, accent: Color32) -> Theme {
        Theme { accent, ..self }
    }

    /// Derive a *tinted-dark* theme from the accent: the surface becomes a dark
    /// tint of the accent (a hint of the source colour over near-black), and text
    /// goes white to read on it. This is the modulation-drawer look — a dark zone
    /// that wears its source's colour, not a neutral grey slab.
    ///
    /// The tint mixes 18% accent into [`color::BG_0`] (the app void), matching the
    /// approved mock's `color-mix(in srgb, accent 18%, #0a0c0f)`.
    pub fn tinted(self) -> Theme {
        Theme {
            accent: self.accent,
            surface: color::mix(color::BG_0, self.accent, 0.18),
            text: color::TEXT_WHITE_C32,
        }
    }

    /// Style for this theme's panel surface at `radius` — the bg fill, no border
    /// (grouping comes from the fill tint, not a box or a spine).
    pub fn surface_style(self, radius: f32) -> UIStyle {
        UIStyle {
            bg_color: self.surface,
            corner_radius: radius,
            ..UIStyle::default()
        }
    }

    /// Slider colours for a slider *inside* this theme: the dark recessed well, a
    /// fill in the theme's accent (so the slider belongs to its source), white
    /// thumb, and the theme's text colour for label + value.
    pub fn slider_colors(self) -> SliderColors {
        SliderColors {
            track: color::SLIDER_TRACK_C32,
            track_hover: color::SLIDER_TRACK_HOVER_C32,
            track_pressed: color::SLIDER_TRACK_PRESSED_C32,
            fill: self.accent,
            thumb: color::SLIDER_THUMB_C32,
            text: self.text,
        }
    }

    /// Style for a segmented option cell in this theme: `selected` fills with the
    /// accent (near-black text for contrast); idle recesses to a dark well with
    /// bright text. Borderless — the wells read against the tinted surface on their
    /// own, and a row of eleven outlined cells would read busy (the approved mock
    /// drops the outline here, the one dense-surface exception to the kit's
    /// one-control-outline rule).
    pub fn option_style(self, selected: bool, font: u16) -> UIStyle {
        if selected {
            UIStyle {
                bg_color: self.accent,
                hover_bg_color: color::lighten(self.accent, 24),
                pressed_bg_color: color::darken(self.accent, 16),
                text_color: color::contrast_text_color(self.accent),
                font_size: font,
                corner_radius: color::CHIP_RADIUS,
                text_align: TextAlign::Center,
                ..UIStyle::default()
            }
        } else {
            UIStyle {
                bg_color: color::SLIDER_TRACK_C32,
                hover_bg_color: color::SLIDER_TRACK_HOVER_C32,
                pressed_bg_color: color::SLIDER_TRACK_PRESSED_C32,
                text_color: color::TEXT_NORMAL,
                font_size: font,
                corner_radius: color::CHIP_RADIUS,
                text_align: TextAlign::Center,
                ..UIStyle::default()
            }
        }
    }

    /// Left-aligned label style in this theme's text colour.
    pub fn label_style(self, font: u16) -> UIStyle {
        UIStyle {
            text_color: self.text,
            font_size: font,
            text_align: TextAlign::Left,
            ..UIStyle::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inspector_base_is_the_one_accent_on_the_panel_surface() {
        assert_eq!(Theme::INSPECTOR.accent, color::INSPECTOR_ACCENT);
        assert_eq!(Theme::INSPECTOR.surface, color::BG_1);
        assert_eq!(Theme::INSPECTOR.text, color::TEXT_NORMAL);
    }

    #[test]
    fn with_accent_swaps_only_the_accent() {
        let t = Theme::INSPECTOR.with_accent(color::ENVELOPE_ACTIVE_C32);
        assert_eq!(t.accent, color::ENVELOPE_ACTIVE_C32);
        // Surface + text are inherited unchanged — this is just the `--acc` override.
        assert_eq!(t.surface, Theme::INSPECTOR.surface);
        assert_eq!(t.text, Theme::INSPECTOR.text);
    }

    #[test]
    fn tinted_derives_a_dark_source_tint_and_white_text() {
        let t = Theme::INSPECTOR.with_accent(color::AUDIO_TRIM_BAR_C32).tinted();
        // Accent survives the tint; surface is the 18%-accent-over-void mix; text white.
        assert_eq!(t.accent, color::AUDIO_TRIM_BAR_C32);
        assert_eq!(t.surface, color::mix(color::BG_0, color::AUDIO_TRIM_BAR_C32, 0.18));
        assert_eq!(t.text, color::TEXT_WHITE_C32);
        // The tint is genuinely dark (it sits over the void) but carries the hue —
        // its green channel leads, distinct from a neutral grey.
        assert!(t.surface.g > t.surface.r && t.surface.g > t.surface.b);
    }

    #[test]
    fn each_source_tints_to_its_own_surface() {
        // The bug the mock had (one baked `:root` tint for all) can't recur: the
        // surface derives per-accent, so two sources never share a surface.
        let env = Theme::INSPECTOR.with_accent(color::ENVELOPE_ACTIVE_C32).tinted();
        let aud = Theme::INSPECTOR.with_accent(color::AUDIO_TRIM_BAR_C32).tinted();
        assert_ne!(env.surface, aud.surface);
    }

    #[test]
    fn slider_fill_is_the_accent() {
        let t = Theme::INSPECTOR.with_accent(color::ENVELOPE_ACTIVE_C32).tinted();
        let s = t.slider_colors();
        assert_eq!(s.fill, color::ENVELOPE_ACTIVE_C32);
        assert_eq!(s.track, color::SLIDER_TRACK_C32);
        assert_eq!(s.text, color::TEXT_WHITE_C32);
    }

    #[test]
    fn option_selected_fills_accent_idle_recesses_to_a_well() {
        let t = Theme::INSPECTOR.with_accent(color::DRIVER_ACTIVE_C32).tinted();
        let on = t.option_style(true, color::FONT_BODY);
        assert_eq!(on.bg_color, color::DRIVER_ACTIVE_C32);
        let off = t.option_style(false, color::FONT_BODY);
        assert_eq!(off.bg_color, color::SLIDER_TRACK_C32);
        assert_ne!(off.bg_color, on.bg_color);
    }

    #[test]
    fn label_and_surface_read_from_the_theme() {
        let t = Theme::INSPECTOR.with_accent(color::ABL_BADGE_C32).tinted();
        assert_eq!(t.label_style(color::FONT_BODY).text_color, color::TEXT_WHITE_C32);
        assert_eq!(t.surface_style(color::CARD_RADIUS).bg_color, t.surface);
    }
}
