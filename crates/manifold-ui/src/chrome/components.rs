//! Component vocabulary — a small typed set of controls built on the design
//! tokens ([`crate::color`], the `DESIGN TOKENS` banner) and the [`View`]
//! builder.
//!
//! The problem this fixes: every panel hand-assembles toggles / buttons /
//! segment cells from raw [`UIStyle`] blocks (see the scattered `*_btn_style`
//! helpers in `panels::param_slider_shared`), so the *same* control drifts in
//! colour and shape across the inspector. These constructors are the one place
//! that look lives — change a token, every control moves together.
//!
//! Each component comes in two forms, because the runtime has two write paths:
//!   * `*_style(state) -> UIStyle` for the **in-place update** path
//!     (`UITree::set_style` on an already-materialised node), and
//!   * `*(...) -> View` for the declarative **build** path.
//!
//! The constructors set appearance + the `interactive` flag only. The caller
//! attaches sizing (`.fixed(..)`), identity (`.key(..)`), and routing — either a
//! Chrome intent (`.on_click(..)`) or `.inert()` for key-routed clicks. A
//! component `View` with neither will (correctly) trip
//! [`super::view::validate`]; that is the "you forgot to wire it" guard, not a
//! bug in the component.
//!
//! Rollout: Phase 5 assembles the Edge Detect card from these; Phase 6 replaces
//! the scattered `*_btn_style` helpers with them. Until then both coexist — the
//! old helpers stay in use and these are their token-based successors.

use crate::chrome::view::View;
use crate::color;
use crate::node::{Color32, TextAlign, UIStyle};

// ── Toggle ──────────────────────────────────────────────────────────
// Binary on/off: `ON`/`Inv`/`Delta`, mute, solo. Bold accent when on, neutral
// control grey when off — shape *and* colour both say the state.

pub fn toggle_style(on: bool) -> UIStyle {
    if on {
        UIStyle {
            bg_color: color::ACCENT_BLUE_C32,
            hover_bg_color: color::ACCENT_BLUE_HOVER_C32,
            pressed_bg_color: color::ACCENT_BLUE_PRESS_C32,
            text_color: color::TEXT_WHITE_C32,
            font_size: color::FONT_CAPTION,
            corner_radius: color::BUTTON_RADIUS,
            text_align: TextAlign::Center,
            ..UIStyle::default()
        }
    } else {
        UIStyle {
            bg_color: color::BG_3,
            hover_bg_color: color::BG_3_HOVER,
            pressed_bg_color: color::BG_3_PRESSED,
            text_color: color::TEXT_DIMMED_C32,
            font_size: color::FONT_CAPTION,
            corner_radius: color::BUTTON_RADIUS,
            text_align: TextAlign::Center,
            ..UIStyle::default()
        }
    }
}

/// A toggle button showing `label`, styled by [`toggle_style`]. Size + wire it:
/// `components::toggle("ON", on).fixed(28.0, 16.0).key(K).inert()`.
pub fn toggle(label: impl Into<String>, on: bool) -> View {
    View::button(label).style(toggle_style(on))
}

// ── Button (primary / secondary) ────────────────────────────────────
// Primary = the one bold accent action (`Change`, a dialog's confirm).
// Secondary = neutral control grey (everything else). One accent, used
// sparingly — that is what keeps it meaning "this is the action".

pub fn button_primary_style() -> UIStyle {
    UIStyle {
        bg_color: color::ACCENT_BLUE_C32,
        hover_bg_color: color::ACCENT_BLUE_HOVER_C32,
        pressed_bg_color: color::ACCENT_BLUE_PRESS_C32,
        text_color: color::TEXT_WHITE_C32,
        font_size: color::FONT_BODY,
        corner_radius: color::BUTTON_RADIUS,
        text_align: TextAlign::Center,
        ..UIStyle::default()
    }
}

pub fn button_secondary_style() -> UIStyle {
    UIStyle {
        bg_color: color::BG_3,
        hover_bg_color: color::BG_3_HOVER,
        pressed_bg_color: color::BG_3_PRESSED,
        text_color: color::TEXT_NORMAL,
        font_size: color::FONT_BODY,
        corner_radius: color::BUTTON_RADIUS,
        text_align: TextAlign::Center,
        ..UIStyle::default()
    }
}

pub fn button_primary(text: impl Into<String>) -> View {
    View::button(text).style(button_primary_style())
}

pub fn button_secondary(text: impl Into<String>) -> View {
    View::button(text).style(button_secondary_style())
}

// ── IconButton ──────────────────────────────────────────────────────
// Glyph-only: hamburger, chevron, cog, reset. No fill at rest, a faint overlay
// on hover/press — so a row of icons reads as quiet until touched.

pub fn icon_button_style() -> UIStyle {
    UIStyle {
        bg_color: Color32::TRANSPARENT,
        hover_bg_color: color::HOVER_OVERLAY,
        pressed_bg_color: color::PRESS_OVERLAY,
        text_color: color::CHEVRON_COLOR,
        font_size: color::FONT_BODY,
        corner_radius: color::BUTTON_RADIUS,
        text_align: TextAlign::Center,
        ..UIStyle::default()
    }
}

/// A transparent glyph button (`\u{2261}` menu, `\u{25B6}`/`\u{25BC}` chevron, …).
pub fn icon_button(glyph: impl Into<String>) -> View {
    View::button(glyph).style(icon_button_style())
}

// ── SegmentedControl ────────────────────────────────────────────────
// A connected row of mutually-exclusive cells: the inspector tabs
// (Clip/Layer/Master), or any param flipped *live* (one click, no menu).
// Selected = raised onto the control level + bright text; the rest sit at panel
// level, dimmed. Compose a row of [`segment`] cells:
//
// ```ignore
// View::row(2.0).fill_w().children(
//     tabs.iter().map(|(label, sel)| components::segment(label, *sel).fill_w().h(Sizing::Fixed(24.0)).key(k).inert()),
// )
// ```

pub fn segment_style(selected: bool) -> UIStyle {
    if selected {
        UIStyle {
            bg_color: color::BG_3,
            hover_bg_color: color::BG_3_HOVER,
            pressed_bg_color: color::BG_3_PRESSED,
            text_color: color::TEXT_NORMAL,
            font_size: color::FONT_SUBHEADING,
            corner_radius: color::BUTTON_RADIUS,
            text_align: TextAlign::Center,
            ..UIStyle::default()
        }
    } else {
        UIStyle {
            bg_color: color::BG_1,
            hover_bg_color: color::BG_2,
            pressed_bg_color: color::BG_1,
            text_color: color::TEXT_DIMMED_C32,
            font_size: color::FONT_SUBHEADING,
            corner_radius: color::BUTTON_RADIUS,
            text_align: TextAlign::Center,
            ..UIStyle::default()
        }
    }
}

/// One cell of a [segmented control](self#segmentedcontrol).
pub fn segment(label: impl Into<String>, selected: bool) -> View {
    View::button(label).style(segment_style(selected))
}

// ── Dropdown trigger ────────────────────────────────────────────────
// The cell that shows the current selection and opens a `DropdownPanel` on
// click. Default for option pickers (Source / Feature / Band / Mode) — reduces
// clutter and scales past a handful of choices. The trailing `\u{25BE}` is the
// "opens a list" affordance.

pub fn dropdown_trigger_style(font_size: u16) -> UIStyle {
    UIStyle {
        bg_color: color::BG_3,
        hover_bg_color: color::BG_3_HOVER,
        pressed_bg_color: color::BG_3_PRESSED,
        text_color: color::TEXT_NORMAL,
        font_size,
        corner_radius: color::BUTTON_RADIUS,
        text_align: TextAlign::Left,
        ..UIStyle::default()
    }
}

/// A dropdown trigger showing `text` plus a trailing chevron affordance.
pub fn dropdown_trigger(text: impl AsRef<str>, font_size: u16) -> View {
    View::button(format!("{}  \u{25BE}", text.as_ref())).style(dropdown_trigger_style(font_size))
}

// ── ParamRow pieces ─────────────────────────────────────────────────
// The full param row (label · slider · value · badge · reset) is assembled in
// Phase 5 on the Edge Detect prototype, because it has to thread the live
// slider materialisation + drag state that lives in `param_card`. These are the
// row's reusable trailing-column atoms, defined here so that assembly is just
// composition.

/// Reset (`\u{21BA}`) — a fixed right-column control, same spot every row
/// (Resolve pattern). An icon button by another name.
pub fn reset_button() -> View {
    View::button("\u{21BA}").style(icon_button_style())
}

/// Modulation glance badge — a small dot on a collapsed row: filled accent when
/// the param is modulated (armed), a faint dot when idle. Non-interactive.
/// Per-source colouring (driver teal / envelope orange / audio green) is a
/// Phase 5 detail; this is the binary armed/idle glance.
pub fn mod_badge(armed: bool) -> View {
    View::panel().fixed(6.0, 6.0).radius(color::BUTTON_RADIUS).bg(if armed {
        color::ACCENT_BLUE_C32
    } else {
        color::STATUS_DOT_INACTIVE
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chrome::view::validate;
    use crate::node::UINodeType;

    #[test]
    fn toggle_uses_accent_when_on_and_ramp_when_off() {
        assert_eq!(toggle_style(true).bg_color, color::ACCENT_BLUE_C32);
        assert_eq!(toggle_style(false).bg_color, color::BG_3);
    }

    #[test]
    fn segment_selected_is_raised_above_unselected() {
        assert_eq!(segment_style(true).bg_color, color::BG_3);
        assert_eq!(segment_style(false).bg_color, color::BG_1);
    }

    #[test]
    fn button_tiers_differ() {
        assert_eq!(button_primary_style().bg_color, color::ACCENT_BLUE_C32);
        assert_eq!(button_secondary_style().bg_color, color::BG_3);
        assert_ne!(
            button_primary_style().bg_color,
            button_secondary_style().bg_color
        );
    }

    #[test]
    fn icon_button_transparent_at_rest_overlay_on_hover() {
        let s = icon_button_style();
        assert_eq!(s.bg_color, Color32::TRANSPARENT);
        assert_eq!(s.hover_bg_color, color::HOVER_OVERLAY);
    }

    #[test]
    fn constructors_are_interactive_and_must_be_wired() {
        // A component button is interactive; without intent/inert it warns —
        // documenting the "caller wires it" contract.
        let v = toggle("ON", true);
        assert_eq!(v.kind, UINodeType::Button);
        assert_eq!(validate(&v).len(), 1, "unwired component must warn");
        // …and is silent once wired (here: key-routed, so inert).
        assert!(validate(&toggle("ON", true).inert()).is_empty());
    }

    #[test]
    fn dropdown_trigger_carries_chevron_affordance() {
        let v = dropdown_trigger("Mode", color::FONT_BODY);
        assert!(v.text.as_deref().unwrap().contains('\u{25BE}'));
        assert!(validate(&v.inert()).is_empty());
    }

    #[test]
    fn mod_badge_is_a_non_interactive_dot() {
        assert!(validate(&mod_badge(true)).is_empty());
        assert_eq!(mod_badge(true).kind, UINodeType::Panel);
    }
}
