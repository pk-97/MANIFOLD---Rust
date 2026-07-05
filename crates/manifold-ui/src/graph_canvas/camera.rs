//! Camera: graph↔screen projection (`Axis`-based), jump-to-node focus,
//! zoom-to-fit framing, and scroll-wheel zoom. Moves the view only —
//! never node positions.

use super::*;

impl GraphCanvas {
    /// Jump-to-node: navigate the canvas to the node with stable `node_id`,
    /// descending into the group that contains it if needed, select it, and
    /// queue it to be centred. Returns `false` if the id isn't in the snapshot.
    /// The host calls this when the user activates a card param's locate
    /// affordance, so the left card and the centre canvas stay in lockstep.
    ///
    /// Centring is deferred to [`Self::resolve_pending_focus`] (next frame) so
    /// the node's position is known after `set_snapshot` rebuilds the level —
    /// otherwise a node inside a just-entered group has no position yet.
    pub fn focus_node(
        &mut self,
        snap: &crate::graph_view::GraphSnapshot,
        node_id: &manifold_foundation::NodeId,
    ) -> bool {
        let Some((scope, titles, rid)) = find_node_scope(snap, node_id) else {
            return false;
        };
        if self.scope != scope {
            self.scope = scope;
            self.scope_titles = titles;
            // Don't auto-format the entered level — preserve its arrangement.
            self.format_on_enter = false;
            // Force the next `set_snapshot` to rebuild for the new level.
            self.topology_hash = 0;
        }
        self.select_single(rid);
        self.pending_focus = Some(rid);
        true
    }

    /// Centre the pending jump-to-node target (set by [`Self::focus_node`]) once
    /// it exists at the current level. No-op when nothing is pending or the
    /// target isn't laid out yet (a scope rebuild is still in flight — retried
    /// next frame). Called by the editor present path, which has the viewport.
    pub fn resolve_pending_focus(&mut self, viewport: Rect) {
        let Some(rid) = self.pending_focus else {
            return;
        };
        let Some(node) = self.nodes.iter().find(|n| n.id == rid) else {
            return;
        };
        let node_cx = node.pos_graph.0 + NODE_WIDTH * 0.5;
        let node_cy = node.pos_graph.1 + node.height() * 0.5;
        // Invert `to_screen` so the node centre lands at the canvas content
        // centre: `screen = origin + (graph + pan) * zoom`.
        let content_cx = viewport.w * 0.5;
        let content_cy = HEADER_HEIGHT + (viewport.h - HEADER_HEIGHT) * 0.5;
        self.pan.0 = content_cx / self.zoom - node_cx;
        self.pan.1 = (content_cy - HEADER_HEIGHT) / self.zoom - node_cy;
        self.pending_focus = None;
    }

    /// Apply a pending zoom-to-fit (set on editor open / scope change) once the
    /// level is laid out. A pending jump-to-node wins — it's an explicit target,
    /// so we drop the fit rather than fight it. Called by the editor present path
    /// alongside [`Self::resolve_pending_focus`]; no-op until then.
    pub fn apply_pending_fit(&mut self, viewport: Rect) {
        if !self.fit_pending {
            return;
        }
        if self.pending_focus.is_some() {
            self.fit_pending = false;
            return;
        }
        if self.zoom_to_fit(viewport) {
            self.fit_pending = false;
        }
    }

    /// Frame every laid-out node in the viewport: the largest zoom whose node
    /// bounding box (plus margin) fits the canvas, centred. Capped at 1.0 so a
    /// sparse graph doesn't balloon. Returns `false` — leaving the request
    /// pending — until the level has at least one finite-positioned node (a
    /// scope rebuild may still be in flight). Camera-only; never moves a node.
    pub(crate) fn zoom_to_fit(&mut self, viewport: Rect) -> bool {
        let (mut min_x, mut min_y) = (f32::INFINITY, f32::INFINITY);
        let (mut max_x, mut max_y) = (f32::NEG_INFINITY, f32::NEG_INFINITY);
        for n in &self.nodes {
            if !n.pos_graph.0.is_finite() || !n.pos_graph.1.is_finite() {
                continue;
            }
            min_x = min_x.min(n.pos_graph.0);
            min_y = min_y.min(n.pos_graph.1);
            max_x = max_x.max(n.pos_graph.0 + NODE_WIDTH);
            max_y = max_y.max(n.pos_graph.1 + n.height());
        }
        if !min_x.is_finite() {
            return false; // nothing laid out yet — retry next frame
        }
        let bbox_w = (max_x - min_x).max(1.0);
        let bbox_h = (max_y - min_y).max(1.0);
        // Margin so nodes don't kiss the canvas edge. Matches the zoom clamp
        // used by scroll-wheel zoom (lower bound) and caps fit at 1.0.
        const FIT_MARGIN: f32 = 40.0;
        let content_w = (viewport.w - 2.0 * FIT_MARGIN).max(1.0);
        let content_h = (viewport.h - HEADER_HEIGHT - 2.0 * FIT_MARGIN).max(1.0);
        // Never magnify past 1.0 (a sparse graph shouldn't balloon). The floor
        // is the shared MIN_ZOOM — the same one scroll-wheel zoom bottoms out at
        // — so the fit can't strand the view at a zoom the user can't reach by
        // hand. A tall generator (e.g. an 8-object glTF import, ~30 nodes in one
        // column) needs ~0.1 to frame; the old 0.25 floor left most of it below
        // the viewport on open.
        self.zoom = (content_w / bbox_w).min(content_h / bbox_h).clamp(MIN_ZOOM, 1.0);
        // Centre the bbox: invert `to_screen` so the bbox centre lands at the
        // canvas content centre. `screen = origin + (graph + pan) * zoom`.
        let bbox_cx = (min_x + max_x) * 0.5;
        let bbox_cy = (min_y + max_y) * 0.5;
        self.pan.0 = viewport.w * 0.5 / self.zoom - bbox_cx;
        self.pan.1 = (viewport.h - HEADER_HEIGHT) * 0.5 / self.zoom - bbox_cy;
        true
    }

    // ── Coordinate transforms ───────────────────────────────────────
    //
    // graph↔screen is the same 1D affine map as the timeline's beat↔pixel,
    // expressed via the shared `Axis`. X and Y share the zoom scale and differ
    // only in their screen origin; pan is a logical-space shift, so each axis is
    // `Axis::from_pan(zoom, pan, origin)`.

    pub(crate) fn x_axis(&self, viewport: Rect) -> Axis {
        Axis::from_pan(self.zoom, self.pan.0, viewport.x)
    }

    pub(crate) fn y_axis(&self, viewport: Rect) -> Axis {
        Axis::from_pan(self.zoom, self.pan.1, viewport.y + HEADER_HEIGHT)
    }

    pub(crate) fn to_screen(&self, viewport: Rect, gx: f32, gy: f32) -> (f32, f32) {
        (
            self.x_axis(viewport).to_screen(gx),
            self.y_axis(viewport).to_screen(gy),
        )
    }

    pub(crate) fn to_graph(&self, viewport: Rect, sx: f32, sy: f32) -> (f32, f32) {
        (
            self.x_axis(viewport).to_logical(sx),
            self.y_axis(viewport).to_logical(sy),
        )
    }

    pub fn on_scroll(&mut self, viewport: Rect, dy: f32) {
        let (gx_before, gy_before) = self.to_graph(viewport, self.cursor.0, self.cursor.1);
        let factor = (dy * 0.0015).exp();
        let new_zoom = (self.zoom * factor).clamp(MIN_ZOOM, MAX_ZOOM);
        self.zoom = new_zoom;
        let (gx_after, gy_after) = self.to_graph(viewport, self.cursor.0, self.cursor.1);
        self.pan.0 += gx_after - gx_before;
        self.pan.1 += gy_after - gy_before;
    }
}
