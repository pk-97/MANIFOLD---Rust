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

// ── StateButton ─────────────────────────────────────────────────────
// A standalone latching/momentary button that carries a *semantic* colour when
// active (transport PLAY=green, REC=red; mixer Mute/Solo/LED/Analysis) and a
// neutral raised chip when off. The active hue is the caller's — ramp aliases
// for transport, the deliberate M/S/L/A identity quartet for the mixer (see the
// `color` carve-outs) — so this fixes the *mechanic* in one place (on = filled
// + `lighten(30)` hover / `darken(20)` press; off = `BUTTON_DIM` chip) without
// dictating the hue. [`toggle`] is the special case where that hue is the accent
// and the off-state recesses to `BG_3` instead of raising to a chip.
//
// Callers needing a non-default font/radius spread over it, like the footer:
// `UIStyle { font_size: F, corner_radius: R, ..state_button_style(c, on) }`.
// Denser surfaces (the inspector cards) pick a [`StateButtonSkin`] instead of
// spreading — same mechanic, different chip + deltas.

/// The visual *skin* of a state button: density (corner radius), the active
/// interaction deltas (how far hover lightens / press darkens the caller's hue),
/// and the neutral off-chip. The **mechanic** is identical across skins — active
/// fills with the caller's hue, off sits on a neutral chip — so a skin is only
/// the handful of constants that differ between the chrome bars and the denser
/// inspector cards. `font_size` is *not* a skin field: the card config buttons
/// size per-caller (effect card 8, gen param 10), so it is always passed in.
pub struct StateButtonSkin {
    /// Hover lightens the active hue by this much (saturating per channel).
    pub active_lighten: u8,
    /// Press darkens the active hue by this much (saturating per channel).
    pub active_darken: u8,
    pub off_bg: Color32,
    pub off_hover: Color32,
    pub off_press: Color32,
    pub off_text: Color32,
    pub corner_radius: f32,
    /// Hairline edge drawn on the chip in BOTH states (active + off). Lets a
    /// chip separate from a coloured surface behind it (the layer-header chips on
    /// an identity-coloured header). Transparent / `0.0` = no border, the chrome
    /// + inspector default.
    pub border_color: Color32,
    pub border_width: f32,
}

impl StateButtonSkin {
    /// Chrome bars (transport / mixer / footer): a bright raised chip with white
    /// off-text and the bold 30/20 active deltas. The default [`state_button`].
    pub const CHROME: Self = Self {
        active_lighten: 30,
        active_darken: 20,
        off_bg: color::BUTTON_DIM,
        off_hover: color::BUTTON_HIGHLIGHTED,
        off_press: color::BUTTON_PRESSED,
        off_text: color::TEXT_WHITE_C32,
        corner_radius: color::BUTTON_RADIUS,
        border_color: Color32::TRANSPARENT,
        border_width: 0.0,
    };

    /// Layer-header chip (§C / §K): a control sitting on the identity-coloured
    /// header. A dark NEUTRAL chip + a white hairline so it reads on any hue,
    /// filling with the caller's M/S/L/A identity hue when active. The bold 30/20
    /// active deltas match the chrome bar; the hairline is what's new.
    pub const HEADER_CHIP: Self = Self {
        active_lighten: 30,
        active_darken: 20,
        off_bg: color::CHIP_BG,
        off_hover: color::CHIP_BG_HOVER,
        off_press: color::CHIP_BG_PRESSED,
        off_text: color::TEXT_WHITE_C32,
        corner_radius: color::CHIP_RADIUS,
        border_color: color::CHIP_LINE,
        border_width: 1.0,
    };

    /// Inspector card, *raised*: the modulation-source buttons (envelope /
    /// driver / audio). A dim raised chip + dimmed off-text, with gentler 20/10
    /// active deltas tuned for the denser card.
    pub const CARD_RAISED: Self = Self {
        active_lighten: 20,
        active_darken: 10,
        off_bg: color::DRIVER_INACTIVE_C32,
        off_hover: color::DRIVER_INACTIVE_HOVER_C32,
        off_press: color::DRIVER_INACTIVE_PRESS_C32,
        off_text: color::TEXT_DIMMED_C32,
        corner_radius: color::SMALL_RADIUS,
        border_color: Color32::TRANSPARENT,
        border_width: 0.0,
    };

    /// Inspector card, *recessed*: the dense config option cells (beat div,
    /// waveform, dot, triplet, reverse). The off-chip sits *below* panel level —
    /// a darker recessed cell — with the same gentle 20/10 active deltas.
    pub const CARD_RECESSED: Self = Self {
        active_lighten: 20,
        active_darken: 10,
        off_bg: color::CONFIG_BTN_INACTIVE_C32,
        off_hover: color::CONFIG_BTN_HOVER_C32,
        off_press: color::CONFIG_BTN_PRESSED_C32,
        off_text: color::TEXT_DIMMED_C32,
        corner_radius: color::SMALL_RADIUS,
        border_color: Color32::TRANSPARENT,
        border_width: 0.0,
    };
}

/// The state-button mechanic with an explicit [`StateButtonSkin`]: active fills
/// with `active_color` (hover/press derived from it via the skin's deltas), off
/// sits on the skin's neutral chip. [`state_button_style`] is the `CHROME`
/// application; the inspector-card helpers in `panels::param_slider_shared`
/// apply `CARD_RAISED` / `CARD_RECESSED`.
pub fn state_button_skinned(
    active_color: Color32,
    active: bool,
    font_size: u16,
    skin: &StateButtonSkin,
) -> UIStyle {
    if active {
        UIStyle {
            bg_color: active_color,
            hover_bg_color: color::lighten(active_color, skin.active_lighten),
            pressed_bg_color: color::darken(active_color, skin.active_darken),
            text_color: color::TEXT_WHITE_C32,
            border_color: skin.border_color,
            border_width: skin.border_width,
            font_size,
            corner_radius: skin.corner_radius,
            text_align: TextAlign::Center,
            ..UIStyle::default()
        }
    } else {
        UIStyle {
            bg_color: skin.off_bg,
            hover_bg_color: skin.off_hover,
            pressed_bg_color: skin.off_press,
            text_color: skin.off_text,
            border_color: skin.border_color,
            border_width: skin.border_width,
            font_size,
            corner_radius: skin.corner_radius,
            text_align: TextAlign::Center,
            ..UIStyle::default()
        }
    }
}

pub fn state_button_style(active_color: Color32, active: bool) -> UIStyle {
    state_button_skinned(
        active_color,
        active,
        color::FONT_BODY,
        &StateButtonSkin::CHROME,
    )
}

/// A state button showing `label`, filled with `active_color` when `active` and
/// a neutral chip otherwise. Size + wire it like any component.
pub fn state_button(label: impl Into<String>, active_color: Color32, active: bool) -> View {
    View::button(label).style(state_button_style(active_color, active))
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

// ── Panel state (empty / error / loading) ───────────────────────────
// One look for "there's nothing to show here": a centered line, dimmed for an
// empty or loading panel and tinted to the status red for an error. Replaces the
// per-panel hand-rolled "Select a …" labels so every empty / error / loading
// state reads as deliberate rather than missing. Text-only by intent — the
// reference DAWs (Ableton, Resolve) place a quiet line here, not an illustration.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PanelStateKind {
    /// Nothing selected / nothing here yet — a neutral dimmed hint.
    Empty,
    /// Something went wrong — the line tints to the status red.
    Error,
    /// Work in flight — a neutral dimmed line. No spinner: the app redraws on
    /// completion, so restraint wins over decorative idle motion (§19).
    Loading,
}

impl PanelStateKind {
    fn text_color(self) -> Color32 {
        match self {
            PanelStateKind::Empty | PanelStateKind::Loading => color::TEXT_DIMMED_C32,
            PanelStateKind::Error => color::STATUS_BAD,
        }
    }
}

/// Label style for an empty / error / loading message: one centered line, dimmed
/// or error-tinted. Imperative callers pass this to `UITree::add_label`;
/// declarative callers use [`panel_state`].
pub fn panel_state_style(kind: PanelStateKind) -> UIStyle {
    UIStyle {
        text_color: kind.text_color(),
        font_size: color::FONT_BODY,
        text_align: TextAlign::Center,
        ..UIStyle::default()
    }
}

/// A centered empty / error / loading message line. Size + wire it (usually
/// `.inert()`): `components::panel_state("Select a node", Empty).fill().inert()`.
pub fn panel_state(message: impl Into<String>, kind: PanelStateKind) -> View {
    View::label(message).style(panel_state_style(kind))
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
    fn state_button_fills_with_hue_when_active_and_chips_when_off() {
        // Active fills with the caller's hue; off ignores it for a neutral chip.
        let on = state_button_style(color::MUTED_COLOR, true);
        assert_eq!(on.bg_color, color::MUTED_COLOR);
        assert_eq!(on.hover_bg_color, color::lighten(color::MUTED_COLOR, 30));
        assert_eq!(on.pressed_bg_color, color::darken(color::MUTED_COLOR, 20));
        let off = state_button_style(color::MUTED_COLOR, false);
        assert_eq!(off.bg_color, color::BUTTON_DIM);
    }

    #[test]
    fn skins_share_the_mechanic_active_fills_hue_off_uses_chip() {
        // Every skin fills with the caller's hue when active (hover/press derived
        // by its own deltas) and falls back to its own neutral chip when off —
        // only the constants differ, the mechanic is one.
        for skin in [
            &StateButtonSkin::CHROME,
            &StateButtonSkin::CARD_RAISED,
            &StateButtonSkin::CARD_RECESSED,
        ] {
            let on = state_button_skinned(color::MUTED_COLOR, true, color::FONT_BODY, skin);
            assert_eq!(on.bg_color, color::MUTED_COLOR);
            assert_eq!(
                on.hover_bg_color,
                color::lighten(color::MUTED_COLOR, skin.active_lighten)
            );
            assert_eq!(
                on.pressed_bg_color,
                color::darken(color::MUTED_COLOR, skin.active_darken)
            );
            let off = state_button_skinned(color::MUTED_COLOR, false, color::FONT_BODY, skin);
            assert_eq!(off.bg_color, skin.off_bg);
            assert_ne!(off.bg_color, color::MUTED_COLOR);
        }
    }

    #[test]
    fn card_raised_skin_reproduces_legacy_de_button_constants() {
        // Parity: the modulation-source button look is unchanged by the kit move
        // (gentle 20/10 deltas, the dim raised chip, dimmed off-text).
        let s = &StateButtonSkin::CARD_RAISED;
        assert_eq!((s.active_lighten, s.active_darken), (20, 10));
        let off = state_button_skinned(color::ENVELOPE_ACTIVE_C32, false, color::FONT_CAPTION, s);
        assert_eq!(off.bg_color, color::DRIVER_INACTIVE_C32);
        assert_eq!(off.hover_bg_color, color::DRIVER_INACTIVE_HOVER_C32);
        assert_eq!(off.pressed_bg_color, color::DRIVER_INACTIVE_PRESS_C32);
        assert_eq!(off.text_color, color::TEXT_DIMMED_C32);
        assert_eq!(off.corner_radius, color::SMALL_RADIUS);
        let on = state_button_skinned(color::ENVELOPE_ACTIVE_C32, true, color::FONT_CAPTION, s);
        assert_eq!(on.font_size, color::FONT_CAPTION);
        assert_eq!(on.text_color, color::TEXT_WHITE_C32);
    }

    #[test]
    fn card_recessed_skin_reproduces_legacy_config_off_and_active_hover() {
        // Parity: off-chip + active hover unchanged. The active *press* is now
        // derived (darken 10) for consistency with the colored config variant —
        // the one deliberate, sub-perceptual delta vs the old hand-tuned constant
        // (the old press was a non-uniform −10/−20/−20 that no `darken` reproduces).
        let s = &StateButtonSkin::CARD_RECESSED;
        let off = state_button_skinned(color::DRIVER_ACTIVE_C32, false, color::FONT_CAPTION, s);
        assert_eq!(off.bg_color, color::CONFIG_BTN_INACTIVE_C32);
        assert_eq!(off.hover_bg_color, color::CONFIG_BTN_HOVER_C32);
        assert_eq!(off.pressed_bg_color, color::CONFIG_BTN_PRESSED_C32);
        let on = state_button_skinned(color::DRIVER_ACTIVE_C32, true, color::FONT_CAPTION, s);
        // Hover still equals the old DRIVER_ACTIVE_HOVER constant value…
        assert_eq!(on.hover_bg_color, Color32::new(40, 186, 211, 255));
        // …press is now the derived darken(10), the consistency fix.
        assert_eq!(
            on.pressed_bg_color,
            color::darken(color::DRIVER_ACTIVE_C32, 10)
        );
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

    #[test]
    fn panel_state_dims_empty_and_loading_tints_error_and_centers() {
        assert_eq!(
            panel_state_style(PanelStateKind::Empty).text_color,
            color::TEXT_DIMMED_C32
        );
        assert_eq!(
            panel_state_style(PanelStateKind::Loading).text_color,
            color::TEXT_DIMMED_C32
        );
        assert_eq!(
            panel_state_style(PanelStateKind::Error).text_color,
            color::STATUS_BAD
        );
        // Centered, so it reads as a deliberate placeholder, not a stray label.
        assert_eq!(
            panel_state_style(PanelStateKind::Empty).text_align,
            TextAlign::Center
        );
        // The View carries the message and is a label.
        let v = panel_state("Select a node", PanelStateKind::Empty);
        assert_eq!(v.kind, UINodeType::Label);
        assert_eq!(v.text.as_deref(), Some("Select a node"));
    }
}
