//! Hit-testing: which node, header, port, param row, chevron, breadcrumb,
//! or button is under a screen-space cursor — plus the marquee/overlap and
//! port-compatibility helpers the gestures lean on. Read-only against the
//! view model.

use super::*;

impl GraphCanvas {
    /// Lay out the breadcrumb segments in the canvas header, left to right:
    /// `[Root › title0 › title1 …]`. Returns `(target_depth, rect, label,
    /// is_current)` per segment. Empty at the document root (no breadcrumb
    /// drawn). Shared by render and hit-test so the click zones match the
    /// glyphs.
    pub(crate) fn breadcrumb_segments(&self, viewport: Rect) -> Vec<(usize, Rect, String, bool)> {
        if self.scope.is_empty() {
            return Vec::new();
        }
        let cw = BREADCRUMB_FONT * 0.55;
        let sep_w = 3.0 * cw; // width reserved for the " › " separator
        let y = viewport.y + (HEADER_HEIGHT - BREADCRUMB_FONT) * 0.5;
        let mut x = viewport.x + 10.0;
        let current_depth = self.scope_titles.len();
        let labels = std::iter::once("Root".to_string())
            .chain(self.scope_titles.iter().cloned());
        let mut segs = Vec::new();
        for (depth, label) in labels.enumerate() {
            let w = label.chars().count() as f32 * cw;
            segs.push((
                depth,
                Rect::new(x, y - 2.0, w, BREADCRUMB_FONT + 4.0),
                label,
                depth == current_depth,
            ));
            x += w + sep_w;
        }
        segs
    }

    /// Breadcrumb segment under a header click, by target depth (0 = root).
    /// `None` when the click misses every segment or there's no breadcrumb.
    pub(crate) fn breadcrumb_hit(&self, viewport: Rect, sx: f32, sy: f32) -> Option<usize> {
        self.breadcrumb_segments(viewport)
            .into_iter()
            .find(|(_, r, _, _)| sx >= r.x && sx <= r.x + r.w && sy >= r.y && sy <= r.y + r.h)
            .map(|(depth, _, _, _)| depth)
    }

    pub(crate) fn node_under(&self, viewport: Rect, sx: f32, sy: f32) -> Option<u32> {
        let (gx, gy) = self.to_graph(viewport, sx, sy);
        for node in self.nodes.iter().rev() {
            let (nx, ny) = node.pos_graph;
            let nh = node.height();
            if gx >= nx && gx <= nx + NODE_WIDTH && gy >= ny && gy <= ny + nh {
                return Some(node.id);
            }
        }
        None
    }

    /// Returns `true` if the cursor is over the header strip of the
    /// node it's hovering. Used to distinguish "click body to select"
    /// from "drag header to move".
    pub(crate) fn header_under(&self, viewport: Rect, sx: f32, sy: f32) -> Option<u32> {
        let (gx, gy) = self.to_graph(viewport, sx, sy);
        for node in self.nodes.iter().rev() {
            let (nx, ny) = node.pos_graph;
            if gx >= nx
                && gx <= nx + NODE_WIDTH
                && gy >= ny
                && gy <= ny + NODE_HEADER_HEIGHT
            {
                return Some(node.id);
            }
        }
        None
    }

    /// Hit-test which on-node param row (if any) is under the cursor,
    /// returning `(node_id, param_index)`. Works in screen space to match
    /// `draw_node`'s row layout exactly. Skips collapsed and param-less
    /// nodes, and walks topmost-first so overlapping nodes resolve like the
    /// draw order.
    pub(crate) fn param_row_under(&self, viewport: Rect, sx: f32, sy: f32) -> Option<(u32, usize)> {
        let header_h = NODE_HEADER_HEIGHT * self.zoom;
        let row_h = PARAM_ROW_H * self.zoom;
        let sw = NODE_WIDTH * self.zoom;
        for node in self.nodes.iter().rev() {
            if node.collapsed || node.params.is_empty() {
                continue;
            }
            let (nx, ny) = self.to_screen(viewport, node.pos_graph.0, node.pos_graph.1);
            let block_top = ny + header_h + node.preview_h() * self.zoom;
            let block_bottom = block_top + node.params.len() as f32 * row_h;
            if sx >= nx && sx <= nx + sw && sy >= block_top && sy < block_bottom {
                let idx = ((sy - block_top) / row_h) as usize;
                if idx < node.params.len() {
                    return Some((node.id, idx));
                }
            }
        }
        None
    }

    /// Hit-test the expose glyph at a param row's left edge, returning
    /// `(node_id, param_index)` when the cursor is over the dot of an
    /// *exposable* param. Built on `param_row_under` + `param_row_rect` +
    /// `expose_glyph_bounds`, so it can't drift from the drawn glyph. Returns
    /// `None` for non-exposable rows and for clicks that miss the dot (which
    /// then fall through to the row scrub). The hit box is padded a couple of
    /// px past the dot so the small target stays clickable.
    pub(crate) fn expose_glyph_under(
        &self,
        viewport: Rect,
        sx: f32,
        sy: f32,
    ) -> Option<(u32, usize)> {
        let (node_id, pi) = self.param_row_under(viewport, sx, sy)?;
        let node = self.find_node(node_id)?;
        let p = node.params.get(pi)?;
        if !kind_is_exposable(p.kind) {
            return None;
        }
        let row = self.param_row_rect(viewport, node_id, pi)?;
        let row_h = PARAM_ROW_H * self.zoom;
        let (gx, gy, gd) = expose_glyph_bounds(row.x, row.y, row_h, self.zoom);
        let pad = 2.0 * self.zoom;
        if sx >= gx - pad && sx <= gx + gd + pad && sy >= gy - pad && sy <= gy + gd + pad {
            Some((node_id, pi))
        } else {
            None
        }
    }

    /// Screen-space rect of one on-node param row, by `(node_id,
    /// param_index)`. Mirrors `param_row_under`'s layout exactly so an
    /// anchored popover lines up with the row it was opened from. `None`
    /// for a missing node / out-of-range index.
    pub(crate) fn param_row_rect(&self, viewport: Rect, node_id: u32, pi: usize) -> Option<Rect> {
        let node = self.find_node(node_id)?;
        if pi >= node.params.len() {
            return None;
        }
        let header_h = NODE_HEADER_HEIGHT * self.zoom;
        let row_h = PARAM_ROW_H * self.zoom;
        let sw = NODE_WIDTH * self.zoom;
        let (nx, ny) = self.to_screen(viewport, node.pos_graph.0, node.pos_graph.1);
        let row_top = ny + header_h + node.preview_h() * self.zoom + pi as f32 * row_h;
        Some(Rect::new(nx, row_top, sw, row_h))
    }

    /// Hit-test ports near the cursor. Searches all output then input
    /// ports of every node, returning the first within `PORT_HIT_RADIUS`
    /// graph-space units of the cursor. Outputs take priority over
    /// inputs when both are nearby (only matters in degenerate layouts
    /// since ports are on opposite edges).
    pub(crate) fn port_under(&self, viewport: Rect, sx: f32, sy: f32) -> Option<PortHit> {
        const PORT_HIT_RADIUS: f32 = 10.0;
        let (gx, gy) = self.to_graph(viewport, sx, sy);
        for node in self.nodes.iter().rev() {
            for (i, port) in node.outputs.iter().enumerate() {
                let (px, py) = node.output_port_pos_graph(i);
                let dx = gx - px;
                let dy = gy - py;
                if dx * dx + dy * dy <= PORT_HIT_RADIUS * PORT_HIT_RADIUS {
                    return Some(PortHit {
                        node_id: node.id,
                        port_name: port.name.clone(),
                        is_output: true,
                    });
                }
            }
            for (i, port) in node.inputs.iter().enumerate() {
                let (px, py) = node.input_port_pos_graph(i);
                let dx = gx - px;
                let dy = gy - py;
                if dx * dx + dy * dy <= PORT_HIT_RADIUS * PORT_HIT_RADIUS {
                    return Some(PortHit {
                        node_id: node.id,
                        port_name: port.name.clone(),
                        is_output: false,
                    });
                }
            }
        }
        None
    }

    /// Hit-test the collapse chevron in a node header (its right edge).
    /// Returns the node id when the cursor is over the chevron of a node
    /// that has params (param-less nodes draw no chevron). Checked before
    /// the header-drag test so toggling collapse doesn't also start a move.
    pub(crate) fn chevron_under(&self, viewport: Rect, sx: f32, sy: f32) -> Option<u32> {
        let header_h = NODE_HEADER_HEIGHT * self.zoom;
        let sw = NODE_WIDTH * self.zoom;
        let chev_w = 20.0 * self.zoom;
        self.nodes.iter().find_map(|node| {
            if node.params.is_empty() {
                return None;
            }
            let (nx, ny) = self.to_screen(viewport, node.pos_graph.0, node.pos_graph.1);
            let in_x = sx >= nx + sw - chev_w && sx <= nx + sw;
            let in_y = sy >= ny && sy <= ny + header_h;
            (in_x && in_y).then_some(node.id)
        })
    }

    /// Find the wire whose destination is `(to_node, to_port)`. Returns
    /// the wire's index in `self.wires`. Each input port has at most
    /// one incoming wire (enforced at graph-validate time), so this is
    /// unambiguous.
    pub(crate) fn wire_into(&self, to_node: u32, to_port: &str) -> Option<usize> {
        self.wires
            .iter()
            .position(|w| w.to_node == to_node && w.to_port == to_port)
    }

    /// Bounding rect of the "Reset to Default" header button. Single
    /// source of truth so render-side and click-hit-test use the same
    /// geometry.
    pub(crate) fn reset_button_rect(&self, viewport: Rect) -> Rect {
        let y = viewport.y + (HEADER_HEIGHT - RESET_BUTTON_H) * 0.5;
        let x = viewport.x + viewport.w - RESET_BUTTON_RIGHT_GAP - RESET_BUTTON_W;
        Rect {
            x,
            y,
            w: RESET_BUTTON_W,
            h: RESET_BUTTON_H,
        }
    }

    /// While dragging a wire from a port of colour `from_color`, classify the
    /// input port currently under the cursor: `Some(true)` compatible drop,
    /// `Some(false)` incompatible, `None` when the cursor isn't over a foreign
    /// input port. Compatibility is port-category equality (the canvas encodes
    /// category as colour); the real connect still validates server-side, so
    /// this is purely the live hint behind the ghost wire's green/red tint.
    pub(crate) fn wire_drop_compat(&self, viewport: Rect, from_node: u32, from_color: Color32) -> Option<bool> {
        let hit = self.port_under(viewport, self.cursor.0, self.cursor.1)?;
        if hit.is_output || hit.node_id == from_node {
            return None;
        }
        let to_color = self
            .find_node(hit.node_id)?
            .inputs
            .iter()
            .find(|p| p.name == hit.port_name)?
            .color;
        Some(ports_compatible(from_color, to_color))
    }
}

/// Two ports can be wired iff they share a port category, which the canvas
/// encodes 1:1 as the port colour (Texture2D and its typed variant share one
/// colour, so they're treated as compatible — exactly the validator's view).
pub(crate) fn ports_compatible(from_color: Color32, to_color: Color32) -> bool {
    from_color == to_color
}

/// Axis-aligned rectangle overlap, each `(x, y, w, h)`. Touching edges don't
/// count as overlapping (strict inequality), matching the marquee feel: a
/// node is grabbed only once the band actually crosses into it.
pub(crate) fn rects_overlap(a: (f32, f32, f32, f32), b: (f32, f32, f32, f32)) -> bool {
    a.0 < b.0 + b.2 && a.0 + a.2 > b.0 && a.1 < b.1 + b.3 && a.1 + a.3 > b.1
}

/// Ids of nodes whose box intersects the marquee `rect` (graph space). Pure;
/// unit-tested via `rects_overlap`.
pub(crate) fn marquee_hits(rect: (f32, f32, f32, f32), nodes: &[NodeView]) -> Vec<u32> {
    nodes
        .iter()
        .filter(|n| {
            rects_overlap(rect, (n.pos_graph.0, n.pos_graph.1, NODE_WIDTH, n.height()))
        })
        .map(|n| n.id)
        .collect()
}
