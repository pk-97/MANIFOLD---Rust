//! [`Dock`] — resizable panels around a central canvas.
//!
//! One layout number per edge (`left_w`, `right_w`, `bottom_h`); the canvas
//! absorbs whatever is left. This generalizes the main window's single
//! video/timeline split (`ScreenLayout::split_handle` + `is_near_split_handle`
//! + `update_split_from_drag`) to three dockable edges around a central region.
//!
//! ## One source of truth
//!
//! [`Dock::rects`] returns *every* rect for a given area — the three panels, the
//! leftover canvas, and the three drag-handle bands — in a single [`DockRects`]
//! struct. The render pass and the input pass call the same method, so they
//! cannot compute different geometry. That is the whole point: the graph editor
//! used to recompute `canvas_x = preview_width; card_x = w - card_width` by hand
//! in five places (render, `editor_canvas_viewport`, the headless PNG path, and
//! two pointer handlers); any drift between them mis-hit-tested the canvas. The
//! `Dock` collapses that arithmetic into one place.
//!
//! ## Interaction (mirrors the main split's triad)
//!
//! `hit_test(area, pos) → begin(edge) → drag(area, pos) → end()`, plus
//! `set_hover_from` for the resize cursor + handle highlight. Same visual
//! constants as the main UI (`RESIZE_HANDLE_*`, `DIVIDER_COLOR`,
//! `INSPECTOR_RESIZE_HANDLE_WIDTH`), so it reads as the same instrument.

use crate::color;
use crate::cursors::TimelineCursor;
use crate::node::{Color32, Rect, Vec2};

/// Width (px) of a drag-handle hit / highlight band, centered on the seam.
/// Same as the main inspector handle so the feel matches.
pub const DOCK_HANDLE_W: f32 = color::INSPECTOR_RESIZE_HANDLE_WIDTH; // 6.0

/// Smallest the central canvas is allowed to get, horizontally and vertically.
/// Dragging a panel past this clamps rather than starving the canvas.
pub const MIN_CANVAS_W: f32 = 200.0;
pub const MIN_CANVAS_H: f32 = 140.0;

/// Graph-editor default column widths. These are the seed for [`Dock::editor`];
/// once the user drags a divider the live value on the `Dock` takes over. The
/// numbers match the fixed widths the editor shipped with before it was
/// resizable, so opening the editor looks identical until a drag happens.
pub const EDITOR_LEFT_DEFAULT: f32 = 460.0;
pub const EDITOR_RIGHT_DEFAULT: f32 = 340.0;

/// Which resizable edge a handle / drag refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DockEdge {
    Left,
    Right,
    Bottom,
}

/// Every rect the dock produces for one area, computed together so all
/// consumers agree. Disabled edges have zero-size panels and handles.
#[derive(Debug, Clone, Copy)]
pub struct DockRects {
    /// Full-height left column (zero-width if the left panel is hidden).
    pub left: Rect,
    /// Full-height right column (zero-width if hidden).
    pub right: Rect,
    /// Bottom strip under the canvas only (zero-height if hidden).
    pub bottom: Rect,
    /// The leftover central region — everything not taken by a panel.
    pub canvas: Rect,
    /// Vertical hit/draw band on the left column's inner seam.
    pub left_handle: Rect,
    /// Vertical hit/draw band on the right column's inner seam.
    pub right_handle: Rect,
    /// Horizontal hit/draw band on the bottom strip's top seam.
    pub bottom_handle: Rect,
}

/// Resizable dock layout: optional left + right columns and an optional bottom
/// strip around a central canvas. Persist one per window; the graph-editor
/// workspace owns one.
#[derive(Debug, Clone)]
pub struct Dock {
    /// Left column width (px). Ignored when `show_left` is false.
    pub left_w: f32,
    /// Right column width (px). Ignored when `show_right` is false.
    pub right_w: f32,
    /// Bottom strip height (px). Ignored when `show_bottom` is false.
    pub bottom_h: f32,

    pub left_range: (f32, f32),
    pub right_range: (f32, f32),
    pub bottom_range: (f32, f32),

    pub show_left: bool,
    pub show_right: bool,
    pub show_bottom: bool,

    /// Edge currently being dragged, if any.
    active: Option<DockEdge>,
    /// Edge under the cursor (for handle highlight + resize cursor).
    hover: Option<DockEdge>,
}

impl Dock {
    /// Graph-editor default: left preview column + right card lane, no bottom
    /// strip yet (the mini-timeline enables it later). Sizes seed from the
    /// `EDITOR_*_DEFAULT` constants.
    pub fn editor() -> Self {
        Self {
            left_w: EDITOR_LEFT_DEFAULT,
            right_w: EDITOR_RIGHT_DEFAULT,
            bottom_h: 140.0,
            left_range: (260.0, 640.0),
            right_range: (240.0, 560.0),
            bottom_range: (80.0, 360.0),
            show_left: true,
            show_right: true,
            show_bottom: false,
            active: None,
            hover: None,
        }
    }

    // ── Geometry (the single source of truth) ──────────────────────────────

    /// Effective sizes after visibility gating.
    fn eff(&self) -> (f32, f32, f32) {
        (
            if self.show_left { self.left_w } else { 0.0 },
            if self.show_right { self.right_w } else { 0.0 },
            if self.show_bottom { self.bottom_h } else { 0.0 },
        )
    }

    /// All panel / canvas / handle rects for `area`, computed together.
    pub fn rects(&self, area: Rect) -> DockRects {
        let (lw, rw, bh) = self.eff();
        let center_x = area.x + lw;
        let center_w = (area.width - lw - rw).max(0.0);
        let canvas_h = (area.height - bh).max(0.0);
        let h = DOCK_HANDLE_W;
        let right_x = area.x_max() - rw;
        let bottom_y = area.y_max() - bh;

        DockRects {
            left: Rect::new(area.x, area.y, lw, area.height),
            right: Rect::new(right_x, area.y, rw, area.height),
            bottom: Rect::new(center_x, bottom_y, center_w, bh),
            canvas: Rect::new(center_x, area.y, center_w, canvas_h),
            left_handle: Rect::new(center_x - h * 0.5, area.y, h, area.height),
            right_handle: Rect::new(right_x - h * 0.5, area.y, h, area.height),
            bottom_handle: Rect::new(center_x, bottom_y - h * 0.5, center_w, h),
        }
    }

    /// Just the canvas rect — the common case for consumers that only place the
    /// graph viewport.
    pub fn canvas(&self, area: Rect) -> Rect {
        self.rects(area).canvas
    }

    // ── Hit-testing + drag (mirrors the main split triad) ───────────────────

    /// The resizable edge whose handle band contains `pos`, if any. Side
    /// handles win over the bottom handle where they meet at the corners.
    pub fn hit_test(&self, area: Rect, pos: Vec2) -> Option<DockEdge> {
        let r = self.rects(area);
        if self.show_left && r.left_handle.contains(pos) {
            Some(DockEdge::Left)
        } else if self.show_right && r.right_handle.contains(pos) {
            Some(DockEdge::Right)
        } else if self.show_bottom && r.bottom_handle.contains(pos) {
            Some(DockEdge::Bottom)
        } else {
            None
        }
    }

    /// Update the hover edge from a cursor position (no-op while dragging — the
    /// active edge stays highlighted). Call on every cursor move.
    pub fn set_hover_from(&mut self, area: Rect, pos: Vec2) {
        if self.active.is_none() {
            self.hover = self.hit_test(area, pos);
        }
    }

    /// Begin dragging an edge (call after a successful `hit_test` on press).
    pub fn begin(&mut self, edge: DockEdge) {
        self.active = Some(edge);
        self.hover = Some(edge);
    }

    /// True while a divider drag is in progress.
    pub fn is_dragging(&self) -> bool {
        self.active.is_some()
    }

    /// The edge currently highlighted (hovered, or dragged). For dirty-checking
    /// so the editor repaints only when the highlight actually changes.
    pub fn highlighted(&self) -> Option<DockEdge> {
        self.hover
    }

    /// Update the dragged edge's size from the cursor, clamped to its range and
    /// to `MIN_CANVAS_*` so a panel can never starve the canvas. No-op when no
    /// edge is active.
    pub fn drag(&mut self, area: Rect, pos: Vec2) {
        let (lw, rw, _) = self.eff();
        match self.active {
            Some(DockEdge::Left) => {
                let ceiling = (area.width - rw - MIN_CANVAS_W).max(self.left_range.0);
                self.left_w = (pos.x - area.x)
                    .clamp(self.left_range.0, self.left_range.1)
                    .min(ceiling);
            }
            Some(DockEdge::Right) => {
                let ceiling = (area.width - lw - MIN_CANVAS_W).max(self.right_range.0);
                self.right_w = (area.x_max() - pos.x)
                    .clamp(self.right_range.0, self.right_range.1)
                    .min(ceiling);
            }
            Some(DockEdge::Bottom) => {
                let ceiling = (area.height - MIN_CANVAS_H).max(self.bottom_range.0);
                self.bottom_h = (area.y_max() - pos.y)
                    .clamp(self.bottom_range.0, self.bottom_range.1)
                    .min(ceiling);
            }
            None => {}
        }
    }

    /// End the active drag.
    pub fn end(&mut self) {
        self.active = None;
    }

    /// The resize cursor for the current hover / drag, if the pointer is on a
    /// handle: horizontal arrows for the columns, vertical for the bottom strip.
    pub fn cursor(&self) -> Option<TimelineCursor> {
        match self.active.or(self.hover) {
            Some(DockEdge::Left) | Some(DockEdge::Right) => Some(TimelineCursor::ResizeHorizontal),
            Some(DockEdge::Bottom) => Some(TimelineCursor::ResizeVertical),
            None => None,
        }
    }

    // ── Draw (one call from the render pass) ────────────────────────────────

    /// Draw the divider seams for every visible edge, highlighting the hovered
    /// or dragged handle. A 1px `DIVIDER_COLOR` seam always; the wider
    /// `RESIZE_HANDLE_*` band only when hovered/active, exactly like the main
    /// split handle.
    pub fn draw(&self, area: Rect, ui: &mut dyn crate::draw::Painter) {
        let r = self.rects(area);
        let mut seam = |edge: DockEdge, handle: Rect, vertical: bool| {
            // Highlight band under the cursor / during drag.
            let band = if self.active == Some(edge) {
                Some(color::RESIZE_HANDLE_DRAG)
            } else if self.hover == Some(edge) {
                Some(color::RESIZE_HANDLE_HOVER)
            } else {
                None
            };
            if let Some(c) = band {
                ui.draw_rect(handle.x, handle.y, handle.width, handle.height, c);
            }
            // Always the thin seam so the column edge reads even at idle.
            let c: Color32 = color::DIVIDER_COLOR;
            if vertical {
                let x = handle.x + handle.width * 0.5;
                ui.draw_line(x, handle.y, x, handle.y_max(), 1.0, c);
            } else {
                let y = handle.y + handle.height * 0.5;
                ui.draw_line(handle.x, y, handle.x_max(), y, 1.0, c);
            }
        };
        if self.show_left {
            seam(DockEdge::Left, r.left_handle, true);
        }
        if self.show_right {
            seam(DockEdge::Right, r.right_handle, true);
        }
        if self.show_bottom {
            seam(DockEdge::Bottom, r.bottom_handle, false);
        }
    }
}

impl Default for Dock {
    fn default() -> Self {
        Self::editor()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area() -> Rect {
        Rect::new(0.0, 0.0, 1600.0, 900.0)
    }

    #[test]
    fn canvas_absorbs_the_rest() {
        let d = Dock::editor();
        let r = d.rects(area());
        assert_eq!(r.canvas.x, EDITOR_LEFT_DEFAULT);
        assert_eq!(r.canvas.width, 1600.0 - EDITOR_LEFT_DEFAULT - EDITOR_RIGHT_DEFAULT);
        assert_eq!(r.canvas.height, 900.0); // bottom hidden by default
        // Panels abut the canvas with no gap or overlap.
        assert_eq!(r.left.x_max(), r.canvas.x);
        assert_eq!(r.canvas.x_max(), r.right.x);
    }

    #[test]
    fn hidden_edge_gives_zero_panel_and_full_canvas() {
        let mut d = Dock::editor();
        d.show_right = false;
        let r = d.rects(area());
        assert_eq!(r.right.width, 0.0);
        assert_eq!(r.canvas.x_max(), 1600.0);
    }

    #[test]
    fn handle_hit_test_maps_to_edge() {
        let d = Dock::editor();
        let r = d.rects(area());
        let mid_left = Vec2::new(r.left_handle.x + DOCK_HANDLE_W * 0.5, 450.0);
        assert_eq!(d.hit_test(area(), mid_left), Some(DockEdge::Left));
        let mid_right = Vec2::new(r.right_handle.x + DOCK_HANDLE_W * 0.5, 450.0);
        assert_eq!(d.hit_test(area(), mid_right), Some(DockEdge::Right));
        // Dead center of the canvas hits nothing.
        assert_eq!(d.hit_test(area(), Vec2::new(800.0, 450.0)), None);
    }

    #[test]
    fn drag_clamps_to_range() {
        let mut d = Dock::editor();
        d.begin(DockEdge::Left);
        // Drag far past the max — clamps to left_range.1.
        d.drag(area(), Vec2::new(5000.0, 450.0));
        assert_eq!(d.left_w, d.left_range.1);
        // Drag to zero — clamps to left_range.0.
        d.drag(area(), Vec2::new(-100.0, 450.0));
        assert_eq!(d.left_w, d.left_range.0);
        d.end();
        assert!(!d.is_dragging());
    }

    #[test]
    fn drag_preserves_min_canvas_when_feasible() {
        // 1100 wide: left(max 640) + right(340) would leave 120 < MIN_CANVAS_W,
        // so the left drag clamps to keep the canvas at exactly MIN_CANVAS_W.
        let w = Rect::new(0.0, 0.0, 1100.0, 900.0);
        let mut d = Dock::editor();
        d.begin(DockEdge::Left);
        d.drag(w, Vec2::new(640.0, 450.0)); // wants max left_w=640
        let r = d.rects(w);
        assert!((r.canvas.width - MIN_CANVAS_W).abs() < 0.01);
        assert!((d.left_w - (1100.0 - d.right_w - MIN_CANVAS_W)).abs() < 0.01);
    }

    #[test]
    fn never_overlaps_even_when_too_narrow() {
        // Infeasible width (column minimums alone exceed it): column mins win,
        // canvas shrinks but panels still never overlap (canvas width >= 0).
        let narrow = Rect::new(0.0, 0.0, 700.0, 900.0);
        let mut d = Dock::editor();
        d.begin(DockEdge::Left);
        d.drag(narrow, Vec2::new(640.0, 450.0));
        let r = d.rects(narrow);
        assert!(r.canvas.width >= 0.0);
        assert!(r.canvas.x_max() <= r.right.x + 0.01);
    }
}
