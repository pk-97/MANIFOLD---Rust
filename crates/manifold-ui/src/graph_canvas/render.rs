//! Immediate-mode painting for the canvas — the master `render` plus every
//! `draw_*` helper. Goes through the [`Painter`] rect+text primitives; no UITree.
//! Reads node geometry from `NodeView` (the one geometry source) and projects
//! through `to_screen`/`to_graph` (camera) — it never recomputes layout.

use super::*;
use crate::chrome::Theme;
use crate::draw::{Depth, Painter};
use crate::slider::BitmapSlider;

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

        // Save to Library / Save to Project (PRESET_LIBRARY_DESIGN D4, P3) —
        // only when there's an active graph to save (mirrors the header
        // label's own "No active graph" gate).
        if !self.nodes.is_empty() {
            let sp_rect = self.save_to_project_button_rect(viewport);
            ui.draw_rect(sp_rect.x, sp_rect.y, sp_rect.w, sp_rect.h, SAVE_BUTTON_BG);
            ui.draw_text(
                sp_rect.x + 8.0,
                sp_rect.y + (sp_rect.h - 11.0) * 0.5,
                "Save to Project",
                11.0,
                TEXT_HEADER,
            );

            let sl_rect = self.save_to_library_button_rect(viewport);
            ui.draw_rect(sl_rect.x, sl_rect.y, sl_rect.w, sl_rect.h, SAVE_BUTTON_BG);
            ui.draw_text(
                sl_rect.x + 8.0,
                sl_rect.y + (sl_rect.h - 11.0) * 0.5,
                "Save to Library",
                11.0,
                TEXT_HEADER,
            );
        }

        // Push to Library (PRESET_LIBRARY_DESIGN D3, P4) — only when
        // diverged (mirrors "Reset to Default"'s own gate; pushing an
        // undiverged card would overwrite the library file with itself).
        if self.has_graph_mod {
            let pl_rect = self.push_to_library_button_rect(viewport);
            ui.draw_rect(pl_rect.x, pl_rect.y, pl_rect.w, pl_rect.h, SAVE_BUTTON_BG);
            ui.draw_text(
                pl_rect.x + 8.0,
                pl_rect.y + (pl_rect.h - 11.0) * 0.5,
                "Push to Library",
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
        //
        // D8 same-pair ribbons (`docs/SCENE_BUILD_AND_GROUP_PARAMS_DESIGN.md`
        // §2): tiers 1/2 collapse ≥2 wires sharing a (from_node, to_node)
        // pair into one ribbon + an "×N" badge. Tier 3 (focused) is the
        // "expanded" state by construction — a pair with either endpoint
        // hovered/selected has ALL its members filtered into this tier by
        // `wire_touches_focus`, so it never reaches tiers 1/2 and always
        // draws per-wire — satisfying "hover / endpoint-selected expands to
        // individuals" with no extra state.
        self.draw_wire_tier(ui, viewport, |w| !self.wire_touches_focus(w) && self.wire_is_control(w));
        self.draw_wire_tier(ui, viewport, |w| !self.wire_touches_focus(w) && !self.wire_is_control(w));
        for wire in &self.wires {
            if self.wire_touches_focus(wire) {
                self.draw_wire(ui, viewport, wire);
            }
        }

        // Ghost wire while the user is dragging from an output port.
        // Drawn beneath nodes so the wire passes "through" the cursor
        // visually if the cursor overlaps a node.
        if let Some(CanvasDrag::WireFrom {
            from_node,
            from_port,
        }) = self.drag.payload()
        {
            self.draw_ghost_wire(ui, viewport, *from_node, from_port);
        }

        // D17 "flow pulse": one dash traveling source→dest, fired the
        // instant a `ConnectPorts` commits (`fire_wire_flow_pulse`).
        if let Some(p) = self.wire_flow_pulse.progress() {
            self.draw_wire_flow_pulse(ui, p);
        }

        // Nodes draw ABOVE the wires (BASE) — but each node now gets its OWN
        // increasing depth band (CONTENT+1, +2, …) in draw order, not one shared
        // CONTENT depth. Within a band the renderer draws the body (rect), then
        // the output preview (image), then labels (text); across bands a node
        // drawn later sits entirely above an earlier one. A single shared depth
        // would batch every body before every preview — putting all previews on
        // top of all bodies regardless of node stacking. Wires
        // stay below CONTENT, so they still route behind every node.
        //
        // Draw order (low band → high band): everything else first, then the
        // hovered node, then the selected nodes last, so the node(s) you're
        // working on are never buried under their neighbours. Bands are capped
        // just below OVERLAY(200); in a graph past ~98 nodes the overflow shares
        // the top band and falls back to submission-order stacking (bodies were
        // always submission-ordered; only previews degrade, and only that far out).
        const MAX_NODE_BANDS: usize = (Depth::OVERLAY.0 - Depth::CONTENT.0 - 1) as usize;
        let mut order: Vec<&NodeView> = Vec::with_capacity(self.nodes.len());
        for node in &self.nodes {
            if !self.selected.contains(&node.id) && self.hovered != Some(node.id) {
                order.push(node);
            }
        }
        if let Some(h) = self.hovered
            && !self.selected.contains(&h)
            && let Some(node) = self.find_node(h)
        {
            order.push(node);
        }
        for &s in &self.selected {
            if let Some(node) = self.find_node(s) {
                order.push(node);
            }
        }
        for (i, node) in order.iter().enumerate() {
            let band = Depth::CONTENT.0 + 1 + i.min(MAX_NODE_BANDS) as i32;
            ui.push_depth(Depth(band));
            self.draw_node(ui, viewport, canvas, node);
            ui.pop_depth();
        }

        // Live rubber-band rectangle while marquee-selecting — above every node
        // band. D17 "marquee fade in/out": `marquee_alpha` eases 0..1 (see
        // `GraphCanvas::tick`), so the rect fades in on press and fades out
        // after release instead of popping/vanishing instantly.
        // `marquee_last_rect` keeps the geometry available for the fade-OUT
        // frames, after `drag_mode` has already reset to `None`.
        let alpha = self.marquee_alpha.value();
        if alpha > 0.001
            && let Some((x, y, w, h)) = self.marquee_last_rect
        {
            let top_band = Depth::CONTENT.0 + 1 + order.len().min(MAX_NODE_BANDS) as i32;
            ui.push_depth(Depth(top_band));
            // Scale each color's OWN baked-in alpha by the fraction — not
            // replace it — so the fully-faded-in state matches the original
            // (already-subtle) fill/border alpha exactly, not full opacity.
            let scale = |c: Color32| color::with_alpha(c, (c.a as f32 * alpha).round() as u8);
            ui.draw_bordered_rect(x, y, w, h, scale(MARQUEE_FILL), 0.0, 1.0, scale(MARQUEE_BORDER));
            ui.pop_depth();
        }

        // Hover tooltip: the node's friendly summary, or — when the cursor is
        // over a param row — that param's help line. Drawn above the nodes, and
        // only when the canvas is idle (a tooltip chasing the cursor mid-drag
        // would be noise) and no popover is open.
        if !self.drag.is_active() && !self.mapping_popover.is_open() {
            ui.push_depth(Depth::TOOLTIP);
            self.draw_hover_tooltip(ui, viewport, canvas);
            ui.pop_depth();
        }

        // D17 connect-pop / error-shake — drawn at TOOLTIP depth so they
        // read above nodes/wires regardless of where the drop landed.
        if self.connect_pop.progress().is_some() || self.error_shake.progress().is_some() {
            ui.push_depth(Depth::TOOLTIP);
            self.draw_connect_pop(ui);
            self.draw_error_shake(ui);
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
        self.render_enum_dropdown(ui);
        self.render_vec_editor(ui);
        self.render_table_editor(ui);
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

    /// D17 "wire→port ... pop" (partial — see `GraphCanvas::tick`'s doc
    /// comment for what's scoped out). A brief expanding, fading ring at the
    /// drop point on a successful `ConnectPorts` commit.
    fn draw_connect_pop(&self, ui: &mut dyn Painter) {
        let Some(p) = self.connect_pop.progress() else {
            return;
        };
        let (cx, cy) = self.connect_pop_pos;
        let radius = 4.0 + 10.0 * p;
        let alpha = ((1.0 - p) * 255.0).round() as u8;
        let ring = color::with_alpha(color::ACCENT_BLUE_C32, alpha);
        ui.draw_bordered_rect(
            cx - radius,
            cy - radius,
            radius * 2.0,
            radius * 2.0,
            Color32::TRANSPARENT,
            radius,
            2.0,
            ring,
        );
    }

    /// D17 error shake — a small red X at the drop point, with a decaying
    /// horizontal jitter (a couple of oscillations, settled by the end of
    /// the transient) when a wire is dropped somewhere invalid.
    fn draw_error_shake(&self, ui: &mut dyn Painter) {
        let Some(p) = self.error_shake.progress() else {
            return;
        };
        let (cx, cy) = self.error_shake_pos;
        let shake_x = (p * std::f32::consts::PI * 4.0).sin() * 3.0 * (1.0 - p);
        let alpha = ((1.0 - p) * 255.0).round() as u8;
        let mark = color::with_alpha(color::RED_BASE, alpha);
        let r = 6.0;
        ui.draw_line(cx - r + shake_x, cy - r, cx + r + shake_x, cy + r, 2.0, mark);
        ui.draw_line(cx - r + shake_x, cy + r, cx + r + shake_x, cy - r, 2.0, mark);
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
            .and_then(|(nid, idx)| self.find_node(nid).and_then(|n| n.params.get(idx)))
            // D5: a wire-driven row's source ("driven by <node>.<port>")
            // outranks its static help line — once the row is read-only,
            // knowing what's driving it is the more useful hover fact.
            .and_then(|p| p.driven_by.as_deref().or(p.tooltip.as_deref()))
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
            format!(
                "drag: {}",
                self.drag.payload().map(CanvasDrag::debug_label).unwrap_or("none")
            ),
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
        // D17 "wire→port magnetize": the endpoint eases toward a nearby
        // input port instead of tracking the raw cursor 1:1 (see
        // `tick_wire_magnet`).
        let (sx1, sy1) = self.wire_ghost_endpoint();

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
            // Port-kind tint at 0.55 alpha ("in flight"). 0.55 * 255 ≈ 140.
            None => port_color.with_alpha(140),
        };
        let mut prev = cubic_bezier(0.0, sx0, sy0, cx0, cy0, cx1, cy1, sx1, sy1);
        for i in 1..=steps {
            let t = i as f32 / steps as f32;
            let curr = cubic_bezier(t, sx0, sy0, cx0, cy0, cx1, cy1, sx1, sy1);
            ui.draw_line(prev.0, prev.1, curr.0, curr.1, thickness, ghost_color);
            prev = curr;
        }
    }

    /// D17 "flow pulse": one bright dot traveling `wire_flow_pulse_from` →
    /// `wire_flow_pulse_to` at `progress` (0..1) along the SAME simplified
    /// bezier shape `draw_ghost_wire` uses (not `draw_wire`'s fuller
    /// fan-in-staggered curve — a brief traveling dash reading close to the
    /// wire is enough; it doesn't need to land pixel-exact on it).
    fn draw_wire_flow_pulse(&self, ui: &mut dyn Painter, progress: f32) {
        let (sx0, sy0) = self.wire_flow_pulse_from;
        let (sx1, sy1) = self.wire_flow_pulse_to;
        let span_x = (sx1 - sx0).abs();
        let dx = span_x.max(40.0) * 0.5;
        let (cx0, cy0, cx1, cy1) = (sx0 + dx, sy0, sx1 - dx, sy1);
        let (px, py) = cubic_bezier(progress.clamp(0.0, 1.0), sx0, sy0, cx0, cy0, cx1, cy1, sx1, sy1);
        // Fades in the first quarter, holds, fades out the last quarter —
        // a clean pop-and-travel rather than an abrupt appear/disappear.
        let alpha = if progress < 0.25 {
            progress / 0.25
        } else if progress > 0.75 {
            (1.0 - progress) / 0.25
        } else {
            1.0
        };
        // Same "rounded square = dot" primitive `draw_port_dot` uses — this
        // file has no separate filled-circle draw call.
        let d = (7.0 * self.zoom).clamp(4.0, 12.0);
        self.draw_port_dot(ui, px, py, d, CONNECT_OK_COLOR.with_alpha((217.0 * alpha) as u8));
    }

    /// A sparse reference grid, drawn at `GRID_SPACING * GRID_LINE_EVERY` —
    /// coarser than the finer increment node dragging actually snaps to
    /// (`DragMode::NodeMove`, `snap_to_grid`), so it reads as a light visual
    /// aid rather than a line for every snap step.
    fn draw_grid(&self, ui: &mut dyn Painter, canvas: Rect) {
        let draw_spacing = GRID_SPACING * GRID_LINE_EVERY;
        let spacing = draw_spacing * self.zoom;
        if spacing < 8.0 {
            return;
        }
        let viewport = canvas_to_viewport(canvas);
        let (g_min_x, g_min_y) = self.to_graph(viewport, canvas.x, canvas.y);
        let start_gx = (g_min_x / draw_spacing).floor() * draw_spacing;
        let start_gy = (g_min_y / draw_spacing).floor() * draw_spacing;
        let line_w = 1.0;

        let mut gy = start_gy;
        while {
            let (_, sy) = self.to_screen(viewport, 0.0, gy);
            sy < canvas.y + canvas.h
        } {
            let (_, sy) = self.to_screen(viewport, 0.0, gy);
            if sy >= canvas.y {
                ui.draw_line(canvas.x, sy, canvas.x + canvas.w, sy, line_w, GRID_LINE);
            }
            gy += draw_spacing;
        }
        let mut gx = start_gx;
        while {
            let (sx, _) = self.to_screen(viewport, gx, 0.0);
            sx < canvas.x + canvas.w
        } {
            let (sx, _) = self.to_screen(viewport, gx, 0.0);
            if sx >= canvas.x {
                ui.draw_line(sx, canvas.y, sx, canvas.y + canvas.h, line_w, GRID_LINE);
            }
            gx += draw_spacing;
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

        // All node text scales linearly with zoom — no font-size floor. A
        // floor made the text out-scale its shrinking box on zoom-out and
        // overlap into mush; letting it scale (and clipping to the box, below)
        // is the real zoom. Titles are elided to the header width so a long
        // node name can't run past the chevron.
        let title_size = 11.0 * self.zoom;
        let title = elide_to_width(&node.title, title_size, sw - 22.0 * self.zoom);
        ui.draw_text(
            sx + 8.0 * self.zoom,
            sy + (header_h - title_size) * 0.5,
            &title,
            title_size,
            TEXT_HEADER,
        );

        // No collapse +/- toggle — nodes default to (and stay) expanded, like
        // Blender. You read a node's params where it lives; there's no folded
        // state to hunt for. (The collapse machinery remains latent for a
        // possible future gesture, but no header affordance drives it.)

        // Group "enter" chevron — signals the box opens on double-click.
        if node.is_group {
            let chev_size = 13.0 * self.zoom;
            ui.draw_text(
                sx + sw - 16.0 * self.zoom,
                sy + (header_h - chev_size) * 0.5,
                "›",
                chev_size,
                BREADCRUMB_TEXT,
            );
        }

        // "Reveal unused sockets" chip: "+N" when the node is hiding N unwired
        // sockets, "−" when they're revealed (click to re-hide). Only when the
        // node has something hideable. Geometry via `reveal_chip_rect`, the same
        // source the hit-test reads, so the drawn chip and click target agree.
        if let Some(chip) = self.reveal_chip_rect(viewport, node.id) {
            ui.draw_rounded_rect(chip.x, chip.y, chip.w, chip.h, REVEAL_CHIP_BG, 3.0 * self.zoom);
            let cs = 9.0 * self.zoom;
            let label = if node.revealed {
                "−".to_string()
            } else {
                format!("+{}", node.hideable_ports)
            };
            let lw = text_width(&label, cs);
            ui.draw_text(
                chip.x + (chip.w - lw) * 0.5,
                chip.y + (chip.h - cs) * 0.5,
                &label,
                cs,
                TEXT_PRIMARY,
            );
        }

        // Output-preview screen — a recessed "screen" directly under the header
        // that the present pass blits this node's atlas thumbnail over. Sized to
        // the project aspect ratio (portrait shows get a portrait screen), so a
        // non-16:9 output fills it instead of sitting letterboxed in a fixed box.
        // Drawn for any node (or group) that emits an image, at every zoom, so
        // the screen is there before the first atlas frame lands. Lives in its
        // own band above the param/port rows — ports never overlap it. A screen
        // narrower than the node is centred in the band.
        let preview_h = node.preview_h() * self.zoom;
        if let Some((screen_w, screen_h)) = node.preview_screen {
            let pad = PREVIEW_PAD * self.zoom;
            let sw_z = screen_w * self.zoom;
            let sh_z = screen_h * self.zoom;
            let screen_x = sx + pad + (PREVIEW_IMG_W * self.zoom - sw_z) * 0.5;
            let screen_y = sy + header_h + pad;
            let corner = 2.0 * self.zoom;
            ui.draw_bordered_rect(
                screen_x,
                screen_y,
                sw_z,
                sh_z,
                PREVIEW_SCREEN_BG,
                corner,
                1.0,
                PREVIEW_SCREEN_BORDER,
            );
            // The live output preview, painted inline over the recessed screen at
            // this node's depth band — so a node stacked above occludes it.
            // The host populates `node_preview_src`
            // each frame; `None` leaves just the recessed placeholder, exactly as
            // before the first atlas frame lands. Edge-straddle clipping is the
            // canvas viewport scissor (`push_immediate_clip` in `render`), so no
            // per-node UV cropping is needed here.
            if let Some(capture_id) = node.preview_node_id.as_ref()
                && let Some((handle, uv)) = self.node_preview_src.get(capture_id)
            {
                ui.draw_image_uv(screen_x, screen_y, sw_z, sh_z, *handle, *uv, corner);
            }
        }
        // Top of the param/summary body — below the header and the preview band.
        let body_top = sy + header_h + preview_h;

        let row_h = PARAM_ROW_H * self.zoom;
        // Same base size the inspector card's sliders/params use
        // (`color::FONT_BODY`), just zoom-scaled — the canvas is the one
        // context where text has to shrink/grow with the view.
        let text_size = color::FONT_BODY as f32 * self.zoom;
        let pad_x = 8.0 * self.zoom;
        let inner_w = sw - 2.0 * pad_x;

        // Collapsed: one summary line ("Mode: FoldX") plus, when the live tap
        // has been moving the node's primary knob, a small sparkline of its
        // recent history on the right — so a folded node still shows its key
        // value AND whether something is modulating it, without the full wall.
        if node.collapsed {
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
                let line = elide_to_width(summary, text_size, avail_w);
                ui.draw_text(sx + pad_x, text_y, &line, text_size, TEXT_SECONDARY);
            }
        }

        // Port label metrics, shared by the collapsed band and the expanded
        // rows. Labels elide to half the node width so a long name clips instead
        // of crossing into the opposite column.
        let port_label_size = 10.0 * self.zoom;
        let port_label_budget = (sw * 0.5 - (PORT_COL_WIDTH + 2.0) * self.zoom).max(0.0);
        let port_d = PORT_RADIUS * 2.0 * self.zoom;

        if node.collapsed {
            // ── Collapsed: the compact port band (inputs left / outputs right).
            // Params are hidden; you still need the sockets to wire a folded node.
            for (i, port) in node.inputs.iter().enumerate() {
                let (px, py) = node.input_port_pos_graph(i);
                let (psx, psy) = self.to_screen(viewport, px, py);
                self.draw_port_dot(ui, psx, psy, port_d, port.color);
                let name = elide_to_width(&port.name, port_label_size, port_label_budget);
                ui.draw_text(
                    psx + PORT_COL_WIDTH * self.zoom,
                    psy - port_label_size * 0.5,
                    &name,
                    port_label_size,
                    TEXT_PRIMARY,
                );
            }
            for (i, port) in node.outputs.iter().enumerate() {
                let (px, py) = node.output_port_pos_graph(i);
                let (psx, psy) = self.to_screen(viewport, px, py);
                self.draw_port_dot(ui, psx, psy, port_d, port.color);
                let name = elide_to_width(&port.name, port_label_size, port_label_budget);
                let approx_w = text_width(&name, port_label_size);
                ui.draw_text(
                    psx - PORT_COL_WIDTH * self.zoom - approx_w,
                    psy - port_label_size * 0.5,
                    &name,
                    port_label_size,
                    TEXT_PRIMARY,
                );
            }
        } else {
            // ── Expanded: one row per NodeRow, Blender-style. Outputs (dot on
            // the right), then params (their shadowing input socket inline on the
            // left + an expose checkbox + value + fill bar), then leftover inputs.
            for (i, row) in node.rows.iter().enumerate() {
                let row_y = body_top + i as f32 * row_h;
                // Centered in the row, matching the expose checkbox
                // (`expose_glyph_bounds`) and the port dots (`expanded_row_center`)
                // — a fixed top offset read off-centre once the row pitch grew to
                // make room for card-matching row spacing.
                let text_y = row_y + (row_h - text_size) * 0.5;
                match *row {
                    NodeRow::Output { port } => {
                        let Some(p) = node.outputs.get(port) else {
                            continue;
                        };
                        let (px, py) = node.output_port_pos_graph(port);
                        let (psx, psy) = self.to_screen(viewport, px, py);
                        self.draw_port_dot(ui, psx, psy, port_d, p.color);
                        let name = elide_to_width(&p.name, port_label_size, port_label_budget);
                        let approx_w = text_width(&name, port_label_size);
                        ui.draw_text(
                            psx - PORT_COL_WIDTH * self.zoom - approx_w,
                            psy - port_label_size * 0.5,
                            &name,
                            port_label_size,
                            TEXT_PRIMARY,
                        );
                    }
                    NodeRow::Input { port } => {
                        let Some(p) = node.inputs.get(port) else {
                            continue;
                        };
                        let (px, py) = node.input_port_pos_graph(port);
                        let (psx, psy) = self.to_screen(viewport, px, py);
                        self.draw_port_dot(ui, psx, psy, port_d, p.color);
                        let name = elide_to_width(&p.name, port_label_size, port_label_budget);
                        ui.draw_text(
                            psx + PORT_COL_WIDTH * self.zoom,
                            psy - port_label_size * 0.5,
                            &name,
                            port_label_size,
                            TEXT_PRIMARY,
                        );
                    }
                    NodeRow::Param { param, input_port } => {
                        let Some(p) = node.params.get(param) else {
                            continue;
                        };
                        // Inline input socket on the left edge when this param is
                        // shadowed by a same-named scalar input (port-shadows-param).
                        if let Some(ii) = input_port
                            && let Some(port) = node.inputs.get(ii)
                        {
                            let (px, py) = node.input_port_pos_graph(ii);
                            let (psx, psy) = self.to_screen(viewport, px, py);
                            if p.wire_driven {
                                // D5: a tinted halo behind the socket dot flags
                                // it as the row's live "driven" jack, so the
                                // attribution is visible at the row itself, not
                                // just in the label text.
                                self.draw_port_dot(ui, psx, psy, port_d * 1.7, PARAM_DRIVEN_JACK);
                            }
                            self.draw_port_dot(ui, psx, psy, port_d, port.color);
                        }
                        // Expose checkbox (exposable kinds only): empty box = not
                        // on the card, filled cyan + tick = exposed. Click toggles.
                        // Wire-driven params can't be exposed (the wire owns them),
                        // so the box draws disabled to match the dead click.
                        // A group-face row (D6) never draws one at all — it's
                        // already the live mirror of an exposed card param, not
                        // an authoring picker of its own.
                        if !node.is_group && kind_is_exposable(p.kind) {
                            self.draw_expose_checkbox(
                                ui, sx, row_y, row_h, p.exposed, !p.wire_driven,
                            );
                        }
                        // Driver hint appended to the label: "← wired" when an
                        // input wire shadows the param (read-only), else
                        // "↳ <outer>" when an outer card slider routes in (still
                        // editable). Wire wins when both apply (parity with the
                        // sidebar's precedence).
                        // D6: wire beats binding for INTERACTIVITY (the row
                        // reads/draws as wire-driven either way), but a param
                        // that's also card-bound keeps that attribution
                        // visible too — hiding it would make the mapping
                        // undiscoverable the moment a wire lands on it.
                        let label_text: std::borrow::Cow<str> =
                            match (p.wire_driven, &p.outer_driver) {
                                (true, Some(outer)) => {
                                    format!("{}  ← wired (↳ {outer})", p.label).into()
                                }
                                (true, None) => format!("{}  ← wired", p.label).into(),
                                (false, Some(outer)) => format!("{}  ↳ {outer}", p.label).into(),
                                (false, None) => p.label.as_str().into(),
                            };
                        let slider_x = sx + PARAM_LABEL_X * self.zoom;
                        let row_right = sx + sw - pad_x;
                        if let Some(frac) = p.fill {
                            // Ranged numeric param — the same track/fill/thumb/
                            // value-cell widget the inspector card draws
                            // (`BitmapSlider::draw`, the immediate-mode twin of
                            // the card's tree-building `build`), reading the
                            // same `Theme`. A wire-driven row is read-only, so
                            // both its label and value dim as one unit instead
                            // of just the number.
                            let mut colors = Theme::INSPECTOR.slider_colors();
                            if p.wire_driven {
                                colors.text = color::TEXT_DIMMED_C32;
                                // D5: the whole slider reads as non-interactive,
                                // not just its label — track/fill/thumb all mix
                                // toward the same dimmed grey so a wire-driven
                                // row visually "looks disabled" before you ever
                                // try to drag it.
                                let dim = |c: Color32| color::mix(c, color::TEXT_DIMMED_C32, 0.55);
                                colors.track = dim(colors.track);
                                colors.track_hover = dim(colors.track_hover);
                                colors.track_pressed = dim(colors.track_pressed);
                                colors.fill = dim(colors.fill);
                                colors.thumb = dim(colors.thumb);
                            }
                            // The widget draws shorter than the full row pitch
                            // and centers in it, so the pitch's extra room
                            // (added for card-matching row spacing) reads as a
                            // real gap to the next row, not slack the slider
                            // eats.
                            let slider_h = PARAM_SLIDER_ROW_H * self.zoom;
                            let slider_rect = crate::node::Rect::new(
                                slider_x,
                                row_y + (row_h - slider_h) * 0.5,
                                (row_right - slider_x).max(0.0),
                                slider_h,
                            );
                            BitmapSlider::draw(
                                ui,
                                slider_rect,
                                Some(label_text.as_ref()),
                                frac,
                                &p.value,
                                &colors,
                                text_size,
                                PARAM_SLIDER_LABEL_W * self.zoom,
                                PARAM_SLIDER_VALUE_BOX_W * self.zoom,
                                self.zoom,
                            );
                        } else {
                            // Non-ranged param (enum / bool / colour / string /
                            // table): label + value text, no track — these
                            // kinds aren't scalars you'd drag, and their editors
                            // live in the floating popovers below.
                            let value_w = text_width(&p.value, text_size);
                            // A colour param gets a small swatch chip just left
                            // of its hex value, so the row reads as a colour at
                            // a glance; clicking the value opens the channel
                            // editor. `right_w` reserves the swatch so the label
                            // truncates clear of it.
                            let mut right_w = value_w;
                            if matches!(p.kind, crate::graph_view::ParamSnapshotKind::Color) {
                                let chip = text_size;
                                let gap = 4.0 * self.zoom;
                                let to_u8 = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
                                let c = p.vec_value;
                                let col = Color32::new(to_u8(c[0]), to_u8(c[1]), to_u8(c[2]), 255);
                                ui.draw_rounded_rect(
                                    row_right - value_w - gap - chip,
                                    text_y,
                                    chip,
                                    chip,
                                    col,
                                    2.0 * self.zoom,
                                );
                                right_w += gap + chip;
                            }
                            // A wire-driven param is read-only (a same-named
                            // input wire feeds it), so its value reads dimmed —
                            // the number is live but you can't scrub it here.
                            let value_color = if p.wire_driven {
                                TEXT_SECONDARY
                            } else {
                                TEXT_PRIMARY
                            };
                            ui.draw_text(
                                row_right - value_w,
                                text_y,
                                &p.value,
                                text_size,
                                value_color,
                            );
                            let label_budget =
                                (row_right - right_w - 6.0 * self.zoom - slider_x).max(0.0);
                            let label = elide_to_width(&label_text, text_size, label_budget);
                            ui.draw_text(slider_x, text_y, &label, text_size, TEXT_SECONDARY);
                        }
                    }
                    NodeRow::Action(kind) => {
                        // The "+ Object" / "+ Light" one-click gestures
                        // (D7/D7a) — same rounded-pill chrome + hover state as
                        // the "Edit Code…" footer below, so a gesture button
                        // reads as a button anywhere on the node face.
                        let label = match kind {
                            NodeActionKind::AddSceneObject => "+ Object",
                            NodeActionKind::AddSceneLight => "+ Light",
                        };
                        let inset = 4.0 * self.zoom;
                        let bx = sx + inset;
                        let bw = (sw - 2.0 * inset).max(0.0);
                        let by = row_y + inset * 0.25;
                        let bh = (row_h - inset * 0.5).max(0.0);
                        let (cx, cy) = self.cursor;
                        let hovered = cx >= bx && cx <= bx + bw && cy >= by && cy <= by + bh;
                        let bg = if hovered {
                            WGSL_FOOTER_HOVER_BG
                        } else {
                            WGSL_FOOTER_BG
                        };
                        ui.draw_rounded_rect(bx, by, bw, bh, bg, 3.0 * self.zoom);
                        let lw = text_width(label, text_size);
                        ui.draw_text(
                            bx + (bw - lw) * 0.5,
                            row_y + (row_h - text_size) * 0.5,
                            label,
                            text_size,
                            TEXT_PRIMARY,
                        );
                    }
                }
            }
            // "Edit Code…" footer for a `wgsl_compute` node — the click target
            // for `EditGraphNodeWgsl`. Geometry mirrors `wgsl_edit_rect`
            // (`body_top` + every row), so the drawn strip and the hit box align.
            if node.wgsl_source.is_some() {
                let fy = body_top + node.rows.len() as f32 * row_h;
                let fh = WGSL_FOOTER_H * self.zoom;
                let inset = 4.0 * self.zoom;
                let fx = sx + inset;
                let fw = (sw - 2.0 * inset).max(0.0);
                let (cx, cy) = self.cursor;
                let hovered = cx >= fx && cx <= fx + fw && cy >= fy && cy <= fy + fh;
                let bg = if hovered {
                    WGSL_FOOTER_HOVER_BG
                } else {
                    WGSL_FOOTER_BG
                };
                let btn_h = (fh - inset).max(0.0);
                ui.draw_rounded_rect(fx, fy + inset * 0.5, fw, btn_h, bg, 3.0 * self.zoom);
                let label = "Edit Code…";
                let ls = 9.0 * self.zoom;
                let lw = text_width(label, ls);
                ui.draw_text(
                    fx + (fw - lw) * 0.5,
                    fy + (fh - ls) * 0.5,
                    label,
                    ls,
                    TEXT_PRIMARY,
                );
            }
        }

        // Find-a-node: dim nodes that don't match the active search so the
        // matches stay bright and jump out of a busy graph. Drawn last, over the
        // node's own content.
        if !self.node_search.is_empty() && !self.node_matches_search(node) {
            ui.draw_rect(sx, sy, sw, sh, Color32::new(13, 13, 18, 168)); // 0.66 alpha dim
        }
    }

    /// Draw a port socket dot centred at `(cx, cy)` with diameter `d`.
    fn draw_port_dot(&self, ui: &mut dyn Painter, cx: f32, cy: f32, d: f32, color: Color32) {
        ui.draw_rounded_rect(cx - d * 0.5, cy - d * 0.5, d, d, color, d * 0.5);
    }

    /// Draw the open enum dropdown (Phase 2): a floating option list anchored
    /// under the param row it opened from. The selected option reads with an
    /// accent wash, the option under the cursor with a faint lift. Screen-space
    /// (the anchor was captured at open time), drawn at POPOVER depth over the
    /// nodes. No-op when no dropdown is open.
    fn render_enum_dropdown(&self, ui: &mut dyn Painter) {
        let Some(dd) = self.enum_dropdown.as_ref() else {
            return;
        };
        let panel = dd.panel_rect();
        // Backing + frame so the list reads as one floating menu.
        ui.draw_bordered_rect(
            panel.x, panel.y, panel.w, panel.h, TOOLTIP_BG, 3.0, 1.0, TOOLTIP_BORDER,
        );
        let text_size = color::FONT_BODY as f32 * self.zoom;
        let pad_x = 8.0 * self.zoom;
        let (cx, cy) = self.cursor;
        for (i, label) in dd.options.iter().enumerate() {
            let r = dd.option_rect(i);
            if i == dd.current {
                ui.draw_rect(r.x, r.y, r.w, r.h, ENUM_DD_CURRENT_BG);
            }
            if cx >= r.x && cx <= r.x + r.w && cy >= r.y && cy <= r.y + r.h {
                ui.draw_rect(r.x, r.y, r.w, r.h, ENUM_DD_HOVER_BG);
            }
            let text = elide_to_width(label, text_size, (r.w - 2.0 * pad_x).max(0.0));
            ui.draw_text(r.x + pad_x, r.y + 2.0 * self.zoom, &text, text_size, TEXT_PRIMARY);
        }
    }

    /// Draw the open Color / Vec channel editor (Phase 3): a floating panel under
    /// the param row, with a colour-swatch header for colours and one channel row
    /// per component — label (R/G/B/A or X/Y/Z/W), value, and a fill bar you drag
    /// to scrub. The channel values + swatch are read live from the node's
    /// `ParamView`, so an edit round-tripping through the snapshot keeps the panel
    /// current. Screen-space (anchor captured at open), POPOVER depth over the
    /// nodes. No-op when closed or the node/param has gone.
    fn render_vec_editor(&self, ui: &mut dyn Painter) {
        let Some(ed) = self.vec_editor.as_ref() else {
            return;
        };
        let Some(node) = self.find_node(ed.node_id) else {
            return;
        };
        let Some(p) = node.params.iter().find(|p| p.name == ed.param_name) else {
            return;
        };
        let vals = p.vec_value;
        let (min, max) = if ed.is_color {
            (0.0, 1.0)
        } else {
            p.range.unwrap_or((-1.0, 1.0))
        };
        let span = (max - min).max(f32::EPSILON);
        let panel = ed.panel_rect();
        ui.draw_bordered_rect(
            panel.x, panel.y, panel.w, panel.h, TOOLTIP_BG, 3.0, 1.0, TOOLTIP_BORDER,
        );
        let text_size = color::FONT_BODY as f32 * self.zoom;
        let pad_x = 8.0 * self.zoom;
        let (cx, cy) = self.cursor;
        let to_u8 = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;

        // Colour swatch header: the live colour + its hex, so the panel shows
        // what you're editing at a glance.
        if let Some(sw) = ed.swatch_rect() {
            let col = Color32::new(to_u8(vals[0]), to_u8(vals[1]), to_u8(vals[2]), 255);
            let inset = 3.0 * self.zoom;
            let chip = (sw.h - 2.0 * inset).max(0.0);
            ui.draw_rounded_rect(sw.x + pad_x, sw.y + inset, chip, chip, col, 2.0 * self.zoom);
            let hex = format_color_hex(vals);
            ui.draw_text(
                sw.x + pad_x + chip + 6.0 * self.zoom,
                sw.y + 2.0 * self.zoom,
                &hex,
                text_size,
                TEXT_PRIMARY,
            );
        }

        // One row per channel: label left, value right, fill bar under (the
        // inline "slider" you drag to scrub the channel).
        let labels = vec_channel_labels(ed.kind);
        for (ch, &cval) in vals.iter().enumerate().take(ed.components) {
            let r = ed.channel_rect(ch);
            if cx >= r.x && cx <= r.x + r.w && cy >= r.y && cy <= r.y + r.h {
                ui.draw_rect(r.x, r.y, r.w, r.h, ENUM_DD_HOVER_BG);
            }
            let text_y = r.y + 2.0 * self.zoom;
            let lab = labels.get(ch).copied().unwrap_or("");
            ui.draw_text(r.x + pad_x, text_y, lab, text_size, TEXT_SECONDARY);
            let val_str = format!("{cval:.3}");
            let vw = text_width(&val_str, text_size);
            ui.draw_text(
                r.x + r.w - pad_x - vw,
                text_y,
                &val_str,
                text_size,
                TEXT_PRIMARY,
            );
            let frac = ((cval - min) / span).clamp(0.0, 1.0);
            let bar_h = 2.0 * self.zoom;
            let bar_y = r.y + r.h - bar_h - 2.0 * self.zoom;
            let bar_x = r.x + pad_x;
            let bar_w = (r.x + r.w - pad_x - bar_x).max(0.0);
            ui.draw_rounded_rect(bar_x, bar_y, bar_w, bar_h, PARAM_FILL_BG, bar_h * 0.5);
            let fw = bar_w * frac;
            if fw > 0.0 {
                ui.draw_rounded_rect(bar_x, bar_y, fw, bar_h, PARAM_FILL_FG, bar_h * 0.5);
            }
        }
    }

    /// Draw the open `Table` grid editor (Phase 4): a floating panel under the
    /// param row — a header line (label + `rows×cols`), then the row-major grid
    /// of numeric cells. Cells read live from the node's `ParamView`, so a
    /// committed cell edit refreshes here. Clicking a cell opens the app's inline
    /// numeric editor over it (`EditGraphNodeTableCell`). Screen-space (anchor
    /// captured at open), POPOVER depth. No-op when closed or the node/param has
    /// gone.
    fn render_table_editor(&self, ui: &mut dyn Painter) {
        let Some(ed) = self.table_editor.as_ref() else {
            return;
        };
        let Some(rows) = self
            .find_node(ed.node_id)
            .and_then(|n| n.params.iter().find(|p| p.name == ed.param_name))
            .and_then(|p| p.table_value.clone())
        else {
            return;
        };
        let panel = ed.panel_rect();
        ui.draw_bordered_rect(
            panel.x, panel.y, panel.w, panel.h, TOOLTIP_BG, 3.0, 1.0, TOOLTIP_BORDER,
        );
        let text_size = color::FONT_BODY as f32 * self.zoom;
        let pad_x = 6.0 * self.zoom;
        let (cx, cy) = self.cursor;
        // Header line: "<label>  rows×cols", left-aligned like the sidebar grid.
        let header = format!("{} {}×{}", ed.param_name, ed.rows, ed.cols);
        ui.draw_text(
            ed.anchor.x + pad_x,
            ed.anchor.y + ed.anchor.h + 2.0 * self.zoom,
            &elide_to_width(&header, text_size, ed.anchor.w - 2.0 * pad_x),
            text_size,
            TEXT_SECONDARY,
        );
        // Grid: one centered numeric cell per (row, col), hover-washed.
        for r in 0..ed.rows {
            for c in 0..ed.cols {
                let cell = ed.cell_rect(r, c);
                if cx >= cell.x && cx <= cell.x + cell.w && cy >= cell.y && cy <= cell.y + cell.h {
                    ui.draw_rect(cell.x, cell.y, cell.w, cell.h, ENUM_DD_HOVER_BG);
                }
                let v = rows.get(r).and_then(|row| row.get(c)).copied().unwrap_or(0.0);
                let s = fmt_table_cell(v);
                let vw = text_width(&s, text_size);
                ui.draw_text(
                    cell.x + (cell.w - vw) * 0.5,
                    cell.y + 2.0 * self.zoom,
                    &s,
                    text_size,
                    TEXT_PRIMARY,
                );
            }
        }
    }

    /// Draw a param row's expose checkbox in the node's left column: an empty
    /// dark box when the param isn't exposed, a filled cyan box with a tick when
    /// it feeds the outer performance card. Geometry via `expose_glyph_bounds`,
    /// the same source `expose_glyph_under` hit-tests, so the drawn box and the
    /// click target can't drift. `enabled` is `false` for a wire-driven param —
    /// the box then draws dimmed so it reads as locked (the click is dead too).
    fn draw_expose_checkbox(
        &self,
        ui: &mut dyn Painter,
        node_x: f32,
        row_y: f32,
        row_h: f32,
        exposed: bool,
        enabled: bool,
    ) {
        let (bx, by, bd) = expose_glyph_bounds(node_x, row_y, row_h, self.zoom);
        let r = 2.0 * self.zoom;
        if !enabled {
            // Locked (wire-driven): a faint outline + inner fill, no interactive
            // tint, so it's visibly present but clearly not a live toggle.
            ui.draw_rounded_rect(bx, by, bd, bd, PARAM_EXPOSE_OFF, r);
            let inset = 1.5 * self.zoom;
            let iw = (bd - 2.0 * inset).max(0.0);
            ui.draw_rounded_rect(
                bx + inset,
                by + inset,
                iw,
                iw,
                PREVIEW_SCREEN_BG,
                (r - inset * 0.5).max(0.0),
            );
        } else if exposed {
            ui.draw_rounded_rect(bx, by, bd, bd, PARAM_EXPOSE_ON, r);
            ui.draw_text(bx + bd * 0.16, by - bd * 0.06, "✓", bd * 0.95, [20, 24, 33, 255]);
        } else {
            // Outer box tint, then a near-black inner fill so it reads as an
            // empty checkbox regardless of the node's hover state.
            ui.draw_rounded_rect(bx, by, bd, bd, PARAM_EXPOSE_OFF, r);
            let inset = 1.5 * self.zoom;
            let iw = (bd - 2.0 * inset).max(0.0);
            ui.draw_rounded_rect(
                bx + inset,
                by + inset,
                iw,
                iw,
                PREVIEW_SCREEN_BG,
                (r - inset * 0.5).max(0.0),
            );
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
            // D5: a click on a wire-driven param row highlights the wire that
            // feeds it, so it draws at the same full-focus brightness as a
            // hovered/selected endpoint.
            || self
                .highlighted_wire
                .as_ref()
                .is_some_and(|(tn, tp)| wire.to_node == *tn && &wire.to_port == tp)
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

    /// Draw every wire in `self.wires` matching `filter`, collapsing ≥2 wires
    /// that share a (from_node, to_node) pair into one ribbon (D8). `filter`
    /// picks the tier (control-fan vs. data) the same way the two unribboned
    /// `for wire in &self.wires` loops used to — this just adds the grouping
    /// pass in front of the per-wire draw.
    fn draw_wire_tier(&self, ui: &mut dyn Painter, viewport: Rect, filter: impl Fn(&WireView) -> bool) {
        let filtered: Vec<&WireView> = self.wires.iter().filter(|w| filter(w)).collect();
        for (_, members) in group_wires_by_pair(filtered.into_iter()) {
            if members.len() >= 2 {
                self.draw_wire_ribbon(ui, viewport, &members);
            } else {
                self.draw_wire(ui, viewport, members[0]);
            }
        }
    }

    /// D8: draw ≥2 same-(from_node, to_node) wires as ONE ribbon with an
    /// "×N" badge. Anchored at each node's vertical CENTRE rather than any
    /// one member's specific port row — decluttering the wall is the point,
    /// so the ribbon deliberately isn't tied to a single port's geometry.
    /// Same bezier-arc shape as `draw_wire`'s forward branch. A pair ending
    /// on a feedback (`breaks_dependency_cycle`) node falls back to drawing
    /// each member individually — the return-arc/dashed styling on that rare
    /// multi-wire loop matters more than decluttering it.
    fn draw_wire_ribbon(&self, ui: &mut dyn Painter, viewport: Rect, members: &[&WireView]) {
        let first = members[0];
        let (Some(from), Some(to)) =
            (self.find_node(first.from_node), self.find_node(first.to_node))
        else {
            return;
        };
        if to.breaks_dependency_cycle {
            for w in members {
                self.draw_wire(ui, viewport, w);
            }
            return;
        }

        let (gx0, gy0) = (from.pos_graph.0 + NODE_WIDTH, from.pos_graph.1 + from.height() * 0.5);
        let (gx1, gy1) = (to.pos_graph.0, to.pos_graph.1 + to.height() * 0.5);
        let (sx0, sy0) = self.to_screen(viewport, gx0, gy0);
        let (sx1, sy1) = self.to_screen(viewport, gx1, gy1);
        let span_x = (sx1 - sx0).abs();
        let span_y = (sy1 - sy0).abs();
        let dx = (span_x.max(40.0) * 0.5 + span_y * 0.35).min(span_x.max(160.0));
        let (cx0, cy0, cx1, cy1) = (sx0 + dx, sy0, sx1 - dx, sy1);

        let approx_len =
            (span_x + span_y + (cx0 - sx0).abs() + (sx1 - cx1).abs()).max(40.0);
        let steps = (approx_len / 12.0).clamp(16.0, 80.0) as i32;
        // Bundle colour: the first member's port colour — a ribbon reads as
        // "more than one," not as a new colour identity, so no new palette
        // entry (`feedback_prefer_high_saturation_identity_colors` is about
        // PORT identity; this is still N ordinary wires, just drawn once).
        let port_color = from
            .outputs
            .iter()
            .find(|p| p.name == first.from_port)
            .map(|p| p.color)
            .unwrap_or(color::TEXT_DIMMED_C32);
        let thickness = (2.2 * self.zoom).clamp(1.6, 3.2);
        let wire_color = port_color.with_alpha(200);

        let mut prev = cubic_bezier(0.0, sx0, sy0, cx0, cy0, cx1, cy1, sx1, sy1);
        let mut mid = prev;
        let mid_step = steps / 2;
        for i in 1..=steps {
            let t = i as f32 / steps as f32;
            let curr = cubic_bezier(t, sx0, sy0, cx0, cy0, cx1, cy1, sx1, sy1);
            ui.draw_line(prev.0, prev.1, curr.0, curr.1, thickness, wire_color);
            if i == mid_step {
                mid = curr;
            }
            prev = curr;
        }

        // "×N" badge at the curve's midpoint — same chip chrome as the
        // feedback return tag, so it reads as "meta information about a
        // wire," not a new UI element family.
        let label = format!("×{}", members.len());
        let font = 9.0 * self.zoom;
        let pad_x = 4.0 * self.zoom;
        let pad_y = 2.0 * self.zoom;
        let text_w = text_width(&label, font);
        let chip_w = text_w + pad_x * 2.0;
        let chip_h = font + pad_y * 2.0;
        ui.draw_rounded_rect(
            mid.0 - chip_w * 0.5,
            mid.1 - chip_h * 0.5,
            chip_w,
            chip_h,
            RETURN_TAG_BG,
            chip_h * 0.3,
        );
        ui.draw_text(
            mid.0 - chip_w * 0.5 + pad_x,
            mid.1 - chip_h * 0.5 + pad_y,
            &label,
            font,
            RETURN_TAG_TEXT,
        );
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
        // (the "Previous X" recurrent-state taps). Layout excludes it
        // (auto_layout skips these), so the source can sit far to the right of
        // a leftmost Feedback node — a literal wire spanning that whole gap
        // either runs off-screen or reads as spaghetti on a wide graph.
        let is_return = to.breaks_dependency_cycle;

        // Quiet by default: a small tag at each end naming the other side,
        // no long-haul arc. Hovering or selecting either endpoint node (the
        // existing `focused` check) reveals the full routed arc below, for
        // when you actually want to trace it.
        if is_return && !focused {
            self.draw_return_tag(ui, sx0, sy0, &format!("↪ {}", to.title), true);
            self.draw_return_tag(ui, sx1, sy1, &format!("↩ {}", from.title), false);
            return;
        }

        // ── Colour + alpha ──
        // Forward wires take the from-port's kind colour (matching the port
        // circles); control/scalar wires (orange) fade to a faint baseline
        // unless focused; data wires stay readable. Return paths get one
        // violet family regardless of port kind, dimmer than data but above
        // the control fan. Any focused wire lights to full.
        let is_control = from.outputs[from_idx].is_control;
        let port_color = from.outputs[from_idx].color;
        // Base RGB from the port kind (or the one return-wire violet), with a
        // runtime alpha for focus/control dimming. Alpha bytes: 0.95≈242,
        // 0.34≈87, 0.16≈41, 0.7≈179.
        let (base, alpha) = if is_return {
            (RETURN_WIRE_COLOR, if focused { 242 } else { 87 })
        } else if focused {
            (port_color, 242)
        } else if is_control {
            (port_color, 41)
        } else {
            (port_color, 179)
        };
        let wire_color = base.with_alpha(alpha);

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

    /// One endpoint tag for a quiet (unfocused) feedback return — a small
    /// violet chip naming the other side, sitting just outside the node's
    /// edge so it reads without a wire drawn under it. `at_source` picks which
    /// way it hangs: right of the source's output dot, or left of the
    /// feedback node's input dot, so the two tags always point away from
    /// their own node and toward where the loop actually goes.
    fn draw_return_tag(&self, ui: &mut dyn Painter, px: f32, py: f32, label: &str, at_source: bool) {
        let font = 9.0 * self.zoom;
        let pad_x = 5.0 * self.zoom;
        let pad_y = 2.0 * self.zoom;
        let gap = 6.0 * self.zoom;
        let text_w = text_width(label, font);
        let chip_w = text_w + pad_x * 2.0;
        let chip_h = font + pad_y * 2.0;
        let chip_x = if at_source { px + gap } else { px - gap - chip_w };
        let chip_y = py - chip_h * 0.5;
        ui.draw_rounded_rect(chip_x, chip_y, chip_w, chip_h, RETURN_TAG_BG, chip_h * 0.3);
        ui.draw_text(chip_x + pad_x, chip_y + pad_y, label, font, RETURN_TAG_TEXT);
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
