//! Hit-testing: which node, header, port, param row, chevron, breadcrumb,
//! or button is under a screen-space cursor — plus the marquee/overlap and
//! port-compatibility helpers the gestures lean on. Read-only against the
//! view model.

use super::*;
use crate::hit_targets::{HitTargetEntry, HitTargets};

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
            // Expanded only — collapsed nodes show a port band, not param rows.
            if node.collapsed || node.rows.is_empty() {
                continue;
            }
            let (nx, ny) = self.to_screen(viewport, node.pos_graph.0, node.pos_graph.1);
            let block_top = ny + header_h + node.preview_h() * self.zoom;
            let block_bottom = block_top + node.rows.len() as f32 * row_h;
            if sx >= nx && sx <= nx + sw && sy >= block_top && sy < block_bottom {
                let idx = ((sy - block_top) / row_h) as usize;
                // A row is a param only if it's a Param row; output / input
                // socket rows return no param hit (they scrub nothing).
                if let Some(crate::graph_canvas::NodeRow::Param { param, .. }) = node.rows.get(idx) {
                    return Some((node.id, *param));
                }
            }
        }
        None
    }

    /// Hit-test which on-node `NodeRow::Action` gesture row (if any) is under
    /// the cursor — the "+ Object" / "+ Light" buttons on `render_scene`'s
    /// face (D7/D7a). Same screen-space geometry as [`Self::param_row_under`]
    /// (one row pitch, walked topmost-first), filtered to `Action` rows so a
    /// click landing on an ordinary param row falls through to the normal
    /// scrub path instead.
    pub(crate) fn action_row_under(
        &self,
        viewport: Rect,
        sx: f32,
        sy: f32,
    ) -> Option<(u32, crate::graph_canvas::NodeActionKind)> {
        let header_h = NODE_HEADER_HEIGHT * self.zoom;
        let row_h = PARAM_ROW_H * self.zoom;
        let sw = NODE_WIDTH * self.zoom;
        for node in self.nodes.iter().rev() {
            if node.collapsed || node.rows.is_empty() {
                continue;
            }
            let (nx, ny) = self.to_screen(viewport, node.pos_graph.0, node.pos_graph.1);
            let block_top = ny + header_h + node.preview_h() * self.zoom;
            let block_bottom = block_top + node.rows.len() as f32 * row_h;
            if sx >= nx && sx <= nx + sw && sy >= block_top && sy < block_bottom {
                let idx = ((sy - block_top) / row_h) as usize;
                if let Some(crate::graph_canvas::NodeRow::Action(kind)) = node.rows.get(idx) {
                    return Some((node.id, *kind));
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
        // A group-face row (D6) is already the live mirror of an exposed card
        // param — it shows the card surface, not an authoring picker, so it
        // never draws or accepts an expose glyph of its own.
        if node.is_group {
            return None;
        }
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
        // Find which body row draws param `pi`, so the rect lands on the right
        // line now that params, outputs, and inputs share one row column.
        let row_idx = node.rows.iter().position(|r| {
            matches!(r, crate::graph_canvas::NodeRow::Param { param, .. } if *param == pi)
        })?;
        let header_h = NODE_HEADER_HEIGHT * self.zoom;
        let row_h = PARAM_ROW_H * self.zoom;
        let sw = NODE_WIDTH * self.zoom;
        let (nx, ny) = self.to_screen(viewport, node.pos_graph.0, node.pos_graph.1);
        let row_top = ny + header_h + node.preview_h() * self.zoom + row_idx as f32 * row_h;
        Some(Rect::new(nx, row_top, sw, row_h))
    }

    /// Screen-space x where a numeric param row's slider TRACK zone begins —
    /// i.e. past the right-aligned label cell. Delegates to
    /// [`crate::slider::BitmapSlider::zones`] (UI_WIDGET_UNIFICATION P1) fed
    /// the exact same `slider_rect`/`SliderMetrics` `render.rs`'s
    /// `NodeRow::Param` branch feeds `BitmapSlider::draw` — one geometry
    /// source (I3), not a canvas-local copy, so the right-click reset
    /// hit-zone (BUG-105) can't drift from the drawn label/track boundary.
    /// Same for every row on a node (the offset is node-relative, not
    /// per-row), so this only needs `node_id`. `None` for a missing node.
    pub(crate) fn param_slider_track_x(&self, viewport: Rect, node_id: u32) -> Option<f32> {
        let node = self.find_node(node_id)?;
        let (nx, _ny) = self.to_screen(viewport, node.pos_graph.0, node.pos_graph.1);
        let sw = NODE_WIDTH * self.zoom;
        let pad_x = PARAM_PAD_X * self.zoom;
        let slider_x = nx + PARAM_LABEL_X * self.zoom;
        let row_right = nx + sw - pad_x;
        // Only x/width feed zones()'s label/track split, so a placeholder
        // y/height (never read for this) keeps this a 1:1 mirror of
        // render.rs's real `slider_rect` without needing the row's y.
        // `zones()` speaks `node::Rect` (build/draw's shared vocabulary);
        // this module's own `graph_canvas::Rect` is screen-space only.
        let slider_rect =
            crate::node::Rect::new(slider_x, 0.0, (row_right - slider_x).max(0.0), 1.0);
        let metrics = crate::slider::SliderMetrics {
            label_width: PARAM_SLIDER_LABEL_W * self.zoom,
            value_box_w: PARAM_SLIDER_VALUE_BOX_W * self.zoom,
            gap: crate::slider::GAP * self.zoom,
            value_gap: crate::slider::VALUE_GAP * self.zoom,
        };
        Some(crate::slider::BitmapSlider::zones(slider_rect, &metrics).track.x)
    }

    /// Screen-space rect of a node's header "reveal sockets" chip — the small
    /// "+N" (hidden) / "−" (revealed) toggle at the header's right edge that
    /// shows / hides the node's unused sockets. `None` for a collapsed node or one
    /// with nothing hideable. Single geometry source for render + hit, and shifted
    /// left of the group-enter chevron on a group so they don't overlap.
    pub(crate) fn reveal_chip_rect(&self, viewport: Rect, node_id: u32) -> Option<Rect> {
        let node = self.find_node(node_id)?;
        if node.collapsed || node.hideable_ports == 0 {
            return None;
        }
        let (nx, ny) = self.to_screen(viewport, node.pos_graph.0, node.pos_graph.1);
        let sw = NODE_WIDTH * self.zoom;
        let header_h = NODE_HEADER_HEIGHT * self.zoom;
        let chip_w = 24.0 * self.zoom;
        let chip_h = header_h * 0.72;
        let right_margin = if node.is_group { 20.0 } else { 6.0 } * self.zoom;
        Some(Rect::new(
            nx + sw - right_margin - chip_w,
            ny + (header_h - chip_h) * 0.5,
            chip_w,
            chip_h,
        ))
    }

    /// Screen-space rect of an expanded `wgsl_compute` node's "Edit Code…"
    /// footer strip, or `None` for any node without a custom kernel (or a
    /// collapsed one). Built from [`NodeView::wgsl_footer_offset`] — the same
    /// geometry the renderer draws — so the click target can't drift from the
    /// drawn strip.
    pub(crate) fn wgsl_edit_rect(&self, viewport: Rect, node_id: u32) -> Option<Rect> {
        let node = self.find_node(node_id)?;
        let off = node.wgsl_footer_offset()?;
        let (nx, ny) = self.to_screen(viewport, node.pos_graph.0, node.pos_graph.1);
        Some(Rect::new(
            nx,
            ny + off * self.zoom,
            NODE_WIDTH * self.zoom,
            WGSL_FOOTER_H * self.zoom,
        ))
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
                // Skip a socket hidden as unused on an expanded node — it isn't
                // drawn, so it must not be a wire-drag target (its position would
                // otherwise fall back to row 0 and steal clicks there).
                if !node.collapsed && node.output_row_of(i).is_none() {
                    continue;
                }
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
                if !node.collapsed && node.input_row_of(i).is_none() {
                    continue;
                }
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
    /// geometry. Pure geometry — doesn't check `has_graph_mod`, so
    /// [`Self::save_to_project_button_rect`] / [`Self::save_to_library_button_rect`]
    /// can anchor off this slot whether or not Reset is currently shown.
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

    /// Bounding rect of the "Save to Project" header button — immediately
    /// left of the (always-reserved) Reset slot.
    pub(crate) fn save_to_project_button_rect(&self, viewport: Rect) -> Rect {
        let reset = self.reset_button_rect(viewport);
        Rect {
            x: reset.x - SAVE_BUTTON_GAP - SAVE_BUTTON_W,
            y: reset.y,
            w: SAVE_BUTTON_W,
            h: SAVE_BUTTON_H,
        }
    }

    /// Bounding rect of the "Save to Library" header button — immediately
    /// left of "Save to Project".
    pub(crate) fn save_to_library_button_rect(&self, viewport: Rect) -> Rect {
        let sp = self.save_to_project_button_rect(viewport);
        Rect {
            x: sp.x - SAVE_BUTTON_GAP - SAVE_BUTTON_W,
            y: sp.y,
            w: SAVE_BUTTON_W,
            h: SAVE_BUTTON_H,
        }
    }

    /// Bounding rect of the "Push to Library" header button
    /// (PRESET_LIBRARY_DESIGN D3, P4) — immediately left of "Save to
    /// Library". Only ever shown while diverged (`has_graph_mod`), same
    /// gate as "Reset to Default".
    pub(crate) fn push_to_library_button_rect(&self, viewport: Rect) -> Rect {
        let sl = self.save_to_library_button_rect(viewport);
        Rect {
            x: sl.x - SAVE_BUTTON_GAP - SAVE_BUTTON_W,
            y: sl.y,
            w: SAVE_BUTTON_W,
            h: SAVE_BUTTON_H,
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

// ── Automation surface (UI_AUTOMATION_DESIGN.md D5/§5) ───────────

/// `crate::node::Rect` conversion — [`HitTargetEntry`] carries the crate-wide
/// UI `Rect` (shared with the clip/automation surfaces), while the canvas'
/// internal geometry uses its own `graph_canvas::Rect`.
fn to_node_rect(r: Rect) -> crate::node::Rect {
    crate::node::Rect::new(r.x, r.y, r.w, r.h)
}

/// Stable domain-id string for a node at `scope_path` — the same
/// `(scope_path, u32 doc id)` addressing `project_graph_command_node_addressing`
/// already pins for graph edit commands (`GraphEditCommand`'s `scope_path` +
/// the node's runtime `u32` id), rendered as a greppable, parseable string
/// (no serde dependency in this crate — a struct payload would need one).
fn node_payload(scope: &[u32], node_id: u32) -> String {
    let scope_str = scope.iter().map(u32::to_string).collect::<Vec<_>>().join(",");
    format!("scope={scope_str}/node={node_id}")
}

/// [`HitTargets`] over a [`GraphCanvas`] at a given screen `viewport` — the
/// canvas needs an external viewport to convert its graph-space geometry to
/// screen rects (`to_screen`/`to_graph`, `camera.rs`), so unlike the
/// clip/automation surfaces (already screen-space at the call site) this
/// bundles `(canvas, viewport)` rather than implementing `HitTargets` on a
/// bare `&GraphCanvas`. Enumerates every node, port, and wire `node_under` /
/// `port_under` / `wire_into` can return at the canvas' *current* scope level
/// (nested groups are only visible by first descending into them, mirroring
/// what `hit_test` itself can reach).
pub struct GraphCanvasTargets<'a> {
    pub canvas: &'a GraphCanvas,
    pub viewport: Rect,
}

impl HitTargets for GraphCanvasTargets<'_> {
    fn surface_id(&self) -> &'static str {
        "graph_canvas"
    }

    fn enumerate(&self, out: &mut Vec<HitTargetEntry>) {
        let c = self.canvas;
        let scope = &c.scope;
        for node in &c.nodes {
            let (sx, sy) = c.to_screen(self.viewport, node.pos_graph.0, node.pos_graph.1);
            out.push(HitTargetEntry {
                kind: "node",
                label: node.title.clone(),
                rect: to_node_rect(Rect::new(sx, sy, NODE_WIDTH * c.zoom, node.height() * c.zoom)),
                payload: node_payload(scope, node.id),
            });

            let half = 6.0 * c.zoom;
            for (i, port) in node.outputs.iter().enumerate() {
                if !node.collapsed && node.output_row_of(i).is_none() {
                    continue; // hidden socket — not a hit_test target either
                }
                let (gx, gy) = node.output_port_pos_graph(i);
                let (px, py) = c.to_screen(self.viewport, gx, gy);
                out.push(HitTargetEntry {
                    kind: "port",
                    label: format!("{} → {}", node.title, port.name),
                    rect: to_node_rect(Rect::new(px - half, py - half, half * 2.0, half * 2.0)),
                    payload: format!(
                        "{}/port={}/dir=out",
                        node_payload(scope, node.id),
                        port.name
                    ),
                });
            }
            for (i, port) in node.inputs.iter().enumerate() {
                if !node.collapsed && node.input_row_of(i).is_none() {
                    continue;
                }
                let (gx, gy) = node.input_port_pos_graph(i);
                let (px, py) = c.to_screen(self.viewport, gx, gy);
                out.push(HitTargetEntry {
                    kind: "port",
                    label: format!("{} ← {}", node.title, port.name),
                    rect: to_node_rect(Rect::new(px - half, py - half, half * 2.0, half * 2.0)),
                    payload: format!(
                        "{}/port={}/dir=in",
                        node_payload(scope, node.id),
                        port.name
                    ),
                });
            }
        }

        for wire in &c.wires {
            let (Some(from), Some(to)) = (c.find_node(wire.from_node), c.find_node(wire.to_node))
            else {
                continue; // boundary/anonymous endpoint outside this scope's node list
            };
            let from_idx = from.outputs.iter().position(|p| p.name == wire.from_port).unwrap_or(0);
            let to_idx = to.inputs.iter().position(|p| p.name == wire.to_port).unwrap_or(0);
            let (fgx, fgy) = from.output_port_pos_graph(from_idx);
            let (tgx, tgy) = to.input_port_pos_graph(to_idx);
            let (fx, fy) = c.to_screen(self.viewport, fgx, fgy);
            let (tx, ty) = c.to_screen(self.viewport, tgx, tgy);
            let (x0, x1) = (fx.min(tx), fx.max(tx));
            let (y0, y1) = (fy.min(ty), fy.max(ty));
            out.push(HitTargetEntry {
                kind: "wire",
                label: format!("{}.{} → {}.{}", from.title, wire.from_port, to.title, wire.to_port),
                rect: to_node_rect(Rect::new(x0, y0, (x1 - x0).max(1.0), (y1 - y0).max(1.0))),
                payload: format!(
                    "scope={}/from={}:{}/to={}:{}",
                    scope.iter().map(u32::to_string).collect::<Vec<_>>().join(","),
                    wire.from_node,
                    wire.from_port,
                    wire.to_node,
                    wire.to_port
                ),
            });
        }
    }
}
