//! `GraphCanvas` — the read-only node-graph viewer hosted by the
//! editor window.
//!
//! Phase 4 scope: render a hardcoded view of the `NodeGraphTestFX`
//! graph (Source A + Source B → Mix → FinalOutput), with pan
//! (middle-mouse drag), zoom (scroll wheel), and hover highlight.
//! No editing, no live data sync to the running graph yet — those
//! land in subsequent phases.
//!
//! Rendering goes straight through `UIRenderer` (rect + text), no
//! `UITree` / `Panel` infrastructure, so the editor stays cheap to
//! reason about while there's only one panel in the window.

use manifold_renderer::ui_renderer::UIRenderer;

const HEADER_HEIGHT: f32 = 28.0;
const NODE_WIDTH: f32 = 140.0;
const NODE_HEADER_HEIGHT: f32 = 22.0;
const PORT_ROW_HEIGHT: f32 = 18.0;
const PORT_RADIUS: f32 = 4.0;
const PORT_COL_WIDTH: f32 = 10.0;
const NODE_CORNER: f32 = 6.0;

const BG_COLOR: [f32; 4] = [0.10, 0.10, 0.12, 1.0];
const HEADER_BG: [f32; 4] = [0.14, 0.14, 0.17, 1.0];
const GRID_DOT: [f32; 4] = [1.0, 1.0, 1.0, 0.06];
const NODE_BG: [f32; 4] = [0.18, 0.18, 0.22, 1.0];
const NODE_BG_HOVER: [f32; 4] = [0.22, 0.22, 0.27, 1.0];
const NODE_HEADER_BG: [f32; 4] = [0.28, 0.30, 0.42, 1.0];
const NODE_BORDER: [f32; 4] = [0.0, 0.0, 0.0, 0.6];
const PORT_TEXTURE_COLOR: [f32; 4] = [0.50, 0.78, 1.00, 1.0];
const WIRE_COLOR: [f32; 4] = [0.50, 0.78, 1.00, 0.85];
const TEXT_PRIMARY: [u8; 4] = [220, 220, 230, 255];
const TEXT_SECONDARY: [u8; 4] = [150, 150, 165, 255];
const TEXT_HEADER: [u8; 4] = [240, 240, 250, 255];

/// One port on a node in the canvas view.
#[derive(Debug, Clone, Copy)]
struct PortView {
    name: &'static str,
    /// Reserved for future per-type port colouring (Float/Bool ports
    /// will pick a different colour). All Phase 4 test-graph ports are
    /// `Texture2D`, so it's effectively unused today — but keeping it
    /// avoids a follow-up refactor.
    _kind: PortKind,
}

#[derive(Debug, Clone, Copy)]
enum PortKind {
    Texture2D,
}

/// One node in the canvas view.
#[derive(Debug, Clone)]
struct NodeView {
    /// Stable identifier within this view. Matches `NodeInstanceId.0`
    /// for the hardcoded test graph; eventually sourced from the live
    /// graph snapshot.
    id: u32,
    title: &'static str,
    /// Position of the node's top-left corner in graph-space (logical
    /// pixels, before pan/zoom).
    pos_graph: (f32, f32),
    inputs: &'static [PortView],
    outputs: &'static [PortView],
}

impl NodeView {
    fn height(&self) -> f32 {
        let port_rows = self.inputs.len().max(self.outputs.len()) as f32;
        NODE_HEADER_HEIGHT + port_rows * PORT_ROW_HEIGHT + 6.0
    }

    fn input_port_pos_graph(&self, idx: usize) -> (f32, f32) {
        let (x, y) = self.pos_graph;
        (
            x,
            y + NODE_HEADER_HEIGHT + idx as f32 * PORT_ROW_HEIGHT + PORT_ROW_HEIGHT * 0.5,
        )
    }

    fn output_port_pos_graph(&self, idx: usize) -> (f32, f32) {
        let (x, y) = self.pos_graph;
        (
            x + NODE_WIDTH,
            y + NODE_HEADER_HEIGHT + idx as f32 * PORT_ROW_HEIGHT + PORT_ROW_HEIGHT * 0.5,
        )
    }
}

/// One wire between two nodes.
#[derive(Debug, Clone, Copy)]
struct WireView {
    from_node: u32,
    from_port: &'static str,
    to_node: u32,
    to_port: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DragMode {
    None,
    Pan,
}

/// The read-only graph canvas hosted in the editor window.
pub struct GraphCanvas {
    nodes: Vec<NodeView>,
    wires: Vec<WireView>,
    /// Pan offset in logical pixels (applied to graph-space before zoom).
    pan: (f32, f32),
    /// Zoom factor — graph-space-to-screen-space multiplier.
    zoom: f32,
    /// Last known cursor position in window-logical pixels. Updated on
    /// every `CursorMoved` so click/drag/scroll can transform without
    /// the caller passing it again.
    cursor: (f32, f32),
    drag_mode: DragMode,
    drag_anchor: (f32, f32),
    drag_pan_start: (f32, f32),
    /// Hovered node id, if any. Populated on every cursor move.
    hovered: Option<u32>,
}

impl GraphCanvas {
    /// Build a canvas seeded with the hardcoded `NodeGraphTestFX`
    /// graph: Source A + Source B → Mix → FinalOutput.
    pub fn new() -> Self {
        const SOURCE_OUTS: &[PortView] = &[PortView {
            name: "out",
            _kind: PortKind::Texture2D,
        }];
        const MIX_INS: &[PortView] = &[
            PortView {
                name: "a",
                _kind: PortKind::Texture2D,
            },
            PortView {
                name: "b",
                _kind: PortKind::Texture2D,
            },
        ];
        const MIX_OUTS: &[PortView] = &[PortView {
            name: "out",
            _kind: PortKind::Texture2D,
        }];
        const FINAL_INS: &[PortView] = &[PortView {
            name: "in",
            _kind: PortKind::Texture2D,
        }];

        let nodes = vec![
            NodeView {
                id: 0,
                title: "Source A",
                pos_graph: (60.0, 80.0),
                inputs: &[],
                outputs: SOURCE_OUTS,
            },
            NodeView {
                id: 1,
                title: "Source B",
                pos_graph: (60.0, 240.0),
                inputs: &[],
                outputs: SOURCE_OUTS,
            },
            NodeView {
                id: 2,
                title: "Mix",
                pos_graph: (300.0, 160.0),
                inputs: MIX_INS,
                outputs: MIX_OUTS,
            },
            NodeView {
                id: 3,
                title: "FinalOutput",
                pos_graph: (540.0, 160.0),
                inputs: FINAL_INS,
                outputs: &[],
            },
        ];

        let wires = vec![
            WireView {
                from_node: 0,
                from_port: "out",
                to_node: 2,
                to_port: "a",
            },
            WireView {
                from_node: 1,
                from_port: "out",
                to_node: 2,
                to_port: "b",
            },
            WireView {
                from_node: 2,
                from_port: "out",
                to_node: 3,
                to_port: "in",
            },
        ];

        Self {
            nodes,
            wires,
            pan: (40.0, 40.0),
            zoom: 1.0,
            cursor: (0.0, 0.0),
            drag_mode: DragMode::None,
            drag_anchor: (0.0, 0.0),
            drag_pan_start: (0.0, 0.0),
            hovered: None,
        }
    }

    // ── Coordinate transforms ───────────────────────────────────────

    /// Graph-space → window-space (pixel).
    fn to_screen(&self, viewport: Rect, gx: f32, gy: f32) -> (f32, f32) {
        let canvas_x = viewport.x;
        let canvas_y = viewport.y + HEADER_HEIGHT;
        (
            canvas_x + (gx + self.pan.0) * self.zoom,
            canvas_y + (gy + self.pan.1) * self.zoom,
        )
    }

    /// Window-space (pixel) → graph-space.
    fn to_graph(&self, viewport: Rect, sx: f32, sy: f32) -> (f32, f32) {
        let canvas_x = viewport.x;
        let canvas_y = viewport.y + HEADER_HEIGHT;
        (
            (sx - canvas_x) / self.zoom - self.pan.0,
            (sy - canvas_y) / self.zoom - self.pan.1,
        )
    }

    // ── Hit testing ─────────────────────────────────────────────────

    fn node_under(&self, viewport: Rect, sx: f32, sy: f32) -> Option<u32> {
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

    // ── Input handlers ──────────────────────────────────────────────

    pub fn on_pointer_move(&mut self, viewport: Rect, sx: f32, sy: f32) {
        self.cursor = (sx, sy);
        if self.drag_mode == DragMode::Pan {
            let dx = (sx - self.drag_anchor.0) / self.zoom;
            let dy = (sy - self.drag_anchor.1) / self.zoom;
            self.pan = (self.drag_pan_start.0 + dx, self.drag_pan_start.1 + dy);
        } else {
            self.hovered = self.node_under(viewport, sx, sy);
        }
    }

    pub fn on_pan_button_down(&mut self, sx: f32, sy: f32) {
        self.drag_mode = DragMode::Pan;
        self.drag_anchor = (sx, sy);
        self.drag_pan_start = self.pan;
    }

    /// Last known cursor position (window-logical pixels). Set by the
    /// most recent `on_pointer_move`.
    pub fn cursor(&self) -> (f32, f32) {
        self.cursor
    }

    pub fn on_pan_button_up(&mut self) {
        self.drag_mode = DragMode::None;
    }

    /// Apply scroll-wheel zoom. `dy` is in logical pixels; positive =
    /// zoom in. Anchored at the current cursor so the point under the
    /// cursor stays put across the zoom.
    pub fn on_scroll(&mut self, viewport: Rect, dy: f32) {
        let (gx_before, gy_before) = self.to_graph(viewport, self.cursor.0, self.cursor.1);
        let factor = (dy * 0.0015).exp();
        let new_zoom = (self.zoom * factor).clamp(0.25, 4.0);
        self.zoom = new_zoom;
        let (gx_after, gy_after) = self.to_graph(viewport, self.cursor.0, self.cursor.1);
        // Re-anchor: shift pan so cursor stays over the same graph point.
        self.pan.0 += gx_after - gx_before;
        self.pan.1 += gy_after - gy_before;
    }

    // ── Render ──────────────────────────────────────────────────────

    /// Render the canvas into the editor's offscreen via `UIRenderer`.
    /// `viewport` is the editor window's logical rect (full window).
    pub fn render(&self, ui: &mut UIRenderer, viewport: Rect) {
        // Solid background covering the whole window.
        ui.draw_rect(viewport.x, viewport.y, viewport.w, viewport.h, BG_COLOR);

        // ── Header bar ──
        ui.draw_rect(viewport.x, viewport.y, viewport.w, HEADER_HEIGHT, HEADER_BG);
        ui.draw_text(
            viewport.x + 10.0,
            viewport.y + (HEADER_HEIGHT - 12.0) * 0.5,
            "Node Graph Test  (read-only)",
            12.0,
            TEXT_HEADER,
        );
        let zoom_text = format!("Zoom {:.0}%", self.zoom * 100.0);
        ui.draw_text(
            viewport.x + viewport.w - 90.0,
            viewport.y + (HEADER_HEIGHT - 11.0) * 0.5,
            &zoom_text,
            11.0,
            TEXT_SECONDARY,
        );

        let canvas = Rect {
            x: viewport.x,
            y: viewport.y + HEADER_HEIGHT,
            w: viewport.w,
            h: (viewport.h - HEADER_HEIGHT).max(0.0),
        };
        if canvas.w <= 0.0 || canvas.h <= 0.0 {
            return;
        }

        // ── Background dot grid ──
        self.draw_grid(ui, canvas);

        // ── Wires (drawn under nodes) ──
        for wire in &self.wires {
            self.draw_wire(ui, viewport, *wire);
        }

        // ── Nodes ──
        for node in &self.nodes {
            self.draw_node(ui, viewport, canvas, node);
        }
    }

    fn draw_grid(&self, ui: &mut UIRenderer, canvas: Rect) {
        const GRAPH_SPACING: f32 = 32.0;
        let spacing = GRAPH_SPACING * self.zoom;
        if spacing < 8.0 {
            return; // too dense at low zoom
        }
        // Find first dot inside the canvas in graph-space.
        let (g_min_x, g_min_y) = self.to_graph(canvas_to_viewport(canvas), canvas.x, canvas.y);
        let start_gx = (g_min_x / GRAPH_SPACING).floor() * GRAPH_SPACING;
        let start_gy = (g_min_y / GRAPH_SPACING).floor() * GRAPH_SPACING;
        let mut gy = start_gy;
        while {
            let (_, sy) = self.to_screen(canvas_to_viewport(canvas), 0.0, gy);
            sy < canvas.y + canvas.h
        } {
            let mut gx = start_gx;
            while {
                let (sx, _) = self.to_screen(canvas_to_viewport(canvas), gx, 0.0);
                sx < canvas.x + canvas.w
            } {
                let (sx, sy) = self.to_screen(canvas_to_viewport(canvas), gx, gy);
                if sx >= canvas.x && sy >= canvas.y {
                    ui.draw_rect(sx - 1.0, sy - 1.0, 2.0, 2.0, GRID_DOT);
                }
                gx += GRAPH_SPACING;
            }
            gy += GRAPH_SPACING;
        }
    }

    fn draw_node(&self, ui: &mut UIRenderer, viewport: Rect, canvas: Rect, node: &NodeView) {
        let (sx, sy) = self.to_screen(viewport, node.pos_graph.0, node.pos_graph.1);
        let sw = NODE_WIDTH * self.zoom;
        let sh = node.height() * self.zoom;
        // Software-clip: skip nodes fully outside the canvas.
        if sx + sw < canvas.x || sx > canvas.x + canvas.w {
            return;
        }
        if sy + sh < canvas.y || sy > canvas.y + canvas.h {
            return;
        }

        let hovered = self.hovered == Some(node.id);
        let bg = if hovered { NODE_BG_HOVER } else { NODE_BG };

        // Body
        ui.draw_bordered_rect(
            sx,
            sy,
            sw,
            sh,
            bg,
            NODE_CORNER * self.zoom,
            1.0,
            NODE_BORDER,
        );

        // Header strip
        let header_h = NODE_HEADER_HEIGHT * self.zoom;
        ui.draw_rounded_rect(sx, sy, sw, header_h, NODE_HEADER_BG, NODE_CORNER * self.zoom);

        // Title
        let title_size = (11.0 * self.zoom).max(8.0);
        ui.draw_text(
            sx + 8.0 * self.zoom,
            sy + (header_h - title_size) * 0.5,
            node.title,
            title_size,
            TEXT_HEADER,
        );

        // Ports
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
                PORT_TEXTURE_COLOR,
                PORT_RADIUS * self.zoom,
            );
            ui.draw_text(
                psx + PORT_COL_WIDTH * self.zoom,
                psy - port_label_size * 0.5,
                port.name,
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
                PORT_TEXTURE_COLOR,
                PORT_RADIUS * self.zoom,
            );
            // Right-aligned label inside the node body.
            let approx_w = port.name.len() as f32 * port_label_size * 0.55;
            ui.draw_text(
                psx - PORT_COL_WIDTH * self.zoom - approx_w,
                psy - port_label_size * 0.5,
                port.name,
                port_label_size,
                TEXT_PRIMARY,
            );
        }
    }

    fn draw_wire(&self, ui: &mut UIRenderer, viewport: Rect, wire: WireView) {
        let from = self.find_node(wire.from_node);
        let to = self.find_node(wire.to_node);
        let (Some(from), Some(to)) = (from, to) else {
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

        // Cubic bezier control points: pull horizontally so the curve
        // exits/enters the ports horizontally — TouchDesigner-style.
        let dx = (sx1 - sx0).abs().max(40.0) * 0.5;
        let cx0 = sx0 + dx;
        let cy0 = sy0;
        let cx1 = sx1 - dx;
        let cy1 = sy1;

        // Sample the cubic bezier as small dots. Step count scales with
        // pixel length so close-up curves stay smooth.
        let approx_len =
            ((sx1 - sx0).abs() + (sy1 - sy0).abs() + 2.0 * dx).max(40.0);
        let steps = (approx_len / 4.0).clamp(20.0, 160.0) as i32;
        let dot = (2.5 * self.zoom).clamp(1.5, 3.0);
        for i in 0..=steps {
            let t = i as f32 / steps as f32;
            let (x, y) = cubic_bezier(t, sx0, sy0, cx0, cy0, cx1, cy1, sx1, sy1);
            ui.draw_rect(x - dot * 0.5, y - dot * 0.5, dot, dot, WIRE_COLOR);
        }
    }

    fn find_node(&self, id: u32) -> Option<&NodeView> {
        self.nodes.iter().find(|n| n.id == id)
    }
}

impl Default for GraphCanvas {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }
}

fn canvas_to_viewport(canvas: Rect) -> Rect {
    // The grid + transforms expect a viewport rect (header included),
    // but we pass in the canvas rect (header excluded). Reconstruct
    // by subtracting HEADER_HEIGHT from the y.
    Rect {
        x: canvas.x,
        y: canvas.y - HEADER_HEIGHT,
        w: canvas.w,
        h: canvas.h + HEADER_HEIGHT,
    }
}

#[allow(clippy::too_many_arguments)]
fn cubic_bezier(
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
