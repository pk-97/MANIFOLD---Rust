//! `GraphCanvas` — editable node-graph view hosted by the editor
//! window.
//!
//! The canvas is data-driven from `GraphSnapshot`s pushed by the
//! content thread (one per frame while the editor is open). When a new
//! topology lands, nodes are auto-laid-out by topological depth: source
//! nodes (no inputs) on the left, each downstream node placed to the
//! right of its deepest predecessor. Node positions persist across
//! parameter-only updates, so the layout doesn't twitch when only
//! `Mix.amount` changes.
//!
//! Future-proofing: when V2's editor lets users move nodes, snapshot
//! `NodeSnapshot.editor_pos` will switch from `None` to `Some`. The
//! canvas already prefers stored positions over auto-layout when present.
//!
//! Rendering goes through `UIRenderer` rect+text primitives — no UITree
//! / Panel infrastructure. Pan via middle-mouse drag, zoom via scroll
//! wheel, hover highlights. No editing yet.

use manifold_renderer::node_graph::{GraphSnapshot, PortKindSnapshot};
use manifold_renderer::ui_renderer::UIRenderer;
use manifold_ui::PanelAction;

const HEADER_HEIGHT: f32 = 28.0;
const NODE_WIDTH: f32 = 140.0;
const NODE_HEADER_HEIGHT: f32 = 22.0;
const SUMMARY_ROW_HEIGHT: f32 = 16.0;
const PORT_ROW_HEIGHT: f32 = 18.0;
const PORT_RADIUS: f32 = 4.0;
const PORT_COL_WIDTH: f32 = 10.0;
const NODE_CORNER: f32 = 6.0;

// Auto-layout grid spacing.
const COL_SPACING: f32 = 220.0;
const ROW_SPACING: f32 = 130.0;
const LAYOUT_ORIGIN: (f32, f32) = (60.0, 60.0);

const BG_COLOR: [f32; 4] = [0.10, 0.10, 0.12, 1.0];
const HEADER_BG: [f32; 4] = [0.14, 0.14, 0.17, 1.0];
const GRID_DOT: [f32; 4] = [1.0, 1.0, 1.0, 0.06];
const NODE_BG: [f32; 4] = [0.18, 0.18, 0.22, 1.0];
const NODE_BG_HOVER: [f32; 4] = [0.22, 0.22, 0.27, 1.0];
const NODE_HEADER_BG: [f32; 4] = [0.28, 0.30, 0.42, 1.0];
const NODE_BORDER: [f32; 4] = [0.0, 0.0, 0.0, 0.6];
const NODE_BORDER_SELECTED: [f32; 4] = [0.50, 0.78, 1.00, 1.0];
const PORT_TEXTURE2D_COLOR: [f32; 4] = [0.50, 0.78, 1.00, 1.0];
const PORT_TEXTURE3D_COLOR: [f32; 4] = [0.78, 0.50, 1.00, 1.0];
const PORT_SCALAR_COLOR: [f32; 4] = [1.00, 0.78, 0.40, 1.0];
const WIRE_COLOR: [f32; 4] = [0.50, 0.78, 1.00, 0.85];
const TEXT_PRIMARY: [u8; 4] = [220, 220, 230, 255];
const TEXT_SECONDARY: [u8; 4] = [150, 150, 165, 255];
const TEXT_HEADER: [u8; 4] = [240, 240, 250, 255];

#[derive(Debug, Clone)]
struct PortView {
    name: String,
    color: [f32; 4],
}

impl PortView {
    fn from_kind(name: String, kind: PortKindSnapshot) -> Self {
        let color = match kind {
            PortKindSnapshot::Texture2D => PORT_TEXTURE2D_COLOR,
            PortKindSnapshot::Texture3D => PORT_TEXTURE3D_COLOR,
            PortKindSnapshot::Scalar => PORT_SCALAR_COLOR,
        };
        Self { name, color }
    }
}

#[derive(Debug, Clone)]
struct NodeView {
    id: u32,
    title: String,
    /// Compact one-line summary of the node's most informative
    /// parameter — e.g. "Mode: FoldX" for Mirror's Transform. Lets the
    /// user read what a node is *doing* from the canvas without
    /// opening the inspector. `None` if the node has no parameters.
    summary: Option<String>,
    /// Top-left corner in graph-space (logical pixels, pre pan/zoom).
    pos_graph: (f32, f32),
    inputs: Vec<PortView>,
    outputs: Vec<PortView>,
}

impl NodeView {
    fn height(&self) -> f32 {
        let port_rows = self.inputs.len().max(self.outputs.len()) as f32;
        let summary_h = if self.summary.is_some() {
            SUMMARY_ROW_HEIGHT
        } else {
            0.0
        };
        NODE_HEADER_HEIGHT + summary_h + port_rows * PORT_ROW_HEIGHT + 6.0
    }

    /// Y offset where port rows start, accounting for an optional
    /// inline summary line below the header.
    fn ports_y_offset(&self) -> f32 {
        let summary_h = if self.summary.is_some() {
            SUMMARY_ROW_HEIGHT
        } else {
            0.0
        };
        NODE_HEADER_HEIGHT + summary_h
    }

    fn input_port_pos_graph(&self, idx: usize) -> (f32, f32) {
        let (x, y) = self.pos_graph;
        (
            x,
            y + self.ports_y_offset() + idx as f32 * PORT_ROW_HEIGHT + PORT_ROW_HEIGHT * 0.5,
        )
    }

    fn output_port_pos_graph(&self, idx: usize) -> (f32, f32) {
        let (x, y) = self.pos_graph;
        (
            x + NODE_WIDTH,
            y + self.ports_y_offset() + idx as f32 * PORT_ROW_HEIGHT + PORT_ROW_HEIGHT * 0.5,
        )
    }
}

/// Pick the most informative parameter to surface in the inline node
/// summary. Heuristic: prefer enum params (whose label is descriptive,
/// e.g. "FoldX"), then floats, then anything else. Returns `None` if
/// the node has no parameters.
fn build_summary(parameters: &[manifold_renderer::node_graph::ParamSnapshot]) -> Option<String> {
    use manifold_renderer::node_graph::ParamSnapshotKind;
    let pick = parameters
        .iter()
        .find(|p| p.kind == ParamSnapshotKind::Enum)
        .or_else(|| {
            parameters
                .iter()
                .find(|p| matches!(p.kind, ParamSnapshotKind::Float | ParamSnapshotKind::Int))
        })
        .or_else(|| parameters.first())?;

    let value_str = match pick.kind {
        ParamSnapshotKind::Enum => pick
            .enum_labels
            .as_ref()
            .and_then(|labels| labels.get(pick.current_value as usize).cloned())
            .unwrap_or_else(|| format!("{}", pick.current_value as i64)),
        ParamSnapshotKind::Bool => {
            if pick.current_value >= 0.5 {
                "true".to_string()
            } else {
                "false".to_string()
            }
        }
        ParamSnapshotKind::Int => format!("{}", pick.current_value as i64),
        ParamSnapshotKind::Float => format!("{:.2}", pick.current_value),
        ParamSnapshotKind::Other => "—".to_string(),
    };
    Some(format!("{}: {}", pick.label, value_str))
}

#[derive(Debug, Clone)]
struct WireView {
    from_node: u32,
    from_port: String,
    to_node: u32,
    to_port: String,
}

#[derive(Debug, Clone)]
enum DragMode {
    None,
    Pan,
    /// Dragging from an output port to draw a wire. On release over an
    /// input port, emits `PanelAction::ConnectPorts`.
    WireFrom {
        from_node: u32,
        from_port: String,
    },
    /// Dragging a node by its header. `anchor_offset` is the graph-space
    /// (cursor - node_origin) at button-down so the node doesn't snap
    /// to the cursor on pickup. `start_pos` is the node's pre-drag
    /// position, retained so the `MoveGraphNode` action emitted on
    /// release reflects only the net delta and the undo command has a
    /// clean previous-pos to restore.
    NodeMove {
        node_id: u32,
        anchor_offset: (f32, f32),
        #[allow(dead_code)]
        start_pos: (f32, f32),
    },
}

impl DragMode {
    fn is_pan(&self) -> bool {
        matches!(self, DragMode::Pan)
    }
}

/// A port resolved from a screen-space cursor position. Used by the
/// wire-drag hit test.
#[derive(Debug, Clone)]
struct PortHit {
    node_id: u32,
    port_name: String,
    is_output: bool,
}

pub struct GraphCanvas {
    nodes: Vec<NodeView>,
    wires: Vec<WireView>,
    /// Hash of the current topology (node ids+types + wire endpoints).
    /// Compared on each `set_snapshot` to skip layout recomputation
    /// when only parameter values changed.
    topology_hash: u64,
    pan: (f32, f32),
    zoom: f32,
    cursor: (f32, f32),
    drag_mode: DragMode,
    drag_anchor: (f32, f32),
    drag_pan_start: (f32, f32),
    hovered: Option<u32>,
    selected: Option<u32>,
    /// Actions accumulated this frame from canvas interactions.
    /// Drained by the editor window's input loop after each event.
    pending_actions: Vec<PanelAction>,
}

impl GraphCanvas {
    pub fn new() -> Self {
        Self {
            nodes: Vec::new(),
            wires: Vec::new(),
            topology_hash: 0,
            pan: (0.0, 0.0),
            zoom: 1.0,
            cursor: (0.0, 0.0),
            drag_mode: DragMode::None,
            drag_anchor: (0.0, 0.0),
            drag_pan_start: (0.0, 0.0),
            hovered: None,
            selected: None,
            pending_actions: Vec::new(),
        }
    }

    /// Drain editor actions queued by canvas interactions. Called
    /// once per input event by the editor window's present path.
    pub fn drain_actions(&mut self) -> Vec<PanelAction> {
        std::mem::take(&mut self.pending_actions)
    }

    /// Emit a `RemoveGraphNode` action for the currently-selected
    /// node, if any. Wired to the Delete/Backspace key handler on the
    /// editor window. Clears the selection on emit so the next frame
    /// doesn't double-fire.
    pub fn request_delete_selected(&mut self) {
        if let Some(id) = self.selected.take() {
            self.pending_actions
                .push(PanelAction::RemoveGraphNode { node_id: id });
        }
    }

    /// Push the latest snapshot. Rebuilds nodes+wires; recomputes
    /// auto-layout only when topology changed.
    pub fn set_snapshot(&mut self, snap: &GraphSnapshot) {
        let new_hash = hash_topology(snap);
        if new_hash == self.topology_hash && !self.nodes.is_empty() {
            // Topology unchanged — keep existing layout. Nothing else
            // in the snapshot affects rendering today (params would, but
            // we don't display values yet).
            return;
        }
        self.topology_hash = new_hash;

        // Preserve positions for nodes that already existed before the
        // topology change. Without this, every wire connection would
        // re-run depth-based auto-layout against the new topology,
        // shifting unrelated nodes into different columns — looked
        // like the graph "snapping to weird positions" each time.
        let prev_positions: ahash::AHashMap<u32, (f32, f32)> = self
            .nodes
            .iter()
            .map(|n| (n.id, n.pos_graph))
            .collect();

        self.nodes = snap
            .nodes
            .iter()
            .map(|n| NodeView {
                id: n.id,
                title: n.title.clone(),
                summary: build_summary(&n.parameters),
                pos_graph: prev_positions
                    .get(&n.id)
                    .copied()
                    .unwrap_or((f32::NAN, f32::NAN)),
                inputs: n
                    .inputs
                    .iter()
                    .map(|p| PortView::from_kind(p.name.clone(), p.kind))
                    .collect(),
                outputs: n
                    .outputs
                    .iter()
                    .map(|p| PortView::from_kind(p.name.clone(), p.kind))
                    .collect(),
            })
            .collect();
        self.wires = snap
            .wires
            .iter()
            .map(|w| WireView {
                from_node: w.from_node,
                from_port: w.from_port.clone(),
                to_node: w.to_node,
                to_port: w.to_port.clone(),
            })
            .collect();

        // Two-step position assignment:
        //   1. Auto-layout writes columns/rows for every node, but
        //      we only keep its result for nodes that didn't have a
        //      previous position (the freshly added ones).
        //   2. Stored `editor_pos` from the def overrides on top for
        //      any node the user has explicitly moved.
        let unplaced_ids: Vec<u32> = self
            .nodes
            .iter()
            .filter(|n| !n.pos_graph.0.is_finite())
            .map(|n| n.id)
            .collect();
        if !unplaced_ids.is_empty() {
            // Save and restore positions of already-placed nodes so
            // auto_layout (which writes to every node) doesn't disturb
            // them. Cheap — graphs are small.
            let saved: Vec<((f32, f32), u32)> = self
                .nodes
                .iter()
                .filter(|n| n.pos_graph.0.is_finite())
                .map(|n| (n.pos_graph, n.id))
                .collect();
            self.auto_layout();
            for (pos, id) in saved {
                if let Some(n) = self.nodes.iter_mut().find(|n| n.id == id) {
                    n.pos_graph = pos;
                }
            }
        }
        for (view, snap_node) in self.nodes.iter_mut().zip(snap.nodes.iter()) {
            if let Some(p) = snap_node.editor_pos {
                view.pos_graph = p;
            }
        }
    }

    /// Compute node positions by topological depth. Sources (in-degree
    /// 0) go in column 0; each downstream node sits one column past
    /// its deepest predecessor. Within a column, nodes stack vertically
    /// in id order.
    fn auto_layout(&mut self) {
        let n = self.nodes.len();
        if n == 0 {
            return;
        }
        let mut depth = vec![0i32; n];
        // Map node id → index in self.nodes for adjacency walks.
        let id_to_idx: ahash::AHashMap<u32, usize> = self
            .nodes
            .iter()
            .enumerate()
            .map(|(i, nv)| (nv.id, i))
            .collect();

        // Iterative relaxation. With a DAG and small n it converges in
        // ≤ n passes; we cap at n+1 as a safety net against malformed
        // input (cycles can't occur — Graph::connect rejects them).
        for _ in 0..=n {
            let mut changed = false;
            for w in &self.wires {
                let (Some(&from_i), Some(&to_i)) =
                    (id_to_idx.get(&w.from_node), id_to_idx.get(&w.to_node))
                else {
                    continue;
                };
                let candidate = depth[from_i] + 1;
                if candidate > depth[to_i] {
                    depth[to_i] = candidate;
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }

        // Group by column, sorted by id within a column for determinism.
        let max_depth = depth.iter().copied().max().unwrap_or(0);
        let mut columns: Vec<Vec<usize>> = vec![Vec::new(); (max_depth as usize) + 1];
        for (i, &d) in depth.iter().enumerate() {
            columns[d as usize].push(i);
        }
        for col in columns.iter_mut() {
            col.sort_by_key(|&i| self.nodes[i].id);
        }
        for (col_idx, col) in columns.iter().enumerate() {
            // Vertical-center the column so taller and shorter columns
            // sit roughly aligned around a common axis.
            let col_height = col.len() as f32 * ROW_SPACING;
            let col_start_y = LAYOUT_ORIGIN.1 - col_height * 0.5 + ROW_SPACING * 0.5;
            for (row_idx, &node_idx) in col.iter().enumerate() {
                let x = LAYOUT_ORIGIN.0 + col_idx as f32 * COL_SPACING;
                let y = col_start_y + row_idx as f32 * ROW_SPACING;
                self.nodes[node_idx].pos_graph = (x, y);
            }
        }
    }

    // ── Coordinate transforms ───────────────────────────────────────

    fn to_screen(&self, viewport: Rect, gx: f32, gy: f32) -> (f32, f32) {
        let canvas_x = viewport.x;
        let canvas_y = viewport.y + HEADER_HEIGHT;
        (
            canvas_x + (gx + self.pan.0) * self.zoom,
            canvas_y + (gy + self.pan.1) * self.zoom,
        )
    }

    fn to_graph(&self, viewport: Rect, sx: f32, sy: f32) -> (f32, f32) {
        let canvas_x = viewport.x;
        let canvas_y = viewport.y + HEADER_HEIGHT;
        (
            (sx - canvas_x) / self.zoom - self.pan.0,
            (sy - canvas_y) / self.zoom - self.pan.1,
        )
    }

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

    /// Returns `true` if the cursor is over the header strip of the
    /// node it's hovering. Used to distinguish "click body to select"
    /// from "drag header to move".
    fn header_under(&self, viewport: Rect, sx: f32, sy: f32) -> Option<u32> {
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

    /// Hit-test ports near the cursor. Searches all output then input
    /// ports of every node, returning the first within `PORT_HIT_RADIUS`
    /// graph-space units of the cursor. Outputs take priority over
    /// inputs when both are nearby (only matters in degenerate layouts
    /// since ports are on opposite edges).
    fn port_under(&self, viewport: Rect, sx: f32, sy: f32) -> Option<PortHit> {
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

    // ── Input handlers ──────────────────────────────────────────────

    pub fn on_pointer_move(&mut self, viewport: Rect, sx: f32, sy: f32) {
        self.cursor = (sx, sy);
        match &self.drag_mode {
            DragMode::Pan => {
                let dx = (sx - self.drag_anchor.0) / self.zoom;
                let dy = (sy - self.drag_anchor.1) / self.zoom;
                self.pan = (self.drag_pan_start.0 + dx, self.drag_pan_start.1 + dy);
            }
            DragMode::NodeMove {
                node_id,
                anchor_offset,
                ..
            } => {
                let nid = *node_id;
                let offset = *anchor_offset;
                let (gx, gy) = self.to_graph(viewport, sx, sy);
                if let Some(n) = self.nodes.iter_mut().find(|n| n.id == nid) {
                    n.pos_graph = (gx - offset.0, gy - offset.1);
                }
            }
            DragMode::WireFrom { .. } => {
                // Cursor position is enough — render reads `self.cursor`.
            }
            DragMode::None => {
                self.hovered = self.node_under(viewport, sx, sy);
            }
        }
    }

    /// Begin panning unconditionally (e.g. middle-mouse drag).
    pub fn on_pan_button_down(&mut self, sx: f32, sy: f32) {
        self.drag_mode = DragMode::Pan;
        self.drag_anchor = (sx, sy);
        self.drag_pan_start = self.pan;
    }

    pub fn on_pan_button_up(&mut self) {
        if self.drag_mode.is_pan() {
            self.drag_mode = DragMode::None;
        }
    }

    /// Left-mouse button down. Priority order:
    /// 1. Output port → start wire-drag.
    /// 2. Input port → swallow (no action for V1 — disconnect lives
    ///    elsewhere later).
    /// 3. Node header → start node-move drag.
    /// 4. Node body → select.
    /// 5. Empty canvas → clear selection, start pan.
    pub fn on_left_button_down(&mut self, viewport: Rect, sx: f32, sy: f32) {
        if let Some(hit) = self.port_under(viewport, sx, sy) {
            if hit.is_output {
                self.drag_mode = DragMode::WireFrom {
                    from_node: hit.node_id,
                    from_port: hit.port_name,
                };
                return;
            }
            // Click on input port — V1 swallows the click (don't
            // accidentally start a pan). Disconnect-by-click is a
            // future polish item.
            return;
        }
        if let Some(node_id) = self.header_under(viewport, sx, sy) {
            self.selected = Some(node_id);
            let (gx, gy) = self.to_graph(viewport, sx, sy);
            if let Some(node) = self.nodes.iter().find(|n| n.id == node_id) {
                let anchor_offset = (gx - node.pos_graph.0, gy - node.pos_graph.1);
                self.drag_mode = DragMode::NodeMove {
                    node_id,
                    anchor_offset,
                    start_pos: node.pos_graph,
                };
            }
            return;
        }
        match self.node_under(viewport, sx, sy) {
            Some(id) => {
                self.selected = Some(id);
            }
            None => {
                self.selected = None;
                self.drag_mode = DragMode::Pan;
                self.drag_anchor = (sx, sy);
                self.drag_pan_start = self.pan;
            }
        }
    }

    pub fn on_left_button_up(&mut self, viewport: Rect, sx: f32, sy: f32) {
        let prev = std::mem::replace(&mut self.drag_mode, DragMode::None);
        match prev {
            DragMode::Pan | DragMode::None => {}
            DragMode::WireFrom {
                from_node,
                from_port,
            } => {
                // Only commit on drop over an input port — drop on
                // empty or an output cancels silently.
                if let Some(hit) = self.port_under(viewport, sx, sy)
                    && !hit.is_output
                    && hit.node_id != from_node
                {
                    self.pending_actions.push(PanelAction::ConnectPorts {
                        from_node,
                        from_port,
                        to_node: hit.node_id,
                        to_port: hit.port_name,
                    });
                }
            }
            DragMode::NodeMove { node_id, .. } => {
                if let Some(node) = self.nodes.iter().find(|n| n.id == node_id) {
                    self.pending_actions.push(PanelAction::MoveGraphNode {
                        node_id,
                        new_pos: node.pos_graph,
                    });
                }
            }
        }
    }

    pub fn cursor(&self) -> (f32, f32) {
        self.cursor
    }

    /// Currently-selected node id within the graph the canvas is
    /// viewing. Set by `on_left_button_down` when the click lands on
    /// a node. Read by the editor's right-sidebar panel to figure out
    /// which inner-node parameters to show as expose checkboxes.
    pub fn selected_node_id(&self) -> Option<u32> {
        self.selected
    }

    pub fn on_scroll(&mut self, viewport: Rect, dy: f32) {
        let (gx_before, gy_before) = self.to_graph(viewport, self.cursor.0, self.cursor.1);
        let factor = (dy * 0.0015).exp();
        let new_zoom = (self.zoom * factor).clamp(0.25, 4.0);
        self.zoom = new_zoom;
        let (gx_after, gy_after) = self.to_graph(viewport, self.cursor.0, self.cursor.1);
        self.pan.0 += gx_after - gx_before;
        self.pan.1 += gy_after - gy_before;
    }

    // ── Render ──────────────────────────────────────────────────────

    pub fn render(&self, ui: &mut UIRenderer, viewport: Rect) {
        ui.draw_rect(viewport.x, viewport.y, viewport.w, viewport.h, BG_COLOR);

        ui.draw_rect(viewport.x, viewport.y, viewport.w, HEADER_HEIGHT, HEADER_BG);
        let header_label = if self.nodes.is_empty() {
            "No active graph — open an effect card"
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

        self.draw_grid(ui, canvas);

        for wire in &self.wires {
            self.draw_wire(ui, viewport, wire);
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

        for node in &self.nodes {
            self.draw_node(ui, viewport, canvas, node);
        }
    }

    fn draw_ghost_wire(
        &self,
        ui: &mut UIRenderer,
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
        let ghost_color = [WIRE_COLOR[0], WIRE_COLOR[1], WIRE_COLOR[2], 0.55];
        let mut prev = cubic_bezier(0.0, sx0, sy0, cx0, cy0, cx1, cy1, sx1, sy1);
        for i in 1..=steps {
            let t = i as f32 / steps as f32;
            let curr = cubic_bezier(t, sx0, sy0, cx0, cy0, cx1, cy1, sx1, sy1);
            ui.draw_line(prev.0, prev.1, curr.0, curr.1, thickness, ghost_color);
            prev = curr;
        }
    }

    fn draw_grid(&self, ui: &mut UIRenderer, canvas: Rect) {
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

    fn draw_node(&self, ui: &mut UIRenderer, viewport: Rect, canvas: Rect, node: &NodeView) {
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
        let selected = self.selected == Some(node.id);
        let bg = if hovered { NODE_BG_HOVER } else { NODE_BG };
        let (border, border_w) = if selected {
            (NODE_BORDER_SELECTED, 2.0)
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
        ui.draw_rounded_rect(
            sx,
            sy,
            sw,
            header_h,
            NODE_HEADER_BG,
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

        // Inline summary line (e.g., "Mode: FoldX") so users can tell
        // what each node is doing without opening the inspector.
        // Truncate gracefully if it'd overflow the node width.
        if let Some(summary) = node.summary.as_deref() {
            let summary_size = (9.0 * self.zoom).max(7.0);
            let summary_y = sy + header_h + (SUMMARY_ROW_HEIGHT * self.zoom - summary_size) * 0.5;
            let max_chars =
                ((NODE_WIDTH * self.zoom - 16.0 * self.zoom) / (summary_size * 0.55)) as usize;
            let display: std::borrow::Cow<'_, str> = if summary.len() > max_chars && max_chars > 1 {
                std::borrow::Cow::Owned(format!(
                    "{}…",
                    &summary[..summary.len().min(max_chars.saturating_sub(1))]
                ))
            } else {
                std::borrow::Cow::Borrowed(summary)
            };
            ui.draw_text(
                sx + 8.0 * self.zoom,
                summary_y,
                &display,
                summary_size,
                TEXT_SECONDARY,
            );
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
    }

    fn draw_wire(&self, ui: &mut UIRenderer, viewport: Rect, wire: &WireView) {
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
        let dx = span_x.max(40.0) * 0.5;
        // Skip wires (those whose horizontal span exceeds ~1.5 columns)
        // arc downward so they read as "going around" intermediate
        // nodes rather than passing through them. Without this, fan-out
        // wires (e.g., SoftFocus's Source → Mix.a) emerge from the
        // intermediate node's right edge and look like they originate
        // there. Magnitude scales with span so longer skips arc more.
        let skip_bump = if span_x > 320.0 {
            ((span_x - 320.0) * 0.25).min(80.0)
        } else {
            0.0
        };
        let cx0 = sx0 + dx;
        let cy0 = sy0 + skip_bump;
        let cx1 = sx1 - dx;
        let cy1 = sy1 + skip_bump;

        // Sample the bezier into ~30 short line segments. Step count
        // scales with screen-space length so close-up curves stay smooth.
        let approx_len = ((sx1 - sx0).abs() + (sy1 - sy0).abs() + 2.0 * dx).max(40.0);
        let steps = (approx_len / 12.0).clamp(16.0, 64.0) as i32;
        let thickness = (1.6 * self.zoom).clamp(1.2, 2.4);
        let mut prev = cubic_bezier(0.0, sx0, sy0, cx0, cy0, cx1, cy1, sx1, sy1);
        for i in 1..=steps {
            let t = i as f32 / steps as f32;
            let curr = cubic_bezier(t, sx0, sy0, cx0, cy0, cx1, cy1, sx1, sy1);
            ui.draw_line(prev.0, prev.1, curr.0, curr.1, thickness, WIRE_COLOR);
            prev = curr;
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

fn hash_topology(snap: &GraphSnapshot) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = ahash::AHasher::default();
    snap.nodes.len().hash(&mut h);
    for n in &snap.nodes {
        n.id.hash(&mut h);
        n.type_id.hash(&mut h);
    }
    snap.wires.len().hash(&mut h);
    for w in &snap.wires {
        w.from_node.hash(&mut h);
        w.from_port.hash(&mut h);
        w.to_node.hash(&mut h);
        w.to_port.hash(&mut h);
    }
    h.finish()
}
