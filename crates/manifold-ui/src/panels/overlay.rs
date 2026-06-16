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
    /// Captures all input. A click outside the overlay's own nodes dismisses
    /// it. `dim_background` adds a full-screen scrim node beneath the overlay.
    Modal { dim_background: bool },
    /// Floats above the UI. Only consumes clicks on its own nodes; a click
    /// elsewhere dismisses it (dismiss-only — the click is consumed, not
    /// passed through). The perf HUD is a modeless overlay that never consumes.
    Modeless,
}

/// Where an overlay positions itself. The driver resolves this to a screen rect
/// via [`compute_overlay_rect`], applying edge-clamping once for every overlay.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Anchor {
    /// Centered in the screen (settings-dialog style).
    Centered,
    /// Top-left pinned to a screen point (click-anchored popups).
    At(Vec2),
    /// Just below a tree node (the driver supplies the node's rect).
    ToNode(i32),
    /// An explicit rect, used verbatim (no clamping).
    Fixed(Rect),
}

/// The result of routing one event to an overlay.
pub enum OverlayResponse {
    /// Not this overlay's event — the driver tries the next overlay / panels.
    Ignored,
    /// Consumed; emit these actions and keep the overlay open.
    Consumed(Vec<PanelAction>),
    /// Consumed; emit these actions and close the overlay afterwards.
    Dismiss(Vec<PanelAction>),
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

    /// The overlay's size in logical pixels (may depend on content).
    fn desired_size(&self) -> Vec2;

    /// Build the overlay's nodes into `tree` at the driver-resolved `rect`.
    /// Only called when `is_open()`.
    fn build_at(&mut self, tree: &mut UITree, rect: Rect);

    /// Route one event. The driver walks open overlays top-of-stack first and
    /// stops at the first non-`Ignored` response.
    fn on_event(&mut self, event: &UIEvent, tree: &mut UITree) -> OverlayResponse;

    /// Close the overlay (called by the driver on `Dismiss` and on Escape).
    fn close(&mut self);
}

/// Resolve an [`Anchor`] + size to an on-screen rect, clamped so the overlay
/// stays fully visible. One place for the edge-clamp math every overlay used to
/// hand-roll. `anchor_node_rect` is the resolved rect of an [`Anchor::ToNode`]
/// target (the driver looks it up in the tree); ignored for other anchors.
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
        Anchor::At(p) => (p.x, p.y),
        Anchor::ToNode(_) => {
            let nr = anchor_node_rect.unwrap_or(Rect::new(0.0, 0.0, 0.0, 0.0));
            (nr.x, nr.y + nr.height)
        }
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
