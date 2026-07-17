//! Input mapping for the P5 interactive 3D viewport
//! (`docs/REALTIME_3D_DESIGN.md` D7, P5 as-built note): translates raw
//! pointer/scroll events into [`manifold_renderer::node_graph::ViewportSession`]
//! calls, per the industry-standard bindings the doc commits to (D7 §7.7):
//! left-drag orbits, shift-drag or middle-drag pans, scroll/pinch dollies.
//!
//! Deliberately winit-agnostic in its inputs — takes resolved primitives
//! (`MouseButton`, a plain `shift_held: bool`, pixel deltas) rather than raw
//! `WindowEvent`s, matching `self.modifiers.shift`'s already-resolved-bool
//! convention in `window_input.rs`. That keeps this module unit-testable
//! without constructing winit types, and keeps the classification logic (the
//! actual design decision — "what does a left-drag mean here") separate from
//! the event-plumbing that calls it (`window_input.rs`).
//!
//! **Wiring status (2026-07-17, P5c):** wired into `window_input.rs` — a
//! press inside the docked viewport rect (`editor_mouse_input`) arms a drag
//! that `editor_cursor_moved` feeds through [`classify_mouse_drag`]/[`apply`]
//! each move, and `editor_mouse_wheel` feeds scroll deltas over the rect
//! through [`classify_scroll`]/[`classify_trackpad_pan`]/
//! [`classify_trackpad_pinch_dolly`] (a `LineDelta` is a physical wheel
//! notch → dolly; a Ctrl-held `PixelDelta` is a trackpad pinch, translated by
//! the OS into a Ctrl-modified scroll absent a native magnify-gesture
//! handler; a bare `PixelDelta` is a two-finger trackpad pan).

use manifold_renderer::node_graph::ViewportSession;
use winit::event::MouseButton;

/// Per-device-family sensitivity constants — the panel owns these (D7: "the
/// panel owns the constant so it can tune per input device", already the
/// convention `ViewportCamera`'s own doc comments state). Mouse and trackpad
/// get separate constants because the two devices report deltas at
/// different natural magnitudes (trackpad two-finger gestures are gentler
/// than a mouse drag for the same felt motion).
#[derive(Debug, Clone, Copy)]
pub struct ViewportInputSensitivity {
    pub orbit: f32,
    pub pan: f32,
    pub dolly: f32,
    pub trackpad_pan: f32,
    pub trackpad_pinch_dolly: f32,
}

impl Default for ViewportInputSensitivity {
    fn default() -> Self {
        Self {
            // Radians per pixel — a full-width drag (~1000px) is a little
            // over half a turn, the usual DCC feel.
            orbit: 0.006,
            // World-units-per-pixel-per-unit-distance (ViewportCamera::pan
            // already scales by distance, so this stays resolution/zoom
            // invariant).
            pan: 0.0025,
            // Per wheel "notch" (matches `window_input.rs`'s existing
            // `LINE_DELTA_PX`-normalized convention for line-delta scroll).
            dolly: 0.08,
            trackpad_pan: 0.0015,
            trackpad_pinch_dolly: 0.05,
        }
    }
}

/// A classified viewport navigation gesture, resolved from raw input by
/// [`classify_mouse_drag`] / [`classify_scroll`] / the trackpad equivalents.
/// `None` means "this input isn't a viewport navigation gesture" (e.g. a
/// right-click, or a button the viewport doesn't bind) — the caller falls
/// through to whatever it would otherwise do with the event.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ViewportGesture {
    Orbit { dx: f32, dy: f32 },
    Pan { dx: f32, dy: f32 },
    Dolly { delta: f32 },
    TrackpadPan { dx: f32, dy: f32 },
    TrackpadPinchDolly { delta: f32 },
}

/// Classify a mouse-drag delta into a navigation gesture per D7's bindings:
/// left-drag orbits; shift-held left-drag OR a middle-drag pans. Any other
/// button (right, forward/back) isn't a viewport gesture — `None`, so the
/// caller's existing right-click handling (context menus etc.) is
/// untouched.
pub fn classify_mouse_drag(button: MouseButton, shift_held: bool, dx: f32, dy: f32) -> Option<ViewportGesture> {
    match button {
        MouseButton::Left if shift_held => Some(ViewportGesture::Pan { dx, dy }),
        MouseButton::Left => Some(ViewportGesture::Orbit { dx, dy }),
        MouseButton::Middle => Some(ViewportGesture::Pan { dx, dy }),
        _ => None,
    }
}

/// Classify a scroll-wheel delta (already normalized to the app's line-delta
/// convention, same units `window_input.rs::normalize_scroll_delta` already
/// produces for the canvas-zoom path) into a dolly gesture.
pub fn classify_scroll(dy: f32) -> ViewportGesture {
    ViewportGesture::Dolly { delta: dy }
}

/// Two-finger trackpad pan gesture (distinct from a mouse middle-drag: same
/// underlying `ViewportCamera::pan` math, different sensitivity constant —
/// see `ViewportInputSensitivity`).
pub fn classify_trackpad_pan(dx: f32, dy: f32) -> ViewportGesture {
    ViewportGesture::TrackpadPan { dx, dy }
}

/// Trackpad pinch-to-zoom gesture.
pub fn classify_trackpad_pinch_dolly(delta: f32) -> ViewportGesture {
    ViewportGesture::TrackpadPinchDolly { delta }
}

/// Apply a classified gesture to `session` — the single call site a
/// viewport panel's input handlers make. Each arm forwards straight to the
/// matching `ViewportSession` method (which itself marks the session dirty
/// via a cheap `Graph::set_param`, never a rebuild — see
/// `viewport_session.rs`).
pub fn apply(session: &mut ViewportSession, gesture: ViewportGesture, sens: &ViewportInputSensitivity) {
    match gesture {
        ViewportGesture::Orbit { dx, dy } => session.orbit(dx, dy, sens.orbit),
        ViewportGesture::Pan { dx, dy } => session.pan(dx, dy, sens.pan),
        ViewportGesture::Dolly { delta } => session.dolly(delta, sens.dolly),
        ViewportGesture::TrackpadPan { dx, dy } => session.trackpad_pan(dx, dy, sens.trackpad_pan),
        ViewportGesture::TrackpadPinchDolly { delta } => {
            session.trackpad_pinch_dolly(delta, sens.trackpad_pinch_dolly)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn left_drag_without_shift_orbits() {
        assert_eq!(
            classify_mouse_drag(MouseButton::Left, false, 10.0, -3.0),
            Some(ViewportGesture::Orbit { dx: 10.0, dy: -3.0 })
        );
    }

    #[test]
    fn shift_left_drag_pans() {
        assert_eq!(
            classify_mouse_drag(MouseButton::Left, true, 4.0, 5.0),
            Some(ViewportGesture::Pan { dx: 4.0, dy: 5.0 })
        );
    }

    #[test]
    fn middle_drag_pans_regardless_of_shift() {
        assert_eq!(
            classify_mouse_drag(MouseButton::Middle, false, 1.0, 1.0),
            Some(ViewportGesture::Pan { dx: 1.0, dy: 1.0 })
        );
        assert_eq!(
            classify_mouse_drag(MouseButton::Middle, true, 1.0, 1.0),
            Some(ViewportGesture::Pan { dx: 1.0, dy: 1.0 })
        );
    }

    #[test]
    fn right_drag_is_not_a_viewport_gesture() {
        assert_eq!(classify_mouse_drag(MouseButton::Right, false, 1.0, 1.0), None);
    }

    #[test]
    fn scroll_and_trackpad_classify_as_dolly_and_pan_variants() {
        assert_eq!(classify_scroll(2.0), ViewportGesture::Dolly { delta: 2.0 });
        assert_eq!(
            classify_trackpad_pan(1.0, 2.0),
            ViewportGesture::TrackpadPan { dx: 1.0, dy: 2.0 }
        );
        assert_eq!(
            classify_trackpad_pinch_dolly(0.5),
            ViewportGesture::TrackpadPinchDolly { delta: 0.5 }
        );
    }

    #[test]
    fn sensitivity_defaults_are_all_positive() {
        let s = ViewportInputSensitivity::default();
        assert!(s.orbit > 0.0 && s.pan > 0.0 && s.dolly > 0.0);
        assert!(s.trackpad_pan > 0.0 && s.trackpad_pinch_dolly > 0.0);
    }
}
