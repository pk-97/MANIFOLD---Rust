//! Overlay system — a uniform lifecycle over the top-level floating surfaces
//! (dropdown, browser popup, Ableton picker, Audio Setup, perf HUD).
//!
//! See `docs/OVERLAY_SYSTEM_DESIGN.md`. The point: every overlay declares its
//! modality, anchor, and size, and exposes build + event hooks, so one driver
//! in the app layer owns build + draw + input for all of them. The render pass
//! and input cascade stop hand-enumerating overlays by name, which is the class
//! of bug that left the Audio Setup panel built-but-never-drawn.
//!
//! `Overlay` is deliberately standalone (not a `Panel` supertrait): the modal
//! panels don't implement `Panel` — they have bespoke build/click APIs — and
//! the driver captures each overlay's node range by bracketing `build_at` with
//! `tree.count()`, so overlays never self-track `first_node`/`node_count`.

use crate::input::UIEvent;
use crate::node::{NodeId, Rect, Vec2};
use crate::tree::UITree;

use super::PanelAction;

/// How an overlay relates to the UI beneath it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Modality {
    /// Captures all input: even an event the overlay `Ignored` does not fall
    /// through to lower overlays / panels. `dim_background` adds a full-screen
    /// scrim node beneath the overlay. The overlay closes itself (sets its open
    /// flag false) on an outside / backdrop click; the driver then pops it.
    Modal { dim_background: bool },
    /// Floats above the UI. An `Ignored` event falls through to lower panels
    /// (so the perf HUD, which never consumes, is click-through). Overlays that
    /// want dismiss-on-outside-click (the dropdown) close themselves inside
    /// `on_event` and return `Consumed`.
    Modeless,
}

/// Screen corner for [`Anchor::Corner`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Corner {
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

/// Where an overlay positions itself. The driver resolves this to a screen rect
/// via [`compute_overlay_rect`], applying edge-clamping once for every overlay.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Anchor {
    /// Centered in the screen (settings-dialog style).
    Centered,
    /// Pinned to a screen corner with a margin (the perf HUD).
    Corner { corner: Corner, margin: f32 },
    /// Top-left pinned to a screen point (click-anchored popups).
    At(Vec2),
    /// Just below a tree node (the driver supplies the node's rect).
    ToNode(NodeId),
    /// An explicit rect, used verbatim (no clamping).
    Fixed(Rect),
    /// The overlay positions itself in `build_at` from `OverlayPlacement.screen`
    /// (content-sized, click-anchored popups that already clamp internally:
    /// browser popup, Ableton picker, dropdown). The driver passes a zero rect.
    SelfManaged,
}

/// How an overlay's on-screen size is determined. Returned by
/// [`Overlay::size_policy`] and resolved by the driver against the screen size
/// *before* centering — so viewport-relative overlays (e.g. a settings modal
/// that fills most of the screen) declare their size here instead of
/// self-positioning inside `build_at`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SizePolicy {
    /// Size from the overlay's intrinsic [`Overlay::desired_size`]
    /// (content-sized popups: dropdown, browser, Ableton picker, perf HUD).
    Content,
    /// A fraction of the screen per axis, floored at `min` logical pixels. The
    /// driver resolves it to `max(screen * frac, min)` componentwise.
    Fraction { frac: Vec2, min: Vec2 },
}

impl SizePolicy {
    /// Resolve to a concrete size. `content` is the overlay's `desired_size()`
    /// (used only by [`SizePolicy::Content`]); `screen` is the full screen size.
    pub fn resolve(&self, screen: Vec2, content: Vec2) -> Vec2 {
        match *self {
            SizePolicy::Content => content,
            SizePolicy::Fraction { frac, min } => Vec2::new(
                (screen.x * frac.x).max(min.x),
                (screen.y * frac.y).max(min.y),
            ),
        }
    }
}

/// What the driver hands an overlay's `build_at`: the resolved placement rect
/// (for placed overlays — `Centered`/`Corner`/`At`/`ToNode`/`Fixed`) and the
/// full screen size (for `SelfManaged` overlays that position themselves).
#[derive(Debug, Clone, Copy)]
pub struct OverlayPlacement {
    pub rect: Rect,
    pub screen: Vec2,
}

/// The result of routing one event to an overlay. Overlays manage their own
/// open/close state; the driver pops any overlay whose `is_open()` flips false.
pub enum OverlayResponse {
    /// Not this overlay's event. Modeless → driver tries lower overlays/panels;
    /// Modal → driver still captures it (no fall-through).
    Ignored,
    /// Consumed; emit these actions (may be empty). The driver stops the walk
    /// and flags an overlay rebuild.
    Consumed(Vec<PanelAction>),
}

/// A top-level floating surface driven uniformly by the app-layer overlay
/// driver. Adding an overlay = implement this + add one `OverlayId` arm; the
/// exhaustive match then forces build/draw/input wiring, so "built but never
/// drawn" stops being expressible.
pub trait Overlay {
    /// Whether the overlay is currently shown.
    fn is_open(&self) -> bool;

    /// Modality (input capture + optional backdrop).
    fn modality(&self) -> Modality;

    /// Where the overlay wants to sit.
    fn anchor(&self) -> Anchor;

    /// How the overlay's size is determined. Defaults to [`SizePolicy::Content`]
    /// (intrinsic [`desired_size`](Self::desired_size)); override to size
    /// relative to the viewport.
    fn size_policy(&self) -> SizePolicy {
        SizePolicy::Content
    }

    /// The overlay's intrinsic size in logical pixels (may depend on content).
    /// Used for [`SizePolicy::Content`]; unused for `SelfManaged` overlays.
    fn desired_size(&self) -> Vec2;

    /// Build the overlay's nodes into `tree`. Only called when `is_open()`.
    fn build_at(&mut self, tree: &mut UITree, placement: OverlayPlacement);

    /// Route one event. The driver walks open overlays top-of-stack first and
    /// stops at the first non-`Ignored` response (or the first Modal). Overlays
    /// manage their own open/close state here (self-close on Escape / outside
    /// click); the driver only reads `is_open()`, so there is no `close()` hook.
    fn on_event(&mut self, event: &UIEvent, tree: &mut UITree) -> OverlayResponse;

    /// Does a drag ORIGINATING at `origin` belong to this overlay? Read by
    /// `UIRoot::resolve_drag_owner` (`docs/DRAG_CAPTURE_DESIGN.md` §3.2) once,
    /// at the gesture's first `DragBegin` — never per-event. Default: no.
    /// Modeless overlays with a drag surface override (the audio panel's
    /// armed band/calibration drag OR origin inside its panel rect). A modal always owns
    /// regardless of this hook (D4); this is only consulted for modeless
    /// overlays.
    fn claims_drag(&self, origin: Vec2) -> bool {
        let _ = origin;
        false
    }

    /// Called once per gesture, unconditionally, when the terminal `DragEnd`/
    /// `PointerUp` broadcasts (`UIRoot::broadcast_gesture_end`, D2) — every
    /// OPEN overlay gets this regardless of whether it owned the gesture, so
    /// it must be idempotent. Default: no-op. Overrides clear any drag state
    /// armed by `claims_drag`/`on_event` (P2: the audio panel's
    /// `DragController<AudioSetupDrag>` session).
    fn gesture_ended(&mut self) {}

    /// Did this overlay just arm an immediate-drag surface (D6,
    /// `docs/DRAG_CAPTURE_DESIGN.md` §3.4) while consuming the `PointerDown`
    /// just routed to it? Read by `UIRoot` once, immediately after
    /// `route_overlay_event` consumes a `PointerDown` — never per-event.
    /// Default: no. Must reflect THIS press, not a stale flag from a
    /// previous gesture (the audio panel returns true iff it just armed a
    /// band-divider grab on the `PointerDown` being routed).
    fn wants_immediate_drag(&self) -> bool {
        false
    }
}

/// Resolve an [`Anchor`] + size to an on-screen rect, clamped so the overlay
/// stays fully visible. One place for the edge-clamp math every overlay used to
/// hand-roll. `anchor_node_rect` is the resolved rect of an [`Anchor::ToNode`]
/// target (the driver looks it up in the tree); ignored for other anchors.
/// `SelfManaged` returns a zero-origin rect — those overlays ignore it.
pub fn compute_overlay_rect(
    anchor: &Anchor,
    size: Vec2,
    screen: Vec2,
    anchor_node_rect: Option<Rect>,
) -> Rect {
    if let Anchor::Fixed(r) = anchor {
        return *r;
    }
    let (mut x, mut y) = match anchor {
        Anchor::Centered => (
            ((screen.x - size.x) * 0.5).max(0.0),
            ((screen.y - size.y) * 0.5).max(0.0),
        ),
        Anchor::Corner { corner, margin } => {
            let m = *margin;
            let x = match corner {
                Corner::TopLeft | Corner::BottomLeft => m,
                Corner::TopRight | Corner::BottomRight => screen.x - size.x - m,
            };
            let y = match corner {
                Corner::TopLeft | Corner::TopRight => m,
                Corner::BottomLeft | Corner::BottomRight => screen.y - size.y - m,
            };
            (x, y)
        }
        Anchor::At(p) => (p.x, p.y),
        Anchor::ToNode(_) => {
            let nr = anchor_node_rect.unwrap_or(Rect::new(0.0, 0.0, 0.0, 0.0));
            (nr.x, nr.y + nr.height)
        }
        Anchor::SelfManaged => return Rect::new(0.0, 0.0, size.x, size.y),
        Anchor::Fixed(_) => unreachable!("handled above"),
    };
    // Edge-clamp so the overlay does not spill off the right / bottom.
    if x + size.x > screen.x {
        x = (screen.x - size.x).max(0.0);
    }
    if y + size.y > screen.y {
        y = (screen.y - size.y).max(0.0);
    }
    Rect::new(x.max(0.0), y.max(0.0), size.x, size.y)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen() -> Vec2 {
        Vec2::new(1000.0, 800.0)
    }

    #[test]
    fn centered_places_in_middle() {
        let r = compute_overlay_rect(&Anchor::Centered, Vec2::new(200.0, 100.0), screen(), None);
        assert!((r.x - 400.0).abs() < 0.01);
        assert!((r.y - 350.0).abs() < 0.01);
        assert_eq!(r.width, 200.0);
        assert_eq!(r.height, 100.0);
    }

    #[test]
    fn corner_bottom_right_with_margin() {
        let r = compute_overlay_rect(
            &Anchor::Corner {
                corner: Corner::BottomRight,
                margin: 8.0,
            },
            Vec2::new(250.0, 394.0),
            screen(),
            None,
        );
        assert!((r.x - (1000.0 - 250.0 - 8.0)).abs() < 0.01);
        assert!((r.y - (800.0 - 394.0 - 8.0)).abs() < 0.01);
    }

    #[test]
    fn corner_top_left_with_margin() {
        let r = compute_overlay_rect(
            &Anchor::Corner {
                corner: Corner::TopLeft,
                margin: 10.0,
            },
            Vec2::new(100.0, 50.0),
            screen(),
            None,
        );
        assert!((r.x - 10.0).abs() < 0.01);
        assert!((r.y - 10.0).abs() < 0.01);
    }

    #[test]
    fn at_point_clamps_to_screen() {
        // Anchored near the right/bottom edge → clamps so it stays on-screen.
        let r = compute_overlay_rect(
            &Anchor::At(Vec2::new(950.0, 780.0)),
            Vec2::new(200.0, 100.0),
            screen(),
            None,
        );
        assert!((r.x - 800.0).abs() < 0.01);
        assert!((r.y - 700.0).abs() < 0.01);
    }

    #[test]
    fn fixed_is_verbatim_no_clamp() {
        let fixed = Rect::new(12.0, 34.0, 56.0, 78.0);
        let r = compute_overlay_rect(&Anchor::Fixed(fixed), Vec2::ZERO, screen(), None);
        assert_eq!(r.x, 12.0);
        assert_eq!(r.y, 34.0);
        assert_eq!(r.width, 56.0);
        assert_eq!(r.height, 78.0);
    }

    #[test]
    fn self_managed_returns_zero_origin() {
        let r = compute_overlay_rect(&Anchor::SelfManaged, Vec2::new(10.0, 10.0), screen(), None);
        assert_eq!(r.x, 0.0);
        assert_eq!(r.y, 0.0);
    }

    #[test]
    fn size_policy_content_passes_through_desired() {
        let s = SizePolicy::Content.resolve(screen(), Vec2::new(300.0, 200.0));
        assert_eq!(s, Vec2::new(300.0, 200.0));
    }

    #[test]
    fn size_policy_fraction_scales_to_screen() {
        // screen() is 1000×800 → 0.8 → 800×640, both above the mins.
        let s = SizePolicy::Fraction {
            frac: Vec2::new(0.8, 0.8),
            min: Vec2::new(460.0, 240.0),
        }
        .resolve(screen(), Vec2::new(300.0, 200.0));
        assert_eq!(s, Vec2::new(800.0, 640.0));
    }

    #[test]
    fn size_policy_fraction_floors_at_min() {
        // Tiny screen → fraction falls below the mins, which win.
        let s = SizePolicy::Fraction {
            frac: Vec2::new(0.8, 0.8),
            min: Vec2::new(460.0, 240.0),
        }
        .resolve(Vec2::new(400.0, 200.0), Vec2::ZERO);
        assert_eq!(s, Vec2::new(460.0, 240.0));
    }

    #[test]
    fn to_node_anchors_below_node() {
        let node = Rect::new(100.0, 200.0, 50.0, 20.0);
        let r = compute_overlay_rect(
            &Anchor::ToNode(NodeId::from_parts(7, 1)),
            Vec2::new(40.0, 30.0),
            screen(),
            Some(node),
        );
        assert!((r.x - 100.0).abs() < 0.01);
        assert!((r.y - 220.0).abs() < 0.01); // node.y + node.height
    }
}
