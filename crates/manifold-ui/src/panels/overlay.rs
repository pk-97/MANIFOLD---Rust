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
use crate::node::{Rect, Vec2};
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
    ToNode(i32),
    /// An explicit rect, used verbatim (no clamping).
    Fixed(Rect),
    /// The overlay positions itself in `build_at` from `OverlayPlacement.screen`
    /// (content-sized, click-anchored popups that already clamp internally:
    /// browser popup, Ableton picker, dropdown). The driver passes a zero rect.
    SelfManaged,
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

    /// The overlay's size in logical pixels (may depend on content). Unused for
    /// `SelfManaged` overlays.
    fn desired_size(&self) -> Vec2;

    /// Build the overlay's nodes into `tree`. Only called when `is_open()`.
    fn build_at(&mut self, tree: &mut UITree, placement: OverlayPlacement);

    /// Route one event. The driver walks open overlays top-of-stack first and
    /// stops at the first non-`Ignored` response (or the first Modal).
    fn on_event(&mut self, event: &UIEvent, tree: &mut UITree) -> OverlayResponse;

    /// Close the overlay (called by the driver on Escape at the top of stack).
    fn close(&mut self);
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
