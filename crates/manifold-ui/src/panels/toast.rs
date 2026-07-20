//! D11 undo/redo toast (`UI_CRAFT_AND_MOTION_PLAN.md` §4 P2) — a transient
//! bottom-center message eased in over `MOTION_SLOW`, held ~1.4s, faded out
//! over `MOTION_SLOW`. Reduces to one `Transient` (D3's one-shot-timed-event
//! piece — the anim.rs doc-comment names "toast" explicitly as a `Transient`
//! use case) whose `progress()` is turned into a three-segment alpha curve
//! (ramp in / hold / ramp out) instead of a bespoke second mechanism.
//!
//! Hosted as a first-class [`Overlay`] (`Modeless`, click-through, so it never
//! steals input from whatever is under it; `SelfManaged` anchor, computed from
//! the screen size the driver hands `build_at`) rather than a bespoke node —
//! see `docs/OVERLAY_SYSTEM_DESIGN.md`. `is_open()` mirrors the `Transient`
//! directly: once it finishes, the overlay driver's existing
//! open/close-edge detection (`detect_overlay_open_change`) tears the toast's
//! nodes down on the next overlay rebuild, exactly like every other overlay's
//! close path — no bespoke teardown code here.
//!
//! One slot, "latest wins" (the doc's explicit anti-queueing rule): calling
//! [`ToastPanel::show`] while a toast is already showing cuts it short and
//! replaces it — it is never queued behind the first.

use super::overlay::{Anchor, Modality, Overlay, OverlayPlacement, OverlayResponse};
use crate::anim::Transient;
use crate::color;
use crate::input::UIEvent;
use crate::node::*;
use crate::tree::UITree;
use std::time::Instant;

/// Ease-in duration — D1's general `MOTION_SLOW` (240ms), same token the plan
/// names for "value flash, toast".
const ENTER_MS: f32 = color::MOTION_SLOW_MS;
/// Fully-visible hold, per D11 ("~1.4s hold").
const HOLD_MS: f32 = 1400.0;
/// Fade-out duration — same `MOTION_SLOW` token as the entrance.
const FADE_MS: f32 = color::MOTION_SLOW_MS;
const TOTAL_MS: f32 = ENTER_MS + HOLD_MS + FADE_MS;

const TOAST_W: f32 = 300.0;
const TOAST_H: f32 = 34.0;
const BOTTOM_MARGIN: f32 = 28.0;
const FONT: u16 = color::FONT_BODY;
const RADIUS: f32 = color::POPUP_RADIUS;

pub struct ToastPanel {
    message: String,
    transient: Transient,
    /// D17 "export-complete green sweep": the accent this toast's text tints
    /// toward at full opacity. `None` = the neutral undo/redo text color.
    accent: Option<Color32>,
    bg_id: Option<NodeId>,
    text_id: Option<NodeId>,
    /// Wall-clock timestamp `update()` last ticked from — the same
    /// self-contained-dt pattern `InspectorCompositePanel`'s
    /// `motion_last_tick` uses, so the `UIRoot::update()` call site needs no
    /// dt of its own to compute or thread through.
    last_tick: Option<Instant>,
}

impl Default for ToastPanel {
    fn default() -> Self {
        Self::new()
    }
}

impl ToastPanel {
    pub fn new() -> Self {
        Self {
            message: String::new(),
            transient: Transient::default(),
            accent: None,
            bg_id: None,
            text_id: None,
            last_tick: None,
        }
    }

    /// Show (or replace) the toast with `message`. One slot: a toast already
    /// in flight is cut short and replaced, never queued (D11 "latest wins").
    pub fn show(&mut self, message: impl Into<String>) {
        self.message = message.into();
        self.accent = None;
        self.transient.fire(TOTAL_MS);
        // Reset so the very next `update()` ticks a small real dt, not the
        // (possibly huge) gap since the last toast.
        self.last_tick = None;
        // Drop the previous nodes' ids: `show()` can fire in the same frame a
        // rebuild already tore the tree down (e.g. undo → toast fires from
        // push_state, after the rebuild ran with the toast still closed), so
        // these ids may already be stale. `build_at` re-mints fresh ones; until
        // then `update()`'s `Some/Some` guard below correctly treats the toast
        // as not-yet-built rather than reading a dead id.
        self.bg_id = None;
        self.text_id = None;
    }

    /// D17 "export-complete green sweep": same one-slot toast, tinted toward
    /// `accent` instead of the neutral text color. Also used for a failed
    /// export in red — still a genuine, distinct-from-undo/redo status event.
    pub fn show_with_accent(&mut self, message: impl Into<String>, accent: Color32) {
        self.message = message.into();
        self.accent = Some(accent);
        self.transient.fire(TOTAL_MS);
        self.last_tick = None;
        // See `show()` — same stale-id reset.
        self.bg_id = None;
        self.text_id = None;
    }

    /// Eased 0..1 alpha for the transient's current progress: ramps 0→1 over
    /// `ENTER_MS`, holds at 1.0, ramps 1→0 over the final `FADE_MS`. `None`
    /// (idle) reads as fully transparent.
    fn alpha(&self) -> f32 {
        let Some(p) = self.transient.progress() else {
            return 0.0;
        };
        let elapsed = p * TOTAL_MS;
        if elapsed < ENTER_MS {
            elapsed / ENTER_MS
        } else if elapsed < ENTER_MS + HOLD_MS {
            1.0
        } else {
            (1.0 - (elapsed - ENTER_MS - HOLD_MS) / FADE_MS).max(0.0)
        }
    }

    /// Advance the transient by real elapsed wall-clock time and repaint the
    /// already-built nodes' alpha in place — no rebuild, no layout change. A
    /// no-op while idle (the overlay driver has already torn the nodes down
    /// via the close edge, exactly like every other overlay's close path).
    /// Call every frame from `UIRoot::update()`, same rail as every other
    /// continuously-live overlay (`update_audio_meters` et al.) — a plain
    /// style write needs no forced-rebuild poll.
    pub fn update(&mut self, tree: &mut UITree) {
        if self.transient.progress().is_none() {
            self.last_tick = None;
            return;
        }
        let now = Instant::now();
        let dt_ms = self
            .last_tick
            .map(|t| (now - t).as_secs_f32() * 1000.0)
            .unwrap_or(0.0)
            .min(100.0);
        self.last_tick = Some(now);
        self.tick(dt_ms);

        let (Some(bg), Some(text)) = (self.bg_id, self.text_id) else {
            return;
        };
        let (Some(mut bg_style), Some(mut text_style)) = (
            tree.get_node(bg).map(|n| n.style),
            tree.get_node(text).map(|n| n.style),
        ) else {
            return;
        };
        let a = self.alpha();
        bg_style.bg_color = with_alpha(color::BG_2, 235.0 * a);
        tree.set_style(bg, bg_style);
        text_style.text_color = with_alpha(self.accent.unwrap_or(color::TEXT_PRIMARY_C32), 255.0 * a);
        tree.set_style(text, text_style);
    }

    /// Advance the underlying transient by `dt_ms`. Split out from `update` so
    /// unit tests can drive timing without building a `UITree`.
    pub fn tick(&mut self, dt_ms: f32) -> bool {
        self.transient.tick(dt_ms)
    }

    /// `EDITOR_WINDOW_UNIFICATION_DESIGN.md` D6: whether this overlay still
    /// needs a per-frame tick to progress (enter/hold/fade) or to detect its
    /// own completion. Named to match the sibling popups' `is_animating()`
    /// (`browser_popup`/`ableton_picker`/`settings_popup`), which the popup
    /// professional pass stubbed permanently `false` (their entrance tweens
    /// are gone) — this one is still live, since the transient keeps
    /// progressing through its hold/fade segments after `show()` fires.
    /// Equivalent to `is_open()` (both read `self.transient.progress()`), but
    /// named for the redraw-keepalive call site's intent, not the overlay
    /// driver's open/close-edge one.
    pub fn is_animating(&self) -> bool {
        self.transient.progress().is_some()
    }
}

fn with_alpha(c: Color32, a: f32) -> Color32 {
    Color32 {
        a: a.clamp(0.0, 255.0) as u8,
        ..c
    }
}

impl Overlay for ToastPanel {
    fn is_open(&self) -> bool {
        self.transient.progress().is_some()
    }

    fn modality(&self) -> Modality {
        // Click-through: a toast never captures input, and never wants
        // dismiss-on-outside-click machinery (D18-adjacent — it disappears on
        // its own timer, never on interaction).
        Modality::Modeless
    }

    fn anchor(&self) -> Anchor {
        // Bottom-center isn't one of the shared `Anchor` corner/center variants
        // (which center on both axes or pin to a screen corner), so this
        // self-positions in `build_at` from the placement's `screen` — the
        // same technique the browser popup / Ableton picker / dropdown already
        // use for placements the shared enum doesn't name.
        Anchor::SelfManaged
    }

    fn desired_size(&self) -> Vec2 {
        Vec2::new(TOAST_W, TOAST_H)
    }

    fn build_at(&mut self, tree: &mut UITree, placement: OverlayPlacement) {
        let x = ((placement.screen.x - TOAST_W) * 0.5).max(0.0);
        let y = (placement.screen.y - TOAST_H - BOTTOM_MARGIN).max(0.0);
        let a = self.alpha();
        self.bg_id = Some(tree.add_panel(
            None,
            x,
            y,
            TOAST_W,
            TOAST_H,
            UIStyle {
                bg_color: with_alpha(color::BG_2, 235.0 * a),
                corner_radius: RADIUS,
                ..UIStyle::default()
            },
        ));
        self.text_id = Some(tree.add_label(
            self.bg_id,
            x,
            y,
            TOAST_W,
            TOAST_H,
            &self.message,
            UIStyle {
                text_color: with_alpha(self.accent.unwrap_or(color::TEXT_PRIMARY_C32), 255.0 * a),
                font_size: FONT,
                text_align: TextAlign::Center,
                ..UIStyle::default()
            },
        ));
    }

    fn on_event(&mut self, _event: &UIEvent, _tree: &mut UITree) -> OverlayResponse {
        // Click-through: never consumes, so it never blocks the UI beneath it.
        OverlayResponse::Ignored
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_before_any_show() {
        let toast = ToastPanel::new();
        assert!(!toast.is_open());
        assert_eq!(toast.alpha(), 0.0);
    }

    #[test]
    fn show_opens_it_and_ramps_alpha_in() {
        let mut toast = ToastPanel::new();
        toast.show("Undo");
        assert!(toast.is_open());
        assert_eq!(toast.alpha(), 0.0, "just fired — at the very start of the ramp-in");

        toast.tick(ENTER_MS * 0.5);
        let mid = toast.alpha();
        assert!(mid > 0.0 && mid < 1.0, "mid ramp-in: {mid}");

        toast.tick(ENTER_MS * 0.5);
        assert!((toast.alpha() - 1.0).abs() < 1e-3, "fully entered");
    }

    #[test]
    fn holds_at_full_alpha_then_fades_and_closes() {
        let mut toast = ToastPanel::new();
        toast.show("Redo");
        toast.tick(ENTER_MS); // entered
        toast.tick(HOLD_MS * 0.5);
        assert_eq!(toast.alpha(), 1.0, "mid-hold: fully opaque");
        assert!(toast.is_open());

        toast.tick(HOLD_MS * 0.5); // end of hold
        toast.tick(FADE_MS * 0.5);
        let mid_fade = toast.alpha();
        assert!(mid_fade > 0.0 && mid_fade < 1.0, "mid fade-out: {mid_fade}");
        assert!(toast.is_open(), "still showing until the fade completes");

        toast.tick(FADE_MS * 0.5);
        assert_eq!(toast.alpha(), 0.0);
        assert!(!toast.is_open(), "closed once the whole timeline elapses");
    }

    /// `EDITOR_WINDOW_UNIFICATION_DESIGN.md` D6/P2: `is_animating()` is the
    /// sole live member of `UIRoot::overlay_redraw_needed()`'s aggregate —
    /// this is the "animating overlay ⇒ predicate true" proof the phase
    /// brief asks for, at the panel level (the aggregate itself is a
    /// one-line OR over this).
    #[test]
    fn is_animating_true_while_showing_false_once_idle() {
        let mut toast = ToastPanel::new();
        assert!(!toast.is_animating(), "idle before any show()");

        toast.show("Undo");
        assert!(toast.is_animating(), "just fired — still has enter/hold/fade to run");

        toast.tick(ENTER_MS + HOLD_MS + FADE_MS); // run the whole timeline out
        assert!(!toast.is_animating(), "settled back to idle once the timeline elapses");
    }

    #[test]
    fn showing_again_replaces_rather_than_queues() {
        let mut toast = ToastPanel::new();
        toast.show("Undo: Move Clip");
        toast.tick(ENTER_MS + HOLD_MS * 0.5); // mid-hold
        toast.show("Redo: Move Clip"); // latest wins — restarts the timeline
        assert_eq!(toast.message, "Redo: Move Clip");
        assert_eq!(toast.alpha(), 0.0, "replacing restarts the ramp-in, not the hold");
    }

    /// Reproduces the BUG-028-class crash: undo fires the toast the same frame
    /// a rebuild already shrank the tree (the toast's own nodes weren't built
    /// into it, since it was closed at rebuild time), leaving `bg_id`/`text_id`
    /// pointing one-past-the-end of the new, smaller tree.
    #[test]
    fn re_show_across_shrinking_rebuild_never_panics() {
        let mut tree = UITree::new();
        let placement = OverlayPlacement {
            rect: Rect::new(0.0, 0.0, 300.0, 34.0),
            screen: Vec2::new(1920.0, 1080.0),
        };

        let mut toast = ToastPanel::new();
        toast.show("Undo: Move Clip");
        toast.build_at(&mut tree, placement);
        assert!(tree.get_node(toast.bg_id.unwrap()).is_some());

        // Simulate a rebuild that runs while the toast is closed (idle between
        // shows) and lands a much smaller tree — the toast's old nodes are
        // gone, but `bg_id`/`text_id` still hold the old (now stale) ids.
        toast.tick(TOTAL_MS); // finish the toast, back to idle
        tree.clear();
        tree.add_panel(None, 0.0, 0.0, 10.0, 10.0, UIStyle::default());

        // Undo fires again in the same frame `push_state` runs, before the
        // overlay driver rebuilds the toast's nodes into the new tree.
        toast.show("Redo: Move Clip");
        toast.update(&mut tree); // must not panic
    }

    #[test]
    fn accent_toast_carries_its_color_neutral_toast_does_not() {
        let mut toast = ToastPanel::new();
        toast.show_with_accent("Export complete", color::GREEN_BASE);
        assert_eq!(toast.accent, Some(color::GREEN_BASE));

        toast.show("Undo"); // neutral show() clears any prior accent
        assert_eq!(toast.accent, None);
    }
}
