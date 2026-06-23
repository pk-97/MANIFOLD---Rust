//! Immediate-mode painting for the canvas — the master `render` plus every
//! `draw_*` helper. Goes through the [`Painter`] rect+text primitives; no UITree.
//! Reads node geometry from `NodeView` (the one geometry source) and projects
//! through `to_screen`/`to_graph` (camera) — it never recomputes layout.

use super::*;
use crate::draw::{Depth, Painter};

impl GraphCanvas {
    // ── Render ──────────────────────────────────────────────────────

    pub fn render(&self, ui: &mut dyn Painter, viewport: Rect) {
        // Clip every node, wire, and label this canvas draws to its own lane so
        // nothing bleeds under the left palette or right sidebar. The panels
        // build their own scissored batches on top via `render_tree_range`.
        ui.push_immediate_clip(viewport.x, viewport.y, viewport.w, viewport.h);
        ui.draw_rect(viewport.x, viewport.y, viewport.w, viewport.h, BG_COLOR);

        ui.draw_rect(viewport.x, viewport.y, viewport.w, HEADER_HEIGHT, HEADER_BG);
        if self.scope.is_empty() {
            let header_label = if self.nodes.is_empty() {
                "No active graph — open an effect card"
            } else if self.has_graph_mod {
                "Live Graph — MODIFIED"
            } else {
                "Live Graph"
            };
            ui.draw_text(
                viewport.x + 10.0,
                viewport.y + (HEADER_HEIGHT - 12.0) * 0.5,
                header_label,
                12.0,
                TEXT_HEADER,
            );
        } else {
            // Inside one or more groups — draw the breadcrumb trail instead.
            // The current (deepest) crumb is bright; ancestors dim, signalling
            // they're clickable jump targets.
            let text_y = viewport.y + (HEADER_HEIGHT - BREADCRUMB_FONT) * 0.5;
            let cw = BREADCRUMB_FONT * 0.55;
            for (_, r, label, is_current) in self.breadcrumb_segments(viewport) {
                let color = if is_current {
                    BREADCRUMB_TEXT
                } else {
                    BREADCRUMB_DIM
                };
                ui.draw_text(r.x, text_y, &label, BREADCRUMB_FONT, color);
                if !is_current {
                    ui.draw_text(r.x + r.w + cw, text_y, "›", BREADCRUMB_FONT, BREADCRUMB_DIM);
                }
            }
        }
        let zoom_text = format!("Zoom {:.0}%", self.zoom * 100.0);
        ui.draw_text(
            viewport.x + viewport.w - 90.0,
            viewport.y + (HEADER_HEIGHT - 11.0) * 0.5,
            &zoom_text,
            11.0,
            TEXT_SECONDARY,
        );

        // "Reset to Default" pill — only when the graph is diverged.
        if self.has_graph_mod {
            let rect = self.reset_button_rect(viewport);
            ui.draw_rect(rect.x, rect.y, rect.w, rect.h, RESET_BUTTON_BG);
            ui.draw_text(
                rect.x + 8.0,
                rect.y + (rect.h - 11.0) * 0.5,
                "Reset to Default",
                11.0,
                TEXT_HEADER,
            );
        }

        let canvas = Rect {
            x: viewport.x,
            y: viewport.y + HEADER_HEIGHT,
            w: viewport.w,
            h: (viewport.h - HEADER_HEIGHT).max(0.0),
        };
        if canvas.w <= 0.0 || canvas.h <= 0.0 {
            ui.pop_immediate_clip();
            return;
        }

        self.draw_grid(ui, canvas);

        // Wires in three back-to-front tiers so the image path reads over the
        // scalar fan and the focused node's connections read over everything:
        //   1. faded control/scalar wires (the orange driver fan)
        //   2. data wires (the actual image path)
        //   3. any wire touching the focused/hovered node
        // A faded control wire stored after a data wire used to paint *on top*
        // of it and muddy the path; ordering the draws fixes that for free —
        // same wires, same geometry, draw-order only.
        for wire in &self.wires {
            if !self.wire_touches_focus(wire) && self.wire_is_control(wire) {
                self.draw_wire(ui, viewport, wire);
            }
        }
        for wire in &self.wires {
            if !self.wire_touches_focus(wire) && !self.wire_is_control(wire) {
                self.draw_wire(ui, viewport, wire);
            }
        }
        for wire in &self.wires {
            if self.wire_touches_focus(wire) {
                self.draw_wire(ui, viewport, wire);
            }
        }

        // Ghost wire while the user is dragging from an output port.
        // Drawn beneath nodes so the wire passes "through" the cursor
        // visually if the cursor overlaps a node.
        if let DragMode::WireFrom {
            from_node,
            from_port,
        } = &self.drag_mode
        {
            self.draw_ghost_wire(ui, viewport, *from_node, from_port);
        }

        // Nodes draw at CONTENT depth so they sit *above* the wires (BASE).
        // The renderer batches all of a depth's rects before its lines, so
        // node bodies (rects) and wires (lines) at the same depth would force
        // every wire to paint over every body — the over-draw that made
        // connections cross the face of a node and its preview. Splitting them
        // across depths makes wires route behind nodes, the standard
        // node-editor read. The node-output thumbnail blit (a later present
        // pass) is already topmost, so it stays correctly over wires.
        ui.push_depth(Depth::CONTENT);

        // Nodes: everything else first, then the hovered node, then the
        // selected nodes last, so the node(s) you're working on are never
        // buried under their neighbours in a dense graph.
        for node in &self.nodes {
            if !self.selected.contains(&node.id) && self.hovered != Some(node.id) {
                self.draw_node(ui, viewport, canvas, node);
            }
        }
        if let Some(h) = self.hovered
            && !self.selected.contains(&h)
            && let Some(node) = self.find_node(h)
        {
            self.draw_node(ui, viewport, canvas, node);
        }
        for &s in &self.selected {
            if let Some(node) = self.find_node(s) {
                self.draw_node(ui, viewport, canvas, node);
            }
        }

        // Live rubber-band rectangle while marquee-selecting — over the nodes.
        if let DragMode::Marquee { origin_screen } = &self.drag_mode {
            let (ox, oy) = *origin_screen;
            let (cx, cy) = self.cursor;
            let x = ox.min(cx);
            let y = oy.min(cy);
            let w = (cx - ox).abs();
            let h = (cy - oy).abs();
            ui.draw_bordered_rect(x, y, w, h, MARQUEE_FILL, 0.0, 1.0, MARQUEE_BORDER);
        }

        ui.pop_depth();

        // Hover tooltip: the node's friendly summary, or — when the cursor is
        // over a param row — that param's help line. Drawn above the nodes, and
        // only when the canvas is idle (a tooltip chasing the cursor mid-drag
        // would be noise) and no popover is open.
        if matches!(self.drag_mode, DragMode::None) && !self.mapping_popover.is_open() {
            ui.push_depth(Depth::TOOLTIP);
            self.draw_hover_tooltip(ui, viewport, canvas);
            ui.pop_depth();
        }

        // Mapping popover floats above the nodes so its handles and buttons are
        // never buried under a node it overlaps — and above the CONTENT-depth
        // node text, so that text can't bleed through its face. It draws
        // unclipped (it may extend past the canvas lane) at POPOVER depth; the
        // lane clip is re-armed afterwards.
        ui.pop_immediate_clip();
        ui.push_depth(Depth::POPOVER);
        self.mapping_popover.render(ui);
        ui.pop_depth();
        ui.push_immediate_clip(viewport.x, viewport.y, viewport.w, viewport.h);

        // Debug overlay last, on top of everything — it's a diagnostic HUD.
        if self.debug_overlay {
            ui.push_depth(Depth::TOOLTIP);
            self.draw_debug_overlay(ui, canvas);
            ui.pop_depth();
        }

        ui.pop_immediate_clip();
    }

    /// Floating help card near the cursor: a param's help line when the
    /// cursor is over a param row, otherwise the hovered node's friendly
    /// summary. Both come from the doc side-channels (`param_doc` and
    /// `NodeDescriptor`) resolved at snapshot time. No-op when there's
    /// nothing registered for whatever the cursor is over.
    fn draw_hover_tooltip(&self, ui: &mut dyn Painter, viewport: Rect, canvas: Rect) {
        let (sx, sy) = self.cursor;
        // A param row under the cursor wins over the node summary — it's the
        // more specific thing the user is pointing at.
        let text: Option<&str> = self
            .param_row_under(viewport, sx, sy)
            .and_then(|(nid, idx)| {
                self.find_node(nid)
                    .and_then(|n| n.params.get(idx))
                    .and_then(|p| p.tooltip.as_deref())
            })
            .or_else(|| {
                self.hovered
                    .and_then(|h| self.find_node(h))
                    .and_then(|n| n.tooltip.as_deref())
            });
        let Some(text) = text else {
            return;
        };

        // Fixed screen-space sizing — a tooltip shouldn't shrink with zoom.
        const FONT: f32 = 11.0;
        const PAD: f32 = 7.0;
        const LINE_H: f32 = 14.0;
        const MAX_W: f32 = 300.0;
        let char_w = FONT * 0.55;
        let max_chars = ((MAX_W - 2.0 * PAD) / char_w).floor().max(1.0) as usize;
        let lines = wrap_text(text, max_chars);
        if lines.is_empty() {
            return;
        }
        let longest = lines.iter().map(|l| l.chars().count()).max().unwrap_or(0);
        let box_w = (longest as f32 * char_w + 2.0 * PAD).min(MAX_W);
        let box_h = lines.len() as f32 * LINE_H + 2.0 * PAD;

        // Sit below-right of the cursor, then flip/clamp so the box is never
        // clipped against the canvas edges.
        let mut x = sx + 16.0;
        let mut y = sy + 18.0;
        if x + box_w > canvas.x + canvas.w {
            x = (sx - box_w - 12.0).max(canvas.x + 2.0);
        }
        if y + box_h > canvas.y + canvas.h {
            y = (sy - box_h - 12.0).max(canvas.y + 2.0);
        }

        ui.draw_bordered_rect(x, y, box_w, box_h, TOOLTIP_BG, 4.0, 1.0, TOOLTIP_BORDER);
        for (i, line) in lines.iter().enumerate() {
            ui.draw_text(
                x + PAD,
                y + PAD + i as f32 * LINE_H,
                line,
                FONT,
                TOOLTIP_TEXT,
            );
        }
    }

    /// Corner HUD showing what the canvas thinks is happening: scope path,
    /// node/wire counts, selection, hover, and the active drag mode. Toggled
    /// by the backtick key. The handoff doc's debug-friendly mandate — Peter
    /// reads this instead of reaching for a debugger.
    fn draw_debug_overlay(&self, ui: &mut dyn Painter, canvas: Rect) {
        let lines = [
            format!("scope: {:?}", self.scope),
            format!("crumbs: {:?}", self.scope_titles),
            format!("nodes: {}   wires: {}", self.nodes.len(), self.wires.len()),
            format!("selected: {:?}   hovered: {:?}", self.selected, self.hovered),
            format!("drag: {}", self.drag_mode.debug_label()),
            format!(
                "zoom: {:.2}   pan: ({:.0}, {:.0})",
                self.zoom, self.pan.0, self.pan.1
            ),
        ];
        let size = 11.0;
        let line_h = 15.0;
        let pad = 6.0;
        let w = 380.0;
        let h = pad * 2.0 + lines.len() as f32 * line_h;
        let x = canvas.x + 8.0;
        let y = canvas.y + canvas.h - h - 8.0;
        ui.draw_rect(x, y, w, h, DEBUG_OVERLAY_BG);
        for (i, line) in lines.iter().enumerate() {
            ui.draw_text(
                x + pad,
                y + pad + i as f32 * line_h,
                line,
                size,
                DEBUG_OVERLAY_TEXT,
            );
        }
    }

    fn draw_ghost_wire(
        &self,
        ui: &mut dyn Painter,
        viewport: Rect,
        from_node: u32,
        from_port: &str,
    ) {
        let Some(node) = self.find_node(from_node) else {
            return;
        };
        let idx = match node.outputs.iter().position(|p| p.name == from_port) {
            Some(i) => i,
            None => return,
        };
        let (gx0, gy0) = node.output_port_pos_graph(idx);
        let (sx0, sy0) = self.to_screen(viewport, gx0, gy0);
        let (sx1, sy1) = self.cursor;

        // Same bezier shape as `draw_wire`, sampled lightly.
        let span_x = (sx1 - sx0).abs();
        let dx = span_x.max(40.0) * 0.5;
        let cx0 = sx0 + dx;
        let cy0 = sy0;
        let cx1 = sx1 - dx;
        let cy1 = sy1;
        let approx_len = ((sx1 - sx0).abs() + (sy1 - sy0).abs() + 2.0 * dx).max(40.0);
        let steps = (approx_len / 12.0).clamp(16.0, 64.0) as i32;
        let thickness = (1.4 * self.zoom).clamp(1.0, 2.2);
        // Ghost takes its colour from the from-port's kind so users
        // can tell what *kind* of wire they're about to make at a
        // glance — drag from a scalar output, drag a warm-orange
        // ghost. 0.55 alpha keeps it readable as "in flight". When the
        // cursor is over an input port, switch to a green/red compat
        // hint so a mis-wire reads before the drop.
        let port_color = node.outputs[idx].color;
        let ghost_color = match self.wire_drop_compat(viewport, from_node, port_color) {
            Some(true) => CONNECT_OK_COLOR,
            Some(false) => CONNECT_BAD_COLOR,
            None => [port_color[0], port_color[1], port_color[2], 0.55],
        };
        let mut prev = cubic_bezier(0.0, sx0, sy0, cx0, cy0, cx1, cy1, sx1, sy1);
        for i in 1..=steps {
            let t = i as f32 / steps as f32;
            let curr = cubic_bezier(t, sx0, sy0, cx0, cy0, cx1, cy1, sx1, sy1);
            ui.draw_line(prev.0, prev.1, curr.0, curr.1, thickness, ghost_color);
            prev = curr;
        }
    }

    fn draw_grid(&self, ui: &mut dyn Painter, canvas: Rect) {
        const GRAPH_SPACING: f32 = 32.0;
        let spacing = GRAPH_SPACING * self.zoom;
        if spacing < 8.0 {
            return;
        }
        let viewport = canvas_to_viewport(canvas);
        let (g_min_x, g_min_y) = self.to_graph(viewport, canvas.x, canvas.y);
        let start_gx = (g_min_x / GRAPH_SPACING).floor() * GRAPH_SPACING;
        let start_gy = (g_min_y / GRAPH_SPACING).floor() * GRAPH_SPACING;
        let mut gy = start_gy;
        while {
            let (_, sy) = self.to_screen(viewport, 0.0, gy);
            sy < canvas.y + canvas.h
        } {
            let mut gx = start_gx;
            while {
                let (sx, _) = self.to_screen(viewport, gx, 0.0);
                sx < canvas.x + canvas.w
            } {
                let (sx, sy) = self.to_screen(viewport, gx, gy);
                if sx >= canvas.x && sy >= canvas.y {
                    ui.draw_rect(sx - 1.0, sy - 1.0, 2.0, 2.0, GRID_DOT);
                }
                gx += GRAPH_SPACING;
            }
            gy += GRAPH_SPACING;
        }
    }

    /// Draw a node's primary-param sparkline (normalized 0..1 history) into the
    /// rect `(x, y, w, h)` as a soft polyline, y inverted so 1.0 sits at the top.
    /// Subtle by design — it signals "this knob is moving," it isn't a readout.
    fn draw_sparkline(
        &self,
        ui: &mut dyn Painter,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        hist: &std::collections::VecDeque<f32>,
    ) {
        let n = hist.len();
        if n < 2 || w <= 1.0 || h <= 1.0 {
            return;
        }
        let dx = w / (n - 1) as f32;
        let thickness = (1.0 * self.zoom).clamp(0.8, 1.6);
        let mut prev: Option<(f32, f32)> = None;
        for (i, &v) in hist.iter().enumerate() {
            let px = x + i as f32 * dx;
            let py = y + h - v.clamp(0.0, 1.0) * h;
            if let Some((px0, py0)) = prev {
                ui.draw_line(px0, py0, px, py, thickness, SPARKLINE_COLOR);
            }
            prev = Some((px, py));
        }
    }

    fn draw_node(&self, ui: &mut dyn Painter, viewport: Rect, canvas: Rect, node: &NodeView) {
        let (sx, sy) = self.to_screen(viewport, node.pos_graph.0, node.pos_graph.1);
        let sw = NODE_WIDTH * self.zoom;
        let sh = node.height() * self.zoom;
        if sx + sw < canvas.x || sx > canvas.x + canvas.w {
            return;
        }
        if sy + sh < canvas.y || sy > canvas.y + canvas.h {
            return;
        }

        let hovered = self.hovered == Some(node.id);
        let selected = self.selected.contains(&node.id);
        // Groups read as containers: a teal-washed body + a brighter accent
        // border so the eye picks out the boxes that "open".
        let bg = if node.is_group {
            if hovered { GROUP_BODY_BG_HOVER } else { GROUP_BODY_BG }
        } else if hovered {
            NODE_BG_HOVER
        } else {
            NODE_BG
        };
        let (border, border_w) = if selected {
            (NODE_BORDER_SELECTED, 2.0)
        } else if node.is_group {
            (GROUP_ACCENT, 1.5)
        } else {
            (NODE_BORDER, 1.0)
        };

        ui.draw_bordered_rect(
            sx,
            sy,
            sw,
            sh,
            bg,
            NODE_CORNER * self.zoom,
            border_w,
            border,
        );

        let header_h = NODE_HEADER_HEIGHT * self.zoom;
        let header_color = if node.is_group {
            // A per-group tint overrides the default group header, so a busy
            // graph reads as a few colour-coded boxes at a glance.
            node.group_tint.unwrap_or(GROUP_HEADER_BG)
        } else {
            node.header_color
        };
        ui.draw_rounded_rect(
            sx,
            sy,
            sw,
            header_h,
            header_color,
            NODE_CORNER * self.zoom,
        );

        let title_size = (11.0 * self.zoom).max(8.0);
        ui.draw_text(
            sx + 8.0 * self.zoom,
            sy + (header_h - title_size) * 0.5,
            &node.title,
            title_size,
            TEXT_HEADER,
        );

        // Below the LOD zoom, draw nothing in the body or header-right: the
        // node reads as a clean colour-coded box (text would be mush).
        let show_text = self.zoom >= PARAM_LOD_ZOOM;

        // Collapse chevron at the header's right edge, for nodes that have
        // params to fold. "+" collapsed (click to expand), "-" expanded.
        if show_text && !node.params.is_empty() {
            let chev_size = (11.0 * self.zoom).max(8.0);
            ui.draw_text(
                sx + sw - 14.0 * self.zoom,
                sy + (header_h - chev_size) * 0.5,
                if node.collapsed { "+" } else { "-" },
                chev_size,
                TEXT_SECONDARY,
            );
        }

        // Group "enter" chevron — signals the box opens on double-click.
        // Groups carry no on-face params, so this never collides with the
        // collapse chevron above.
        if show_text && node.is_group {
            let chev_size = (13.0 * self.zoom).max(9.0);
            ui.draw_text(
                sx + sw - 16.0 * self.zoom,
                sy + (header_h - chev_size) * 0.5,
                "›",
                chev_size,
                BREADCRUMB_TEXT,
            );
        }

        // Output-preview strip — a recessed 16:9 "screen" directly under the
        // header that the present pass blits this node's atlas thumbnail over.
        // Drawn for any node (or group) that emits an image, at every zoom, so
        // the strip is there before the first atlas frame lands and shows
        // through the letterbox bars of a non-16:9 output. Lives in its own band
        // above the param/port rows — ports never overlap it.
        let preview_h = node.preview_h() * self.zoom;
        if node.preview_node_id.is_some() {
            let pad = PREVIEW_PAD * self.zoom;
            ui.draw_bordered_rect(
                sx + pad,
                sy + header_h + pad,
                PREVIEW_IMG_W * self.zoom,
                PREVIEW_IMG_H * self.zoom,
                PREVIEW_SCREEN_BG,
                2.0 * self.zoom,
                1.0,
                PREVIEW_SCREEN_BORDER,
            );
        }
        // Top of the param/summary body — below the header and the preview band.
        let body_top = sy + header_h + preview_h;

        let row_h = PARAM_ROW_H * self.zoom;
        let text_size = (9.0 * self.zoom).max(7.0);
        let pad_x = 8.0 * self.zoom;
        let inner_w = sw - 2.0 * pad_x;

        // Collapsed: one summary line ("Mode: FoldX") plus, when the live tap
        // has been moving the node's primary knob, a small sparkline of its
        // recent history on the right — so a folded node still shows its key
        // value AND whether something is modulating it, without the full wall.
        if show_text && node.collapsed {
            let text_y = body_top + 2.0 * self.zoom;
            // Reserve the right edge for a sparkline if this node has a trace.
            let hist = self
                .spark_history
                .get(&node.node_id)
                .filter(|h| h.len() >= 2 && spark_has_variation(h));
            let spark_w = if hist.is_some() {
                (inner_w * 0.4).min(56.0 * self.zoom)
            } else {
                0.0
            };
            if let Some(hist) = hist {
                self.draw_sparkline(
                    ui,
                    sx + sw - pad_x - spark_w,
                    text_y,
                    spark_w,
                    row_h - 4.0 * self.zoom,
                    hist,
                );
            }
            if let Some(summary) = node.summary.as_deref() {
                let avail_w = (inner_w - spark_w - 4.0 * self.zoom).max(1.0);
                let max_chars = (avail_w / (text_size * 0.55)) as usize;
                let line: std::borrow::Cow<'_, str> =
                    if summary.chars().count() > max_chars && max_chars > 1 {
                        let take = max_chars.saturating_sub(1);
                        std::borrow::Cow::Owned(format!(
                            "{}…",
                            summary.chars().take(take).collect::<String>()
                        ))
                    } else {
                        std::borrow::Cow::Borrowed(summary)
                    };
                ui.draw_text(sx + pad_x, text_y, &line, text_size, TEXT_SECONDARY);
            }
        }

        // Expanded: every param row — label + value with a fill bar under
        // ranged values, each draggable in place (see ParamScrub).
        let expanded_params: &[ParamView] = if show_text && !node.collapsed {
            &node.params
        } else {
            &[]
        };
        for (i, p) in expanded_params.iter().enumerate() {
            let row_y = body_top + i as f32 * row_h;
            let text_y = row_y + 2.0 * self.zoom;

            // Value, right-aligned. Measured first so the label can be
            // truncated against the space the value leaves.
            let value_w = p.value.chars().count() as f32 * text_size * 0.55;
            ui.draw_text(
                sx + sw - pad_x - value_w,
                text_y,
                &p.value,
                text_size,
                TEXT_PRIMARY,
            );

            // Label, left, truncated so it can't collide with the value.
            let label_budget = (inner_w - value_w - 6.0 * self.zoom).max(0.0);
            let max_chars = (label_budget / (text_size * 0.55)) as usize;
            let label: std::borrow::Cow<'_, str> = if p.label.chars().count() > max_chars
                && max_chars > 1
            {
                let take = max_chars.saturating_sub(1);
                std::borrow::Cow::Owned(format!(
                    "{}…",
                    p.label.chars().take(take).collect::<String>()
                ))
            } else {
                std::borrow::Cow::Borrowed(p.label.as_str())
            };
            ui.draw_text(sx + pad_x, text_y, &label, text_size, TEXT_SECONDARY);

            // Fill bar under the row for ranged values.
            if let Some(frac) = p.fill {
                let bar_h = 2.0 * self.zoom;
                let bar_y = row_y + row_h - bar_h - 2.0 * self.zoom;
                ui.draw_rounded_rect(sx + pad_x, bar_y, inner_w, bar_h, PARAM_FILL_BG, bar_h * 0.5);
                let fill_w = inner_w * frac;
                if fill_w > 0.0 {
                    ui.draw_rounded_rect(sx + pad_x, bar_y, fill_w, bar_h, PARAM_FILL_FG, bar_h * 0.5);
                }
            }
        }

        let port_label_size = (10.0 * self.zoom).max(7.0);
        let port_d = PORT_RADIUS * 2.0 * self.zoom;
        for (i, port) in node.inputs.iter().enumerate() {
            let (px, py) = node.input_port_pos_graph(i);
            let (psx, psy) = self.to_screen(viewport, px, py);
            ui.draw_rounded_rect(
                psx - PORT_RADIUS * self.zoom,
                psy - PORT_RADIUS * self.zoom,
                port_d,
                port_d,
                port.color,
                PORT_RADIUS * self.zoom,
            );
            ui.draw_text(
                psx + PORT_COL_WIDTH * self.zoom,
                psy - port_label_size * 0.5,
                &port.name,
                port_label_size,
                TEXT_PRIMARY,
            );
        }
        for (i, port) in node.outputs.iter().enumerate() {
            let (px, py) = node.output_port_pos_graph(i);
            let (psx, psy) = self.to_screen(viewport, px, py);
            ui.draw_rounded_rect(
                psx - PORT_RADIUS * self.zoom,
                psy - PORT_RADIUS * self.zoom,
                port_d,
                port_d,
                port.color,
                PORT_RADIUS * self.zoom,
            );
            let approx_w = port.name.len() as f32 * port_label_size * 0.55;
            ui.draw_text(
                psx - PORT_COL_WIDTH * self.zoom - approx_w,
                psy - port_label_size * 0.5,
                &port.name,
                port_label_size,
                TEXT_PRIMARY,
            );
        }

        // Find-a-node: dim nodes that don't match the active search so the
        // matches stay bright and jump out of a busy graph. Drawn last, over the
        // node's own content.
        if !self.node_search.is_empty() && !self.node_matches_search(node) {
            ui.draw_rect(sx, sy, sw, sh, [0.05, 0.05, 0.07, 0.66]);
        }
    }

    /// Whether a wire connects to the focused node (selected or hovered).
    /// Such wires draw last and at full strength so the focused node's
    /// connections stand out from the rest of the graph.
    fn wire_touches_focus(&self, wire: &WireView) -> bool {
        self.selected.contains(&wire.from_node)
            || self.selected.contains(&wire.to_node)
            || self.hovered == Some(wire.from_node)
            || self.hovered == Some(wire.to_node)
    }

    /// Whether a wire carries a control/scalar value (orange) rather than image
    /// data — resolved from the source output port's kind, the same way
    /// `draw_wire` decides its alpha. Drives the back-to-front draw tier so the
    /// faded driver fan sits *under* the image path.
    fn wire_is_control(&self, wire: &WireView) -> bool {
        self.find_node(wire.from_node)
            .and_then(|f| f.outputs.iter().find(|p| p.name == wire.from_port))
            .is_some_and(|p| p.is_control)
    }

    fn draw_wire(&self, ui: &mut dyn Painter, viewport: Rect, wire: &WireView) {
        let (Some(from), Some(to)) = (self.find_node(wire.from_node), self.find_node(wire.to_node))
        else {
            return;
        };
        let from_idx = from
            .outputs
            .iter()
            .position(|p| p.name == wire.from_port)
            .unwrap_or(0);
        let to_idx = to
            .inputs
            .iter()
            .position(|p| p.name == wire.to_port)
            .unwrap_or(0);
        let (gx0, gy0) = from.output_port_pos_graph(from_idx);
        let (gx1, gy1) = to.input_port_pos_graph(to_idx);
        let (sx0, sy0) = self.to_screen(viewport, gx0, gy0);
        let (sx1, sy1) = self.to_screen(viewport, gx1, gy1);

        let span_x = (sx1 - sx0).abs();
        let span_y = (sy1 - sy0).abs();
        let focused = self.wire_touches_focus(wire);
        // A wire terminating on a cycle-breaking node is a feedback RETURN path
        // (the "Previous X" recurrent-state taps). It's drawn as a deliberate
        // return — muted violet, dashed, routed over the top — so the loop
        // topology reads instead of looking like a wrong-direction data wire.
        // Layout still excludes it (auto_layout skips these), so this is
        // purely cosmetic — the endpoints are untouched ground truth.
        let is_return = to.breaks_dependency_cycle;

        // ── Colour + alpha ──
        // Forward wires take the from-port's kind colour (matching the port
        // circles); control/scalar wires (orange) fade to a faint baseline
        // unless focused; data wires stay readable. Return paths get one
        // violet family regardless of port kind, dimmer than data but above
        // the control fan. Any focused wire lights to full.
        let is_control = from.outputs[from_idx].is_control;
        let port_color = from.outputs[from_idx].color;
        let (base_rgb, alpha): ([f32; 3], f32) = if is_return {
            (RETURN_WIRE_COLOR, if focused { 0.95 } else { 0.34 })
        } else {
            let a = if focused {
                0.95
            } else if is_control {
                0.16
            } else {
                0.7
            };
            ([port_color[0], port_color[1], port_color[2]], a)
        };
        let wire_color = [base_rgb[0], base_rgb[1], base_rgb[2], alpha];

        // ── Control points ──
        let (cx0, cy0, cx1, cy1) = if is_return {
            // Route up and OVER the node band: the source is downstream
            // (right) and target upstream (left), so the curve sweeps
            // right-to-left along the top — visibly "around", not "through".
            // Endpoints stay on their port dots; only the interior controls
            // move. (No skip_bump — it pulls down, fighting the arc.)
            let top_g = from.pos_graph.1.min(to.pos_graph.1);
            let (_, top_s) = self.to_screen(viewport, from.pos_graph.0, top_g);
            let arc_y = top_s - RETURN_ARC_CLEAR * self.zoom;
            let dx = span_x.max(40.0) * 0.3;
            (sx0 + dx, arc_y, sx1 - dx, arc_y)
        } else {
            // Forward wire. Handle reach grows with the vertical drop so steep
            // wires leave/enter more horizontally — a clean S instead of a
            // near-straight diagonal — which peels apart the fan-in to a
            // many-input node. On high-fan-in nodes, stagger the landing
            // handle by port depth so converging wires splay into the input
            // stack instead of overlapping for the last stretch. Long wires
            // still bump downward to read as going around intermediates.
            let dx = (span_x.max(40.0) * 0.5 + span_y * 0.35).min(span_x.max(160.0));
            let skip_bump = if span_x > 320.0 {
                ((span_x - 320.0) * 0.25).min(80.0)
            } else {
                0.0
            };
            let to_count = to.inputs.len();
            let land_dx = if to_count >= FANIN_STAGGER_MIN {
                let frac = if to_count > 1 {
                    to_idx as f32 / (to_count - 1) as f32
                } else {
                    0.0
                };
                dx * (0.6 + 0.8 * frac)
            } else {
                dx
            };
            (sx0 + dx, sy0 + skip_bump, sx1 - land_dx, sy1 + skip_bump)
        };

        // Sample the bezier into short line segments. Step count scales with
        // screen-space extent (endpoints + control reach) so close-up curves
        // and the tall return arc both stay smooth.
        let approx_len = (span_x
            + span_y
            + (cy0 - sy0).abs()
            + (cy1 - sy1).abs()
            + (cx0 - sx0).abs()
            + (sx1 - cx1).abs())
        .max(40.0);
        let steps = (approx_len / 12.0).clamp(16.0, 80.0) as i32;
        let thickness = (1.6 * self.zoom).clamp(1.2, 2.4) * if focused { 1.5 } else { 1.0 };
        let mut prev = cubic_bezier(0.0, sx0, sy0, cx0, cy0, cx1, cy1, sx1, sy1);
        for i in 1..=steps {
            let t = i as f32 / steps as f32;
            let curr = cubic_bezier(t, sx0, sy0, cx0, cy0, cx1, cy1, sx1, sy1);
            // Dash return paths (3 segments on, 3 off) so they read as
            // feedback at a glance; advance `prev` every step regardless so
            // the gaps land on the curve.
            if !is_return || (i / RETURN_DASH) % 2 == 0 {
                ui.draw_line(prev.0, prev.1, curr.0, curr.1, thickness, wire_color);
            }
            prev = curr;
        }
    }
}

pub(crate) fn canvas_to_viewport(canvas: Rect) -> Rect {
    Rect {
        x: canvas.x,
        y: canvas.y - HEADER_HEIGHT,
        w: canvas.w,
        h: canvas.h + HEADER_HEIGHT,
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn cubic_bezier(
    t: f32,
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
    x2: f32,
    y2: f32,
    x3: f32,
    y3: f32,
) -> (f32, f32) {
    let u = 1.0 - t;
    let b0 = u * u * u;
    let b1 = 3.0 * u * u * t;
    let b2 = 3.0 * u * t * t;
    let b3 = t * t * t;
    (
        b0 * x0 + b1 * x1 + b2 * x2 + b3 * x3,
        b0 * y0 + b1 * y1 + b2 * y2 + b3 * y3,
    )
}
