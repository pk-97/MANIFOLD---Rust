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

use manifold_renderer::node_graph::{
    GraphSnapshot, GroupSnapshot, NodeSnapshot, PortKindSnapshot, PortSnapshot, WireSnapshot,
};
use manifold_renderer::ui_renderer::{Layer, UIRenderer};
use manifold_ui::PanelAction;

use manifold_core::effect_graph_def::{GROUP_INPUT_TYPE_ID, GROUP_OUTPUT_TYPE_ID, GROUP_TYPE_ID};

use crate::mapping_popover::MappingPopover;

/// Set `GROUP_CANVAS_LOG=1` in the environment to print the gesture pipeline
/// (scope enter/exit, group/ungroup emits, marquee commits) to stderr. Cheap
/// when off — one env read per gesture, never per frame. The handoff doc's
/// debug-friendly mandate: a failing interaction should leave a trail.
fn group_log_enabled() -> bool {
    std::env::var_os("GROUP_CANVAS_LOG").is_some()
}

macro_rules! group_log {
    ($($arg:tt)*) => {
        if group_log_enabled() {
            eprintln!("[group-canvas] {}", format!($($arg)*));
        }
    };
}

const HEADER_HEIGHT: f32 = 28.0;
const NODE_WIDTH: f32 = 300.0;
const NODE_HEADER_HEIGHT: f32 = 22.0;
/// Padding around the preview strip inside a node, so the thumbnail reads as a
/// recessed screen rather than a fill bleeding to the node edges.
const PREVIEW_PAD: f32 = 6.0;
/// Inner width / height of a node's output-preview strip — a 16:9 image area
/// inset by `PREVIEW_PAD` on each side. Only nodes (and groups) that output an
/// image carry the strip; pure scalar nodes (param distributors, the generator
/// input) don't reserve the space. The strip sits in its own band between the
/// header and the param/port rows, so ports never overlap the thumbnail.
const PREVIEW_IMG_W: f32 = NODE_WIDTH - 2.0 * PREVIEW_PAD;
const PREVIEW_IMG_H: f32 = PREVIEW_IMG_W * 9.0 / 16.0;
/// Total vertical space the preview band occupies (image + pad above and below).
const PREVIEW_BAND_H: f32 = PREVIEW_IMG_H + 2.0 * PREVIEW_PAD;
/// Height of one on-node parameter row: label + value on one line, with a
/// thin fill bar underneath for ranged values. Nodes carry their params on
/// their face so you read (and, in a later pass, tune) them where you are,
/// instead of darting to a side panel.
const PARAM_ROW_H: f32 = 18.0;
/// Pixels of horizontal drag that scrub a value across its full min..max
/// range when editing a param on the node face. Matches the inspector
/// sidebar's feel (`DRAG_FULL_RANGE_PX`).
const PARAM_SCRUB_FULL_RANGE_PX: f32 = 240.0;
/// Below this zoom, nodes render header + ports only (no param/summary
/// text): the text would be sub-pixel mush, so the zoomed-out graph reads as
/// clean colour-coded boxes instead of an unreadable wall.
const PARAM_LOD_ZOOM: f32 = 0.5;
const PORT_ROW_HEIGHT: f32 = 18.0;
const PORT_RADIUS: f32 = 4.0;
const PORT_COL_WIDTH: f32 = 10.0;
const NODE_CORNER: f32 = 6.0;

// Auto-layout grid spacing. Wide enough that 300px nodes never touch
// horizontally (NODE_WIDTH + ~60px breathing room for the wires between cols).
const COL_SPACING: f32 = 360.0;
const LAYOUT_ORIGIN: (f32, f32) = (60.0, 60.0);
/// Vertical gap between two stacked nodes (or routing lanes) within a
/// column. Node heights vary, so the layout spaces by `height + VGAP`
/// rather than a fixed centre-to-centre pitch.
const LAYOUT_VGAP: f32 = 26.0;
/// Height a virtual routing waypoint occupies in a column. A wire that
/// spans more than one column gets one of these per column it crosses so
/// the crossing-reduction pass can see it and route around it; small so
/// the lane it reserves is thin.
const LAYOUT_DUMMY_H: f32 = 6.0;
/// Up/down sweeps for crossing minimisation. Each sweep reorders every
/// column by the median position of its neighbours; the best-scoring
/// ordering across all sweeps is kept. A handful converges on graphs
/// this size.
const LAYOUT_ORDER_ITERS: usize = 8;
/// Forward/backward passes for vertical coordinate assignment. Each pass
/// pulls every node toward the average height of what it connects to,
/// then resolves overlaps; alternating direction straightens wires.
const LAYOUT_COORD_ITERS: usize = 12;

// ── Wire routing (draw-only) ──
/// One muted-violet colour for every feedback return path, so they read as a
/// family distinct from the blue data / orange control wires regardless of the
/// source port's kind.
const RETURN_WIRE_COLOR: [f32; 3] = [0.62, 0.55, 0.78];
/// How far (graph px) above the higher endpoint's node-top a return path arcs,
/// so it clears the node band and reads as "going around".
const RETURN_ARC_CLEAR: f32 = 36.0;
/// Return paths are dashed: `RETURN_DASH` sampled segments drawn, then the same
/// count skipped, repeating — a feedback wire at a glance.
const RETURN_DASH: i32 = 3;
/// Stagger the incoming-wire landing handle by port depth only on nodes with at
/// least this many inputs, so a dense fan-in (e.g. a ~15-input tracking node)
/// splays into the input stack instead of overlapping. Small mixers (a/b,
/// numbered slots) keep their uniform handles.
const FANIN_STAGGER_MIN: usize = 6;

const BG_COLOR: [f32; 4] = [0.10, 0.10, 0.12, 1.0];
const HEADER_BG: [f32; 4] = [0.14, 0.14, 0.17, 1.0];
const GRID_DOT: [f32; 4] = [1.0, 1.0, 1.0, 0.06];
const NODE_BG: [f32; 4] = [0.18, 0.18, 0.22, 1.0];
const NODE_BG_HOVER: [f32; 4] = [0.22, 0.22, 0.27, 1.0];
/// Recessed "screen" the preview thumbnail is blitted over (and the letterbox
/// colour for non-16:9 outputs). Near-black so an empty / loading strip reads
/// as an off monitor, not a hole in the node.
const PREVIEW_SCREEN_BG: [f32; 4] = [0.04, 0.04, 0.05, 1.0];
const PREVIEW_SCREEN_BORDER: [f32; 4] = [0.0, 0.0, 0.0, 0.5];
const NODE_HEADER_BG: [f32; 4] = [0.28, 0.30, 0.42, 1.0];
const NODE_BORDER: [f32; 4] = [0.0, 0.0, 0.0, 0.6];
const NODE_BORDER_SELECTED: [f32; 4] = [0.50, 0.78, 1.00, 1.0];
const PORT_TEXTURE2D_COLOR: [f32; 4] = [0.50, 0.78, 1.00, 1.0];
const PORT_TEXTURE3D_COLOR: [f32; 4] = [0.78, 0.50, 1.00, 1.0];
const PORT_SCALAR_COLOR: [f32; 4] = [1.00, 0.78, 0.40, 1.0];
const PORT_ARRAY_COLOR: [f32; 4] = [0.50, 1.00, 0.62, 1.0];
const PORT_CAMERA_COLOR: [f32; 4] = [1.00, 0.55, 0.55, 1.0];
const PORT_LIGHT_COLOR: [f32; 4] = [1.00, 0.95, 0.55, 1.0];
const PORT_MATERIAL_COLOR: [f32; 4] = [0.95, 0.65, 0.40, 1.0];
/// Ghost-wire tint while dragging over a compatible / incompatible input port —
/// a live green/red "this will / won't connect" hint, so a mis-wire is caught
/// before the drop, not after. The actual connect still validates server-side.
const CONNECT_OK_COLOR: [f32; 4] = [0.42, 0.88, 0.52, 0.85];
const CONNECT_BAD_COLOR: [f32; 4] = [0.92, 0.38, 0.38, 0.85];
/// Group node tint. A group reads as a distinct, slightly heavier box than an
/// atom so a complex graph shows its structure at a glance — teal-leaning
/// header + a faint teal body wash, the colour we reserve for "container".
const GROUP_HEADER_BG: [f32; 4] = [0.18, 0.34, 0.40, 1.0];
/// Preset group accent colours the recolour gesture cycles through — muted so
/// they read as labels, not alerts, under stage lighting. The first entry is
/// the default teal (`GROUP_HEADER_BG`), so cycling from untinted lands on a
/// real colour immediately.
const GROUP_TINT_PALETTE: [[f32; 4]; 6] = [
    [0.18, 0.34, 0.40, 1.0], // teal (default)
    [0.40, 0.24, 0.42, 1.0], // plum
    [0.42, 0.30, 0.18, 1.0], // amber
    [0.22, 0.40, 0.26, 1.0], // moss
    [0.40, 0.22, 0.24, 1.0], // rust
    [0.24, 0.28, 0.44, 1.0], // indigo
];
const GROUP_BODY_BG: [f32; 4] = [0.16, 0.22, 0.25, 1.0];
const GROUP_BODY_BG_HOVER: [f32; 4] = [0.20, 0.27, 0.30, 1.0];
/// Border on a group's bounding box and the "enter" chevron, brighter than a
/// plain node border so the affordance ("this opens") is legible.
const GROUP_ACCENT: [f32; 4] = [0.45, 0.82, 0.88, 1.0];
/// Breadcrumb bar text + the "› " separators, drawn in the canvas header when
/// the view is inside one or more groups.
const BREADCRUMB_TEXT: [u8; 4] = [180, 215, 220, 255];
const BREADCRUMB_DIM: [u8; 4] = [120, 130, 140, 255];
/// Translucent backdrop behind the debug overlay readout so it stays legible
/// over busy graph content.
const DEBUG_OVERLAY_BG: [f32; 4] = [0.0, 0.0, 0.0, 0.62];
const DEBUG_OVERLAY_TEXT: [u8; 4] = [120, 230, 160, 255];
/// Breadcrumb font size (logical px). The bitmap font is ~0.55em wide; the
/// segment layout uses that ratio so render and hit-test agree.
const BREADCRUMB_FONT: f32 = 12.0;
/// Rubber-band selection rectangle: a faint blue wash with a brighter border.
const MARQUEE_FILL: [f32; 4] = [0.50, 0.78, 1.00, 0.12];
const MARQUEE_BORDER: [f32; 4] = [0.50, 0.78, 1.00, 0.80];
/// On-node param fill bar: a faint track plus a brighter fill showing where
/// a ranged value sits between its declared min and max.
const PARAM_FILL_BG: [f32; 4] = [1.0, 1.0, 1.0, 0.07];
const PARAM_FILL_FG: [f32; 4] = [0.50, 0.78, 1.00, 0.55];
/// Sparkline trace colour — the same soft cyan as the fill bar, a touch brighter
/// so the moving line reads against the node body without shouting.
const SPARKLINE_COLOR: [f32; 4] = [0.55, 0.82, 1.00, 0.85];
const TEXT_PRIMARY: [u8; 4] = [220, 220, 230, 255];
const TEXT_SECONDARY: [u8; 4] = [150, 150, 165, 255];
const TEXT_HEADER: [u8; 4] = [240, 240, 250, 255];
/// Hover-tooltip chrome: a near-opaque dark card with a faint border,
/// drawn above the nodes so the help line reads cleanly over any graph.
const TOOLTIP_BG: [f32; 4] = [0.10, 0.10, 0.13, 0.97];
const TOOLTIP_BORDER: [f32; 4] = [0.45, 0.48, 0.60, 0.85];
const TOOLTIP_TEXT: [u8; 4] = [224, 226, 236, 255];
/// Pink chip behind the "Reset to Default" header button —
/// same family as the MOD badge on the effect card so the
/// "you are diverged" cue is consistent across surfaces.
const RESET_BUTTON_BG: [f32; 4] = [0.78, 0.27, 0.45, 0.90];
const RESET_BUTTON_W: f32 = 124.0;
const RESET_BUTTON_H: f32 = 18.0;
/// Gap between the reset button and the zoom indicator on its right.
const RESET_BUTTON_RIGHT_GAP: f32 = 96.0;

#[derive(Debug, Clone)]
struct PortView {
    name: String,
    color: [f32; 4],
    /// True for scalar (control/value) ports. Wires out of these are the
    /// "set once" driver bindings that dominate the spaghetti, so they get
    /// dimmed unless their node is focused.
    is_control: bool,
}

impl PortView {
    // Takes `&PortKindSnapshot` because the snapshot's `Array`
    // variant now carries owned channel metadata (post-Phase-6); a
    // by-value signature would force every caller to clone the
    // channels Vec just to read the tag.
    fn from_kind(name: String, kind: &PortKindSnapshot) -> Self {
        let color = match kind {
            PortKindSnapshot::Texture2D => PORT_TEXTURE2D_COLOR,
            // Typed Texture2D shares the texture-port colour — the
            // four-slot channel signature is a tooltip-level detail,
            // not a separate port category. See
            // `docs/CHANNEL_TYPE_SYSTEM.md` §17.
            PortKindSnapshot::Texture2DTyped { .. } => PORT_TEXTURE2D_COLOR,
            PortKindSnapshot::Texture3D => PORT_TEXTURE3D_COLOR,
            PortKindSnapshot::Scalar => PORT_SCALAR_COLOR,
            PortKindSnapshot::Array { .. } => PORT_ARRAY_COLOR,
            PortKindSnapshot::Camera => PORT_CAMERA_COLOR,
            PortKindSnapshot::Light => PORT_LIGHT_COLOR,
            PortKindSnapshot::Material => PORT_MATERIAL_COLOR,
        };
        let is_control = matches!(kind, PortKindSnapshot::Scalar);
        Self {
            name,
            color,
            is_control,
        }
    }
}

#[derive(Debug, Clone)]
struct NodeView {
    id: u32,
    /// Stable [`manifold_core::NodeId`] of this node — the addressing identity
    /// that survives grouping. Empty for anonymous boundary nodes. Used to match
    /// the per-frame live param tap (`ContentState::live_node_params`, keyed by
    /// node_id) onto this view so on-face values reflect live modulation.
    node_id: manifold_core::NodeId,
    /// Stable string handle from the def, if any (`None` for boundary /
    /// anonymous nodes). Used to mint a collision-free handle when this
    /// node's level gets a new group, and by Ctrl+G's payload.
    handle: Option<String>,
    title: String,
    /// The node's parameters, drawn as compact rows on the node face when
    /// the node is expanded, so you can read and tune each one in place.
    /// Empty if the node has no params.
    params: Vec<ParamView>,
    /// One-line summary of the node's key param (e.g. "Mode: FoldX"), shown
    /// when the node is collapsed so a folded node still tells you its most
    /// important value at a glance. `None` if the node has no params.
    summary: Option<String>,
    /// Whether this node is collapsed (header + one summary line) rather than
    /// expanded (every param row). Nodes default to collapsed so a complex
    /// graph reads cleanly; expand the one you're tuning. Mirrors
    /// `GraphCanvas::collapsed` for this node so layout/drawing skip the map.
    collapsed: bool,
    /// Header tint for this node's `Category` (Color & Tone, Noise, Distort,
    /// ...), so the graph reads by family at a glance. `NODE_HEADER_BG` for
    /// nodes with no descriptor / `Uncategorized`.
    header_color: [f32; 4],
    /// Top-left corner in graph-space (logical pixels, pre pan/zoom).
    pos_graph: (f32, f32),
    inputs: Vec<PortView>,
    outputs: Vec<PortView>,
    /// Mirrors `NodeSnapshot::breaks_dependency_cycle`. Wires terminating
    /// here close a feedback loop; `auto_layout` skips them so depth
    /// propagation doesn't accumulate around the loop.
    breaks_dependency_cycle: bool,
    /// True when this node is a group (subgraph) instance — `type_id ==
    /// GROUP_TYPE_ID`. Drives the distinct group rendering and the
    /// double-click-to-enter gesture. Its `inputs`/`outputs` are the group's
    /// interface ports; the body lives in the snapshot and is re-resolved by
    /// scope, not stored on the view.
    is_group: bool,
    /// Group accent colour (`GroupDef::tint`), painted on the group header in
    /// place of the default group tint. `None` for ordinary nodes and untinted
    /// groups. Cycled by the recolour gesture on a selected group.
    group_tint: Option<[f32; 4]>,
    /// Friendly one-line summary from the node's `NodeDescriptor`, shown
    /// as a hover tooltip over the node's header/body. `None` for groups
    /// (no descriptor) and for any node whose author left the summary
    /// blank. Resolved once on the topology rebuild — it never changes.
    tooltip: Option<String>,
    /// Stable [`NodeId`] whose captured output texture this node shows in its
    /// preview strip, or `None` for nodes that output no image (param
    /// distributors, the generator input). For an ordinary image node this is
    /// its own `node_id`; for a **group** it's the inner node producing the
    /// group's primary output (resolved once at build time), so a collapsed
    /// group still previews what it emits without any extra capture — that
    /// inner node is already a cell in the flattened atlas. The presence of a
    /// value is what gives the node a preview band ([`Self::preview_h`]).
    preview_node_id: Option<manifold_core::NodeId>,
}

/// Whether a port carries an image (the only kind the thumbnail atlas captures).
fn port_kind_is_image(kind: &PortKindSnapshot) -> bool {
    matches!(
        kind,
        PortKindSnapshot::Texture2D
            | PortKindSnapshot::Texture2DTyped { .. }
            | PortKindSnapshot::Texture3D
    )
}

/// The stable [`NodeId`] whose captured texture a node previews, or `None` for a
/// node that emits no image. Ordinary node → its own id (if it has an image
/// output); group → the inner producer of its primary output. See
/// [`NodeView::preview_node_id`].
fn node_preview_target(n: &NodeSnapshot) -> Option<manifold_core::NodeId> {
    if let Some(body) = n.group.as_deref() {
        let port = group_primary_output_port(&n.outputs)?;
        group_output_producer(body, port)
    } else if !n.node_id.as_str().is_empty()
        && n.outputs.iter().any(|p| port_kind_is_image(&p.kind))
    {
        Some(n.node_id.clone())
    } else {
        None
    }
}

/// A group's primary image output port name — the first output that carries a
/// texture. Unlike `manifold_core::flatten::primary_output_port` (which falls
/// back to the first output of any kind), this is image-only: a group with no
/// image output gets no preview band, exactly like an ordinary scalar node.
fn group_primary_output_port(outputs: &[PortSnapshot]) -> Option<&str> {
    outputs
        .iter()
        .find(|p| port_kind_is_image(&p.kind))
        .map(|p| p.name.as_str())
}

/// The stable [`NodeId`] of the concrete inner node producing `port` of a group
/// `body`, resolving through nested sub-groups. Snapshot-side mirror of
/// `manifold_core::flatten::producer_for_output`. `None` if the port is fed by
/// the group's own input (an unsupported passthrough the flattener also rejects)
/// or has no producer.
fn group_output_producer(body: &GroupSnapshot, port: &str) -> Option<manifold_core::NodeId> {
    let out_boundary = body
        .nodes
        .iter()
        .find(|n| n.type_id == GROUP_OUTPUT_TYPE_ID)?;
    let wire = body
        .wires
        .iter()
        .find(|w| w.to_node == out_boundary.id && w.to_port == port)?;
    let producer = body.nodes.iter().find(|n| n.id == wire.from_node)?;
    if let Some(inner) = producer.group.as_deref() {
        group_output_producer(inner, &wire.from_port)
    } else if producer.type_id == GROUP_INPUT_TYPE_ID {
        None
    } else {
        Some(producer.node_id.clone())
    }
}

impl NodeView {
    fn height(&self) -> f32 {
        let port_rows = self.inputs.len().max(self.outputs.len()) as f32;
        NODE_HEADER_HEIGHT + self.preview_h() + self.body_h() + port_rows * PORT_ROW_HEIGHT + 6.0
    }

    /// Height of the output-preview band below the header: the 16:9 strip plus
    /// its padding, or `0` for a node that emits no image. Zoom-independent.
    fn preview_h(&self) -> f32 {
        if self.preview_node_id.is_some() {
            PREVIEW_BAND_H
        } else {
            0.0
        }
    }

    /// Height of the body block below the header: collapsed shows the single
    /// summary line (if any), expanded shows every param row. Zoom-independent
    /// so port positions stay put as you zoom (the LOD cull is draw-only).
    fn body_h(&self) -> f32 {
        if self.collapsed {
            if self.summary.is_some() {
                PARAM_ROW_H
            } else {
                0.0
            }
        } else {
            self.params.len() as f32 * PARAM_ROW_H
        }
    }

    /// Y offset where port rows start, below the header, preview band, and body
    /// block. Ports live in their own band beneath the preview, so the strip and
    /// the port dots/labels never overlap.
    fn ports_y_offset(&self) -> f32 {
        NODE_HEADER_HEIGHT + self.preview_h() + self.body_h()
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

    /// Y-offset (from the node's top edge) of the named input port's centre.
    /// Used by auto-layout to align a node so this wire's two ports line up,
    /// rather than aligning box-centre to box-centre. Falls back to the node
    /// mid-height for an unknown name (shouldn't happen for a live wire).
    fn input_port_offset(&self, name: &str) -> f32 {
        match self.inputs.iter().position(|p| p.name == name) {
            Some(idx) => {
                self.ports_y_offset() + idx as f32 * PORT_ROW_HEIGHT + PORT_ROW_HEIGHT * 0.5
            }
            None => self.height() * 0.5,
        }
    }

    /// Y-offset (from the node's top edge) of the named output port's centre.
    /// Companion to [`input_port_offset`](Self::input_port_offset).
    fn output_port_offset(&self, name: &str) -> f32 {
        match self.outputs.iter().position(|p| p.name == name) {
            Some(idx) => {
                self.ports_y_offset() + idx as f32 * PORT_ROW_HEIGHT + PORT_ROW_HEIGHT * 0.5
            }
            None => self.height() * 0.5,
        }
    }
}

/// One parameter as shown on the node face: its label, the formatted
/// current value, and an optional 0..1 fill fraction for ranged values
/// (drives the thin bar under the row). Owned so it survives the
/// content/UI snapshot boundary.
#[derive(Debug, Clone)]
struct ParamView {
    /// Inner-param name, used as `param_name` when a scrub emits
    /// `SetGraphNodeParam`.
    name: String,
    label: String,
    /// Snapshot kind + declared range, retained so a per-frame live value
    /// ([`GraphCanvas::apply_live_values`]) can reformat the value string and
    /// fill bar exactly as the structural snapshot did, without re-snapshotting.
    kind: manifold_renderer::node_graph::ParamSnapshotKind,
    range: Option<(f32, f32)>,
    value: String,
    /// `Some(0..1)` position of the current value within its declared
    /// range, for the fill bar. `None` for params with no numeric range
    /// (enums, bools, triggers, or floats whose ParamDef declared none).
    fill: Option<f32>,
    /// Scrub metadata for in-place editing. `Some` only for numeric params
    /// (Float/Angle/Frequency/Int) that declared a range — those can be
    /// dragged on the node face. `None` params stay read-only on the canvas
    /// (still editable via the inspector sidebar).
    scrub: Option<ScrubInfo>,
    /// Plain-English help line for this param, from the `param_doc`
    /// side-channel keyed by `(node type_id, param name)`. Shown as a
    /// hover tooltip over the param row. `None` if the node author didn't
    /// register one. Static per `(type_id, name)`, so it's resolved once
    /// on the topology rebuild and carried forward on value-only refreshes.
    tooltip: Option<String>,
}

/// What a draggable on-node param needs to turn a horizontal drag into a
/// new value: its range, the value at press time, and whether to round.
#[derive(Debug, Clone, Copy)]
struct ScrubInfo {
    range: (f32, f32),
    current_value: f32,
    is_int: bool,
}

/// Format one parameter snapshot for on-node display: a short value string
/// plus, when the param has a numeric range, the 0..1 position of the
/// current value within it. Value formatting mirrors the inspector
/// (degrees for angles, Hz for frequencies, enum labels, On/Off).
fn format_param_for_node(p: &manifold_renderer::node_graph::ParamSnapshot) -> ParamView {
    use manifold_renderer::node_graph::ParamSnapshotKind;
    // Numeric / bool kinds share their formatting + fill with the per-frame
    // live-value path; enum / trigger / other are display-only here.
    let (value, fill) = match numeric_value_fill(p.kind, p.current_value, p.range) {
        Some(vf) => vf,
        None => {
            let v = match p.kind {
                ParamSnapshotKind::Enum => p
                    .enum_labels
                    .as_ref()
                    .and_then(|labels| labels.get(p.current_value as usize).cloned())
                    .unwrap_or_else(|| format!("{}", p.current_value as i64)),
                ParamSnapshotKind::Trigger => format!("{}", p.current_value as i64),
                // Colour reads as a hex string on the face; the swatch + channel
                // editor lives in the inspector sidebar.
                ParamSnapshotKind::Color => p
                    .vec_value
                    .map(format_color_hex)
                    .unwrap_or_else(|| "—".to_string()),
                ParamSnapshotKind::Vec2 => p
                    .vec_value
                    .map(|v| format!("{:.2}, {:.2}", v[0], v[1]))
                    .unwrap_or_else(|| "—".to_string()),
                ParamSnapshotKind::Vec3 => p
                    .vec_value
                    .map(|v| format!("{:.2}, {:.2}, {:.2}", v[0], v[1], v[2]))
                    .unwrap_or_else(|| "—".to_string()),
                ParamSnapshotKind::Vec4 => p
                    .vec_value
                    .map(|v| format!("{:.2}, {:.2}, {:.2}, {:.2}", v[0], v[1], v[2], v[3]))
                    .unwrap_or_else(|| "—".to_string()),
                // String + Table both read out of `summary` (the string value /
                // the table dimensions).
                ParamSnapshotKind::String | ParamSnapshotKind::Other => {
                    p.summary.clone().unwrap_or_else(|| "—".to_string())
                }
                // Numeric/bool kinds are handled by numeric_value_fill above.
                _ => String::new(),
            };
            (v, None)
        }
    };
    let scrub = scrub_for(p.kind, p.current_value, p.range);
    ParamView {
        name: p.name.clone(),
        label: p.label.clone(),
        kind: p.kind,
        range: p.range,
        value,
        fill,
        scrub,
        // Resolved by the caller that knows the owning node's type_id;
        // this formatter only sees the param snapshot.
        tooltip: None,
    }
}

/// Format the value string + fill fraction for the param kinds the on-node face
/// can render and the live tap can drive: continuous numerics (Float / Angle /
/// Frequency / Int) and bools. `None` for kinds whose display needs more than a
/// scalar (enum labels, trigger, multi-component "Other"). The single source of
/// truth for both the structural snapshot ([`format_param_for_node`]) and the
/// per-frame live refresh ([`GraphCanvas::apply_live_values`]), so a frozen and
/// a modulated value format identically.
fn numeric_value_fill(
    kind: manifold_renderer::node_graph::ParamSnapshotKind,
    value: f32,
    range: Option<(f32, f32)>,
) -> Option<(String, Option<f32>)> {
    use manifold_renderer::node_graph::ParamSnapshotKind;
    let s = match kind {
        ParamSnapshotKind::Float => format!("{value:.2}"),
        // Stored radians, shown as degrees (see ParamType::Angle).
        ParamSnapshotKind::Angle => format!("{:.0}°", value.to_degrees()),
        // Stored rad/s, shown as Hz (see ParamType::Frequency).
        ParamSnapshotKind::Frequency => format!("{:.2} Hz", value / std::f32::consts::TAU),
        ParamSnapshotKind::Int => format!("{}", value as i64),
        ParamSnapshotKind::Bool => if value >= 0.5 { "On" } else { "Off" }.to_string(),
        _ => return None,
    };
    let fill = match kind {
        ParamSnapshotKind::Float
        | ParamSnapshotKind::Angle
        | ParamSnapshotKind::Frequency
        | ParamSnapshotKind::Int => range.map(|(lo, hi)| {
            if hi > lo {
                ((value - lo) / (hi - lo)).clamp(0.0, 1.0)
            } else {
                0.0
            }
        }),
        _ => None,
    };
    Some((s, fill))
}

/// Scrub metadata for the draggable numeric kinds (Float / Angle / Frequency /
/// Int) that declared a range. `None` otherwise.
fn scrub_for(
    kind: manifold_renderer::node_graph::ParamSnapshotKind,
    value: f32,
    range: Option<(f32, f32)>,
) -> Option<ScrubInfo> {
    use manifold_renderer::node_graph::ParamSnapshotKind;
    match kind {
        ParamSnapshotKind::Float
        | ParamSnapshotKind::Angle
        | ParamSnapshotKind::Frequency
        | ParamSnapshotKind::Int => range.map(|(lo, hi)| ScrubInfo {
            range: (lo, hi),
            current_value: value,
            is_int: matches!(kind, ParamSnapshotKind::Int),
        }),
        _ => None,
    }
}

/// Format an RGBA colour (0..1 components) as `#RRGGBB` for the node-face value
/// cell. Alpha is dropped from the compact face string (it's still editable on
/// the inspector's A channel). Shared with the inspector's swatch path.
fn format_color_hex(c: [f32; 4]) -> String {
    let to_u8 = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!("#{:02X}{:02X}{:02X}", to_u8(c[0]), to_u8(c[1]), to_u8(c[2]))
}

/// Whether a sparkline history actually moves — `true` only if its range spans
/// more than a hair. A dead-flat trace (a static, unmodulated knob) isn't worth
/// the ink, so the node face stays clean until something drives the param.
fn spark_has_variation(hist: &std::collections::VecDeque<f32>) -> bool {
    let mut lo = f32::INFINITY;
    let mut hi = f32::NEG_INFINITY;
    for &v in hist {
        lo = lo.min(v);
        hi = hi.max(v);
    }
    hi - lo > 0.01
}

/// Pick the node's most informative param and format it as a one-line
/// summary ("Mode: FoldX", "Scale: 0.02") shown on the collapsed node face.
/// Prefers an enum (its label is descriptive), then a numeric, else the
/// first param. `None` for param-less nodes.
fn node_summary(params: &[manifold_renderer::node_graph::ParamSnapshot]) -> Option<String> {
    use manifold_renderer::node_graph::ParamSnapshotKind;
    let pick = params
        .iter()
        .find(|p| p.kind == ParamSnapshotKind::Enum)
        .or_else(|| {
            params.iter().find(|p| {
                matches!(
                    p.kind,
                    ParamSnapshotKind::Float
                        | ParamSnapshotKind::Angle
                        | ParamSnapshotKind::Frequency
                        | ParamSnapshotKind::Int
                )
            })
        })
        .or_else(|| params.first())?;
    let pv = format_param_for_node(pick);
    Some(format!("{}: {}", pv.label, pv.value))
}

/// Wrap `text` to lines no wider than `max_chars`, breaking on spaces.
/// A single word longer than the limit is left whole — it overflows the
/// box a touch rather than being chopped mid-word. Only the hover tooltip
/// calls this, so the per-call allocation is off any hot path.
fn wrap_text(text: &str, max_chars: usize) -> Vec<String> {
    let max = max_chars.max(1);
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        if line.is_empty() {
            line.push_str(word);
        } else if line.chars().count() + 1 + word.chars().count() <= max {
            line.push(' ');
            line.push_str(word);
        } else {
            lines.push(std::mem::take(&mut line));
            line.push_str(word);
        }
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

/// Muted header tint per node `Category`, so the graph reads at a glance by
/// family (Color & Tone warm, Noise teal, Distort purple, ...). Kept low in
/// saturation and brightness so headers stay subtle on the dark canvas; an
/// exhaustive match means a new `Category` variant forces a colour choice
/// here rather than silently defaulting.
fn category_header_color(cat: manifold_renderer::node_graph::Category) -> [f32; 4] {
    use manifold_renderer::node_graph::Category as C;
    match cat {
        C::ColorAndTone => [0.40, 0.30, 0.22, 1.0],
        C::BlurAndSharpen => [0.22, 0.30, 0.40, 1.0],
        C::DistortAndWarp => [0.34, 0.24, 0.40, 1.0],
        C::Stylize => [0.40, 0.24, 0.34, 1.0],
        C::Generate => [0.24, 0.36, 0.28, 1.0],
        C::Noise => [0.22, 0.36, 0.36, 1.0],
        C::Mask => [0.30, 0.30, 0.34, 1.0],
        C::Composite => [0.26, 0.28, 0.42, 1.0],
        C::Geometry3D => [0.30, 0.26, 0.42, 1.0],
        C::MaterialsAndLighting => [0.38, 0.32, 0.22, 1.0],
        C::Particles2D => [0.24, 0.34, 0.40, 1.0],
        C::Particles3D => [0.22, 0.32, 0.42, 1.0],
        C::Control => [0.36, 0.34, 0.22, 1.0],
        C::DetectionAndSampling => [0.40, 0.26, 0.26, 1.0],
        C::MathAndConvert => [0.30, 0.30, 0.30, 1.0],
        C::Routing => [0.26, 0.30, 0.38, 1.0],
        C::FieldsAndCoordinates => [0.24, 0.34, 0.34, 1.0],
        C::Uncategorized => NODE_HEADER_BG,
    }
}

/// Median of a slice of values (mutates it by sorting). Returns `0.0` for an
/// empty slice. Used by both layout passes: the ordering pass takes the
/// median neighbour *position*, the coordinate pass the median target *y*.
fn layout_median(vals: &mut [f32]) -> f32 {
    vals.sort_by(|a, b| a.partial_cmp(b).unwrap_or(core::cmp::Ordering::Equal));
    let len = vals.len();
    if len == 0 {
        0.0
    } else if len % 2 == 1 {
        vals[len / 2]
    } else {
        0.5 * (vals[len / 2 - 1] + vals[len / 2])
    }
}

/// Push apart the `desired` y-positions of one column so adjacent vertices
/// keep `gap` of clearance and never reorder, then rigid-shift the whole
/// column back so its mean matches the mean of the requested positions. The
/// shift keeps the column centred where alignment wanted it instead of
/// drifting downward each pass. `desired[i]` pairs with `col[i]`.
fn layout_resolve_overlaps(col: &[usize], height: &[f32], desired: &mut [f32], gap: f32) {
    let len = col.len();
    if len == 0 {
        return;
    }
    let mean_before: f32 = desired.iter().sum::<f32>() / len as f32;
    for i in 1..len {
        let min_y = desired[i - 1] + height[col[i - 1]] + gap;
        if desired[i] < min_y {
            desired[i] = min_y;
        }
    }
    let mean_after: f32 = desired.iter().sum::<f32>() / len as f32;
    let shift = mean_before - mean_after;
    for d in desired.iter_mut() {
        *d += shift;
    }
}

/// A layered ("Sugiyama"-style) auto-layout. Nodes are assigned to
/// left-to-right columns by dependency depth (done by the caller), ordered
/// within each column to minimise wire crossings, then nudged vertically so
/// connected ports line up and wires run straight.
///
/// Vertices are *layout vertices*, addressed by `lvid`. The first `n` are the
/// real graph nodes (lvid == index into `GraphCanvas::nodes`); the rest are
/// virtual routing waypoints inserted for wires that span more than one
/// column, so a long wire participates in ordering and alignment instead of
/// slicing diagonally across the graph. Waypoints are discarded once the real
/// nodes' positions are read back.
struct LayeredLayout {
    num_cols: usize,
    /// Column index per layout vertex.
    column: Vec<usize>,
    /// Layout height per vertex (real node height, or `LAYOUT_DUMMY_H`).
    height: Vec<f32>,
    /// Vertices in each column, top to bottom. Mutated by the ordering pass.
    order: Vec<Vec<usize>>,
    /// Per vertex `v`, the segments arriving from the previous column:
    /// `(u, up_off, down_off)` where `u` sits one column left, `up_off` is the
    /// y-offset of the wire's exit port on `u`, and `down_off` its entry port
    /// on `v`. Alignment lines those two ports up, not the boxes.
    up_edges: Vec<Vec<(usize, f32, f32)>>,
    /// Mirror of `up_edges` pointing forward: segments leaving `v` toward the
    /// next column. `(w, up_off, down_off)` with `up_off` the exit port on `v`.
    down_edges: Vec<Vec<(usize, f32, f32)>>,
}

impl LayeredLayout {
    /// Position index (0 = top) of every vertex within its column.
    fn positions(&self) -> Vec<usize> {
        let mut pos = vec![0usize; self.column.len()];
        for col in &self.order {
            for (i, &v) in col.iter().enumerate() {
                pos[v] = i;
            }
        }
        pos
    }

    /// Total wire crossings across all adjacent column pairs, counted as
    /// inversions between the two endpoints' position indices. O(edges²) per
    /// column boundary — fine for graphs this size.
    fn count_crossings(&self) -> usize {
        let pos = self.positions();
        let mut total = 0;
        for c in 0..self.num_cols.saturating_sub(1) {
            let mut edges: Vec<(usize, usize)> = Vec::new();
            for &v in &self.order[c] {
                for &(w, _, _) in &self.down_edges[v] {
                    edges.push((pos[v], pos[w]));
                }
            }
            for i in 0..edges.len() {
                for j in (i + 1)..edges.len() {
                    let (a, b) = (edges[i], edges[j]);
                    if (a.0 < b.0 && a.1 > b.1) || (a.0 > b.0 && a.1 < b.1) {
                        total += 1;
                    }
                }
            }
        }
        total
    }

    /// Reorder one column by the median position of each vertex's neighbours
    /// in the adjacent column (`use_up` → look left, else look right).
    /// Vertices with no neighbour on that side keep their current slot, so
    /// they drift with — rather than against — their surroundings.
    fn order_column_by(&mut self, col: usize, use_up: bool) {
        let pos = self.positions();
        let mut keyed: Vec<(f32, usize, usize)> = Vec::with_capacity(self.order[col].len());
        for (idx, &v) in self.order[col].iter().enumerate() {
            let edges = if use_up {
                &self.up_edges[v]
            } else {
                &self.down_edges[v]
            };
            let mut np: Vec<f32> = edges.iter().map(|&(u, _, _)| pos[u] as f32).collect();
            let key = if np.is_empty() {
                idx as f32
            } else {
                layout_median(&mut np)
            };
            keyed.push((key, v, idx));
        }
        keyed.sort_by(|a, b| {
            a.0.partial_cmp(&b.0)
                .unwrap_or(core::cmp::Ordering::Equal)
                .then(a.2.cmp(&b.2))
        });
        self.order[col] = keyed.into_iter().map(|(_, v, _)| v).collect();
    }

    /// Alternating up/down median sweeps; keep the best-scoring ordering seen.
    fn minimise_crossings(&mut self) {
        let mut best = self.order.clone();
        let mut best_cross = self.count_crossings();
        for it in 0..LAYOUT_ORDER_ITERS {
            if it % 2 == 0 {
                for c in 1..self.num_cols {
                    self.order_column_by(c, true);
                }
            } else {
                for c in (0..self.num_cols.saturating_sub(1)).rev() {
                    self.order_column_by(c, false);
                }
            }
            let cross = self.count_crossings();
            if cross < best_cross {
                best_cross = cross;
                best = self.order.clone();
                if cross == 0 {
                    break;
                }
            }
        }
        self.order = best;
    }

    /// Assign a top-edge y to every vertex. Starts by stacking each column,
    /// then runs alternating passes that pull each vertex toward the median
    /// height of the ports it connects to (resolving overlaps after each), so
    /// wires straighten. Returns y per lvid in an un-shifted frame.
    fn assign_y(&self) -> Vec<f32> {
        let mut y = vec![0.0f32; self.column.len()];
        for col in &self.order {
            let mut yy = 0.0;
            for &v in col {
                y[v] = yy;
                yy += self.height[v] + LAYOUT_VGAP;
            }
        }
        for pass in 0..LAYOUT_COORD_ITERS {
            let forward = pass % 2 == 0;
            let cols: Vec<usize> = if forward {
                (1..self.num_cols).collect()
            } else {
                (0..self.num_cols.saturating_sub(1)).rev().collect()
            };
            for c in cols {
                let col = &self.order[c];
                let mut desired: Vec<f32> = Vec::with_capacity(col.len());
                for &v in col {
                    let edges = if forward {
                        &self.up_edges[v]
                    } else {
                        &self.down_edges[v]
                    };
                    if edges.is_empty() {
                        desired.push(y[v]);
                    } else {
                        // Top-of-`v` that lines its port up with the neighbour's
                        // port. Forward: neighbour `u` is left, its exit port at
                        // y[u]+up_off, v's entry port at top+down_off. Backward:
                        // neighbour is right, entry at y[u]+down_off, v's exit at
                        // top+up_off.
                        let mut targets: Vec<f32> = edges
                            .iter()
                            .map(|&(u, up_off, down_off)| {
                                if forward {
                                    y[u] + up_off - down_off
                                } else {
                                    y[u] + down_off - up_off
                                }
                            })
                            .collect();
                        desired.push(layout_median(&mut targets));
                    }
                }
                layout_resolve_overlaps(col, &self.height, &mut desired, LAYOUT_VGAP);
                for (i, &v) in col.iter().enumerate() {
                    y[v] = desired[i];
                }
            }
        }
        y
    }
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
    /// Scrubbing a numeric param on a node's face. Cumulative pixel delta
    /// from `press_origin_x` maps to a value delta over
    /// `PARAM_SCRUB_FULL_RANGE_PX`, anchored on `start_value` so a long
    /// drag doesn't accumulate float error. Emits `SetGraphNodeParam` each
    /// pointer move, matching the inspector sidebar.
    ParamScrub {
        node_id: u32,
        param_name: String,
        range: (f32, f32),
        start_value: f32,
        is_int: bool,
        press_origin_x: f32,
    },
    /// Rubber-band selection from a Shift+empty-canvas press. `origin_screen`
    /// is the press point; the live rect spans it to the current cursor. On
    /// release, the nodes the box intersects become the selection (replace).
    Marquee { origin_screen: (f32, f32) },
}

impl DragMode {
    fn is_pan(&self) -> bool {
        matches!(self, DragMode::Pan)
    }

    /// Short tag for the debug overlay readout.
    fn debug_label(&self) -> &'static str {
        match self {
            DragMode::None => "none",
            DragMode::Pan => "pan",
            DragMode::WireFrom { .. } => "wire",
            DragMode::NodeMove { .. } => "node-move",
            DragMode::ParamScrub { .. } => "param-scrub",
            DragMode::Marquee { .. } => "marquee",
        }
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
    /// Selected node ids at the current scope level. A set so the user can
    /// rubber-band or Shift-click several nodes before collapsing them into a
    /// group. A plain click selects exactly one; Shift toggles.
    selected: ahash::AHashSet<u32>,
    /// `instance.graph.is_some()` for the watched effect. Drives the
    /// "Reset to Default" affordance in the header — only shown when
    /// the user has diverged from the bundled preset.
    has_graph_mod: bool,
    /// Actions accumulated this frame from canvas interactions.
    /// Drained by the editor window's input loop after each event.
    pending_actions: Vec<PanelAction>,
    /// Per-node collapse state (UI-only, keyed by runtime node id so it
    /// survives snapshot rebuilds like positions do). A collapsed node
    /// hides its on-face param rows but keeps its header and ports, so it
    /// can still be wired. Absent = expanded.
    collapsed: ahash::AHashMap<u32, bool>,
    /// In-place mapping editor for a card binding, anchored on the param
    /// row it was right-clicked from. Surface-agnostic widget; the canvas
    /// just hosts it, draws it on top of the nodes, and forwards pointer
    /// events to it while it's open. Closed by default.
    mapping_popover: MappingPopover,
    /// Wall-clock seconds at the last left-press, used to detect a
    /// double-click — on empty space (opens the node picker) or on a group
    /// node (descends into it). `None` until the first press, and reset to
    /// `None` after a double-click fires so a third press starts a fresh
    /// single-click rather than re-triggering.
    last_click_time: Option<f32>,
    /// Screen-space cursor at the last left-press. Paired with
    /// `last_click_time` so a double-click only registers when the two
    /// presses land within a few pixels of each other.
    last_click_pos: (f32, f32),
    /// Node id under the last left-press (`None` for empty space). A
    /// double-click only counts when both presses land on the *same* target,
    /// so dragging between two groups doesn't accidentally enter one.
    last_click_node: Option<u32>,
    /// Current view scope — a path of group node ids from the document root
    /// to the level being shown. Empty = root. Pushed on enter-group, popped
    /// on exit. The canvas re-resolves which level to render from the live
    /// snapshot each frame using this path, so navigation is purely UI-local
    /// (no command, no content round-trip).
    scope: Vec<u32>,
    /// Display titles of the groups in `scope`, captured at enter time (the
    /// ancestor group nodes aren't in the current level's views, so their
    /// names have to be remembered). Always the same length as `scope`; the
    /// breadcrumb bar reads `["Root", scope_titles…]`.
    scope_titles: Vec<String>,
    /// When true, draw the debug overlay (scope path, selection, hover, drag
    /// mode) in the canvas corner. Toggled by the backtick key. The handoff
    /// doc's mandate: let the canvas tell Peter what it thinks is happening
    /// without a debugger.
    debug_overlay: bool,
    /// Set when the view descends into a group; consumed by the next
    /// `set_snapshot`, which auto-formats the level *only if it has never been
    /// laid out* (every node's `editor_pos` is `None`). Preserves any manual
    /// arrangement — once a layout exists (hand-moved or a prior auto-format),
    /// this never fires for that group again.
    format_on_enter: bool,
    /// Per-node recent history (normalized 0..1) of the node's primary numeric
    /// param, keyed by stable `NodeId`. Pushed each frame by
    /// [`Self::apply_live_values`] from the live tap and drawn as a small
    /// sparkline on the collapsed node face, so a modulated knob reads as a
    /// moving trace — the design's "even the invisible math nodes become
    /// legible." Bounded to [`SPARK_CAPACITY`] points per node; pruned to the
    /// live node set on a topology rebuild. Empty when no editor is watching.
    spark_history: ahash::AHashMap<manifold_core::NodeId, std::collections::VecDeque<f32>>,
    /// Runtime id of a node the canvas should centre + select once it is laid
    /// out at the current scope — set by [`Self::focus_node`] (jump-to-node from
    /// a card param) and consumed by [`Self::resolve_pending_focus`] one frame
    /// later, after `set_snapshot` has rebuilt the (possibly newly-entered)
    /// level so the node's position is known. `None` when nothing is pending.
    pending_focus: Option<u32>,
    /// Lowercased find-a-node query. Non-empty dims nodes whose title/handle
    /// doesn't contain it and brightens the matches, so a name jumps out of a
    /// busy graph. Empty = no search active. Set live by the editor's search box.
    node_search: String,
}

/// Max seconds between two empty-canvas presses for them to count as a
/// double-click. Matches the typical OS double-click window.
const DOUBLE_CLICK_SECONDS: f32 = 0.3;
/// Max screen-space distance (px) between the two presses of a double-click.
/// A drag further than this is two separate single-clicks, not a double.
const DOUBLE_CLICK_RADIUS_PX: f32 = 4.0;
/// A left-press that moves less than this on release counts as a click, not a
/// drag — used to tell a pan from a deselecting click, and a marquee from a
/// stray shift-click.
const CLICK_MOVE_SLOP_PX: f32 = 4.0;

/// Points retained in each node's sparkline history. At the editor's ~UI frame
/// rate this is roughly a second of trace — enough to read a slow LFO without
/// hoarding memory across a big graph.
const SPARK_CAPACITY: usize = 48;

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
            selected: ahash::AHashSet::new(),
            has_graph_mod: false,
            pending_actions: Vec::new(),
            collapsed: ahash::AHashMap::new(),
            mapping_popover: MappingPopover::new(),
            last_click_time: None,
            last_click_pos: (0.0, 0.0),
            last_click_node: None,
            scope: Vec::new(),
            scope_titles: Vec::new(),
            format_on_enter: false,
            spark_history: ahash::AHashMap::new(),
            pending_focus: None,
            node_search: String::new(),
            debug_overlay: false,
        }
    }

    /// Tell the canvas whether the watched effect is currently on its
    /// bundled-preset default (`false`) or carries a per-card graph
    /// override (`true`). When `true`, the header surfaces a
    /// "Reset to Default" button. Called once per frame by the editor
    /// window's present path.
    pub fn set_has_graph_mod(&mut self, has_mod: bool) {
        self.has_graph_mod = has_mod;
    }

    /// Drain editor actions queued by canvas interactions — including the
    /// mapping popover's `EffectMapping*` edits, so the app's existing
    /// dispatch (which routes them to `EditUserParamBindingCommand`) sees
    /// them on the same pass as canvas actions. Called once per input
    /// event by the editor window's present path.
    pub fn drain_actions(&mut self) -> Vec<PanelAction> {
        let mut actions = std::mem::take(&mut self.pending_actions);
        actions.extend(self.mapping_popover.drain_actions());
        actions
    }

    /// Emit a `RemoveGraphNode` action for every currently-selected node.
    /// Wired to the Delete/Backspace key handler on the editor window. Clears
    /// the selection on emit so the next frame doesn't double-fire. Multiple
    /// selected nodes each emit one action (and one undo entry apiece).
    pub fn request_delete_selected(&mut self) {
        for id in std::mem::take(&mut self.selected) {
            self.pending_actions
                .push(PanelAction::RemoveGraphNode { node_id: id });
        }
    }

    /// Emit a `GroupSelection` action collapsing the current selection into a
    /// new group at this scope level. Wired to Ctrl+G. No-op on an empty
    /// selection. The new group's handle is auto-named (`group_N`) and made
    /// unique among the level's existing handles so flatten-time prefixing
    /// can't collide. The content thread validates the rest (boundary nodes,
    /// connectivity); a rejected group simply doesn't change the def.
    pub fn request_group_selection(&mut self) {
        let node_ids = self.selected_ids();
        if node_ids.is_empty() {
            return;
        }
        let existing: ahash::AHashSet<&str> =
            self.nodes.iter().filter_map(|n| n.handle.as_deref()).collect();
        let mut i = 1u32;
        let mut handle = format!("group_{i}");
        while existing.contains(handle.as_str()) {
            i += 1;
            handle = format!("group_{i}");
        }
        group_log!(
            "GroupSelection scope={:?} ids={node_ids:?} -> {handle:?}",
            self.scope
        );
        self.pending_actions.push(PanelAction::GroupSelection {
            scope_path: self.scope.clone(),
            node_ids,
            handle,
            centroid: self.selection_centroid(),
        });
    }

    /// Emit an `Ungroup` action dissolving the selected group back into this
    /// level. Wired to Ctrl+Shift+G. No-op unless exactly one group node is
    /// selected.
    pub fn request_ungroup(&mut self) {
        let Some(group_id) = self.single_selected_group() else {
            return;
        };
        group_log!("Ungroup scope={:?} group={group_id}", self.scope);
        self.pending_actions.push(PanelAction::Ungroup {
            scope_path: self.scope.clone(),
            group_id,
        });
    }

    /// Cycle the selected group's accent colour to the next preset tint, for
    /// legibility (Resolume / TouchDesigner colour-coding). No-op unless exactly
    /// one group node is selected. Emits `SetGroupTint`; the next colour is the
    /// one after the group's current tint in [`GROUP_TINT_PALETTE`] (or the
    /// first, when it's untinted or off-palette).
    pub fn request_cycle_group_tint(&mut self) {
        let Some(group_id) = self.single_selected_group() else {
            return;
        };
        let current = self
            .nodes
            .iter()
            .find(|n| n.id == group_id)
            .and_then(|n| n.group_tint);
        let next_idx = current
            .and_then(|c| GROUP_TINT_PALETTE.iter().position(|p| *p == c))
            .map(|i| (i + 1) % GROUP_TINT_PALETTE.len())
            .unwrap_or(0);
        group_log!("CycleGroupTint group={group_id} -> palette[{next_idx}]");
        self.pending_actions.push(PanelAction::SetGroupTint {
            scope_path: self.scope.clone(),
            group_id,
            tint: Some(GROUP_TINT_PALETTE[next_idx]),
        });
    }

    /// Set the find-a-node query (stored lowercased). Empty clears the search,
    /// restoring every node to full brightness.
    pub fn set_node_search(&mut self, query: &str) {
        self.node_search = query.to_ascii_lowercase();
    }

    /// The active find-a-node query (lowercased), or empty when no search is
    /// running. Lets the editor re-seed the field when reopening the search box.
    pub fn node_search(&self) -> &str {
        &self.node_search
    }

    /// Visible previewable nodes as `(capture node_id, strip_x, strip_y,
    /// strip_w, strip_h)` in screen space — the 16:9 preview-strip region the
    /// present pass blits each node's atlas thumbnail into. The `node_id` is the
    /// *capture* id ([`NodeView::preview_node_id`]): a node's own id, or for a
    /// group the inner node producing its output — so groups preview too,
    /// reusing the producer's existing atlas cell. Nodes with no image output
    /// emit nothing. Culls off-canvas nodes.
    pub fn visible_node_thumbnails(
        &self,
        viewport: Rect,
    ) -> Vec<(manifold_core::NodeId, f32, f32, f32, f32)> {
        let mut out = Vec::new();
        let header = NODE_HEADER_HEIGHT * self.zoom;
        let pad = PREVIEW_PAD * self.zoom;
        for node in &self.nodes {
            let Some(capture_id) = node.preview_node_id.clone() else {
                continue;
            };
            let (sx, sy) = self.to_screen(viewport, node.pos_graph.0, node.pos_graph.1);
            let sw = NODE_WIDTH * self.zoom;
            let sh = node.height() * self.zoom;
            if sx + sw < viewport.x
                || sx > viewport.x + viewport.w
                || sy + sh < viewport.y
                || sy > viewport.y + viewport.h
            {
                continue;
            }
            let strip_w = PREVIEW_IMG_W * self.zoom;
            let strip_h = PREVIEW_IMG_H * self.zoom;
            if strip_h > 1.0 {
                out.push((capture_id, sx + pad, sy + header + pad, strip_w, strip_h));
            }
        }
        out
    }

    /// Whether `node` matches the active search — its title or handle contains
    /// the query. Always true when no search is active.
    fn node_matches_search(&self, node: &NodeView) -> bool {
        if self.node_search.is_empty() {
            return true;
        }
        node.title.to_ascii_lowercase().contains(&self.node_search)
            || node
                .handle
                .as_deref()
                .is_some_and(|h| h.to_ascii_lowercase().contains(&self.node_search))
    }

    /// For a single selected group: its id, current display name, and the
    /// screen-space rect of its header (where the rename field anchors). `None`
    /// unless exactly one group node is selected. Drives F2-to-rename.
    pub fn group_rename_target(&self, viewport: Rect) -> Option<(u32, String, f32, f32, f32, f32)> {
        let gid = self.single_selected_group()?;
        let node = self.nodes.iter().find(|n| n.id == gid)?;
        let (sx, sy) = self.to_screen(viewport, node.pos_graph.0, node.pos_graph.1);
        Some((
            gid,
            node.title.clone(),
            sx,
            sy,
            NODE_WIDTH * self.zoom,
            NODE_HEADER_HEIGHT * self.zoom,
        ))
    }

    /// Re-run the layered auto-layout over the current level and emit a single
    /// undoable `RelayoutGraph` action carrying every node's new position.
    /// Wired to Cmd+L. Writes positions optimistically so the canvas updates
    /// immediately; the command persists them to `editor_pos`. No-op on an
    /// empty level.
    pub fn request_relayout(&mut self) {
        if self.nodes.is_empty() {
            return;
        }
        self.auto_layout();
        let positions: Vec<(u32, (f32, f32))> =
            self.nodes.iter().map(|n| (n.id, n.pos_graph)).collect();
        self.pending_actions.push(PanelAction::RelayoutGraph {
            scope_path: self.scope.clone(),
            positions,
        });
    }

    /// Push the latest snapshot. Rebuilds nodes+wires; recomputes
    /// auto-layout only when topology changed.
    pub fn set_snapshot(&mut self, snap: &GraphSnapshot) {
        // Resolve which level the current scope addresses. If the path no
        // longer resolves — the group was deleted, ungrouped, or an undo
        // pulled it out from under us — drop back to the document root.
        if !self.scope.is_empty() && resolve_level(snap, &self.scope).is_none() {
            group_log!("scope {:?} no longer resolves — returning to root", self.scope);
            self.scope.clear();
            self.scope_titles.clear();
        }
        let (level_nodes, level_wires) =
            resolve_level(snap, &self.scope).unwrap_or((&snap.nodes, &snap.wires));

        // Hash the resolved level (not the whole snapshot) plus the scope, so
        // entering or leaving a group re-runs layout even though the
        // underlying snapshot is byte-for-byte the same document.
        let new_hash = hash_level(&self.scope, level_nodes, level_wires);
        if new_hash == self.topology_hash && !self.nodes.is_empty() {
            // Topology unchanged — keep the existing layout, but refresh
            // each node's on-face param values in place. They show live
            // values now, so a param-only change (a driver moving a knob,
            // an inspector edit) must update them without re-running
            // auto-layout.
            for node in &mut self.nodes {
                if let Some(sn) = level_nodes.iter().find(|s| s.id == node.id) {
                    // Param tooltips are static per (type_id, name); carry the
                    // already-resolved ones forward by index rather than
                    // re-scanning the doc inventory on this per-frame path.
                    let prev_tips: Vec<Option<String>> =
                        node.params.iter().map(|p| p.tooltip.clone()).collect();
                    node.params = sn
                        .parameters
                        .iter()
                        .enumerate()
                        .map(|(i, p)| {
                            let mut pv = format_param_for_node(p);
                            pv.tooltip = prev_tips.get(i).cloned().flatten();
                            pv
                        })
                        .collect();
                    node.summary = node_summary(&sn.parameters);
                }
            }
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

        let new_nodes: Vec<NodeView> = level_nodes
            .iter()
            .map(|n| NodeView {
                id: n.id,
                node_id: n.node_id.clone(),
                handle: n.node_handle.clone(),
                title: n.title.clone(),
                params: n
                    .parameters
                    .iter()
                    .map(|p| {
                        let mut pv = format_param_for_node(p);
                        pv.tooltip =
                            manifold_renderer::node_graph::tooltip_for(&n.type_id, &p.name)
                                .map(str::to_owned);
                        pv
                    })
                    .collect(),
                summary: node_summary(&n.parameters),
                collapsed: self.collapsed.get(&n.id).copied().unwrap_or(true),
                header_color: category_header_color(
                    manifold_renderer::node_graph::descriptor_for(&n.type_id)
                        .map(|d| d.category)
                        .unwrap_or(manifold_renderer::node_graph::Category::Uncategorized),
                ),
                pos_graph: prev_positions
                    .get(&n.id)
                    .copied()
                    .unwrap_or((f32::NAN, f32::NAN)),
                inputs: n
                    .inputs
                    .iter()
                    .map(|p| PortView::from_kind(p.name.clone(), &p.kind))
                    .collect(),
                outputs: n
                    .outputs
                    .iter()
                    .map(|p| PortView::from_kind(p.name.clone(), &p.kind))
                    .collect(),
                breaks_dependency_cycle: n.breaks_dependency_cycle,
                is_group: n.type_id == GROUP_TYPE_ID,
                group_tint: n.group.as_ref().and_then(|g| g.tint),
                tooltip: manifold_renderer::node_graph::descriptor_for(&n.type_id)
                    .map(|d| d.summary)
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned),
                preview_node_id: node_preview_target(n),
            })
            .collect();
        self.nodes = new_nodes;
        self.wires = level_wires
            .iter()
            .map(|w| WireView {
                from_node: w.from_node,
                from_port: w.from_port.clone(),
                to_node: w.to_node,
                to_port: w.to_port.clone(),
            })
            .collect();

        // Drop sparkline histories for nodes that left this level (group
        // navigation, delete, ungroup) so the map can't grow unbounded across a
        // long authoring session. Param-only edits keep the same topology hash
        // and take the early-return path above, so traces survive a knob tweak.
        if !self.spark_history.is_empty() {
            self.spark_history
                .retain(|id, _| self.nodes.iter().any(|n| &n.node_id == id));
        }

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
        for (view, snap_node) in self.nodes.iter_mut().zip(level_nodes.iter()) {
            if let Some(p) = snap_node.editor_pos {
                view.pos_graph = p;
            }
        }

        // Auto-format on first entry into a never-laid-out group. `auto_layout`
        // above already positioned the nodes (all were unplaced); persist those
        // positions so the tidy layout sticks. Only fires when EVERY node lacks
        // a saved `editor_pos`, so a manual arrangement is never overwritten.
        // RelayoutGraph routes through the non-structural layout command, so it
        // doesn't rebuild the chain or reset state.
        if std::mem::take(&mut self.format_on_enter)
            && !self.nodes.is_empty()
            && level_nodes.iter().all(|n| n.editor_pos.is_none())
        {
            let positions: Vec<(u32, (f32, f32))> =
                self.nodes.iter().map(|n| (n.id, n.pos_graph)).collect();
            self.pending_actions.push(PanelAction::RelayoutGraph {
                scope_path: self.scope.clone(),
                positions,
            });
        }
    }

    /// Overlay this frame's live (post-modulation) param values onto the node
    /// faces. The structural snapshot ([`Self::set_snapshot`]) only rebuilds on
    /// a `graph_version` bump, so a driver / Ableton / envelope / card slider
    /// moving a knob never reached the canvas — this closes that gap by
    /// refreshing each on-face value string, fill bar, and scrub anchor every
    /// frame from `ContentState::live_node_params`, matched by stable `NodeId`.
    ///
    /// Only continuous-numeric and bool params are touched (enums, triggers, and
    /// multi-component params keep their snapshot display, which an edit already
    /// refreshes via `graph_version`). The param the user is actively scrubbing
    /// is skipped so the live feed never fights a drag. No-op when `live` is
    /// empty (no editor watching), so the closed-editor path pays nothing.
    pub fn apply_live_values(&mut self, live: &manifold_renderer::node_graph::LiveNodeParams) {
        if live.is_empty() {
            return;
        }
        let by_id: ahash::AHashMap<&manifold_core::NodeId, &Vec<(&'static str, f32)>> =
            live.iter().map(|(id, vals)| (id, vals)).collect();
        // The param the user is mid-scrub on stays the source of truth until
        // release; cloned so the per-node mutable walk below has no live borrow
        // of `self.drag_mode`.
        let scrubbing: Option<(u32, String)> = match &self.drag_mode {
            DragMode::ParamScrub {
                node_id,
                param_name,
                ..
            } => Some((*node_id, param_name.clone())),
            _ => None,
        };
        // Sparkline samples gathered during the mutable node walk, applied to
        // `spark_history` afterwards (can't touch `self.spark_history` while
        // `self.nodes` is borrowed mutably).
        let mut spark_updates: Vec<(manifold_core::NodeId, f32)> = Vec::new();
        for node in &mut self.nodes {
            if node.node_id.is_empty() {
                continue;
            }
            let Some(vals) = by_id.get(&node.node_id) else {
                continue;
            };
            let node_id = node.id;
            // The node's first ranged numeric param drives its sparkline — the
            // same "primary" pick the collapsed summary uses, so the trace and
            // the summary read the same knob.
            let mut primary_fill: Option<f32> = None;
            for pv in &mut node.params {
                if scrubbing
                    .as_ref()
                    .is_some_and(|(sn, sp)| *sn == node_id && sp.as_str() == pv.name)
                {
                    continue;
                }
                let Some(&(_, value)) = vals.iter().find(|(name, _)| *name == pv.name) else {
                    continue;
                };
                let Some((value_str, fill)) = numeric_value_fill(pv.kind, value, pv.range) else {
                    continue;
                };
                pv.value = value_str;
                pv.fill = fill;
                if let Some(scrub) = pv.scrub.as_mut() {
                    scrub.current_value = value;
                }
                if primary_fill.is_none()
                    && let Some(f) = fill
                {
                    primary_fill = Some(f);
                }
            }
            if let Some(f) = primary_fill {
                spark_updates.push((node.node_id.clone(), f));
            }
        }
        for (id, f) in spark_updates {
            let hist = self.spark_history.entry(id).or_default();
            if hist.len() >= SPARK_CAPACITY {
                hist.pop_front();
            }
            hist.push_back(f);
        }
    }

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
        snap: &manifold_renderer::node_graph::GraphSnapshot,
        node_id: &manifold_core::NodeId,
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

    // ── Group navigation (scope) ────────────────────────────────────

    /// The current view scope as a path of group node ids (empty = root).
    /// Read by the app to scope graph edits (group/ungroup and per-node
    /// mutations) to the level the canvas is showing.
    pub fn scope_path(&self) -> &[u32] {
        &self.scope
    }

    /// Descend into a group node, showing its body as the canvas level. The
    /// next `set_snapshot` re-resolves and re-lays-out at the new level.
    /// No-op if the id isn't a group in the current view. Clears selection so
    /// a stale id from the parent level can't linger.
    fn enter_group(&mut self, group_id: u32) {
        let Some(node) = self.nodes.iter().find(|n| n.id == group_id) else {
            return;
        };
        if !node.is_group {
            return;
        }
        let title = node.title.clone();
        group_log!(
            "enter group {group_id} ({title:?}): scope {:?} -> depth {}",
            self.scope,
            self.scope.len() + 1
        );
        self.selected.clear();
        self.scope.push(group_id);
        self.scope_titles.push(title);
        // Auto-format this group the first time we open it (handled in the next
        // set_snapshot, and only if it has no saved layout).
        self.format_on_enter = true;
    }

    /// Pop one level back toward the root. Returns `true` if it moved (there
    /// was a level to leave), so the caller can mark the editor dirty. Clears
    /// selection for the same reason as `enter_group`.
    pub fn exit_group(&mut self) -> bool {
        if let Some(left) = self.scope.pop() {
            self.scope_titles.pop();
            group_log!("exit group {left}: scope now {:?}", self.scope);
            self.selected.clear();
            true
        } else {
            false
        }
    }

    /// Jump directly to a breadcrumb depth (0 = root, 1 = first group, …),
    /// truncating the scope path. Used by breadcrumb-bar clicks. No-op if the
    /// depth is already current or out of range.
    pub fn set_scope_depth(&mut self, depth: usize) {
        if depth < self.scope.len() {
            group_log!("breadcrumb jump to depth {depth}: {:?}", self.scope);
            self.scope.truncate(depth);
            self.scope_titles.truncate(depth);
            self.selected.clear();
        }
    }

    /// Toggle the debug overlay (scope/selection/hover/drag readout). Wired to
    /// the backtick key in the editor window.
    pub fn toggle_debug_overlay(&mut self) {
        self.debug_overlay = !self.debug_overlay;
        group_log!("debug overlay -> {}", self.debug_overlay);
    }

    /// Lay out the breadcrumb segments in the canvas header, left to right:
    /// `[Root › title0 › title1 …]`. Returns `(target_depth, rect, label,
    /// is_current)` per segment. Empty at the document root (no breadcrumb
    /// drawn). Shared by render and hit-test so the click zones match the
    /// glyphs.
    fn breadcrumb_segments(&self, viewport: Rect) -> Vec<(usize, Rect, String, bool)> {
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
    fn breadcrumb_hit(&self, viewport: Rect, sx: f32, sy: f32) -> Option<usize> {
        self.breadcrumb_segments(viewport)
            .into_iter()
            .find(|(_, r, _, _)| sx >= r.x && sx <= r.x + r.w && sy >= r.y && sy <= r.y + r.h)
            .map(|(depth, _, _, _)| depth)
    }

    /// Lay the graph out as left-to-right layers (the Sugiyama framework):
    /// assign every node a column by dependency depth, order each column to
    /// minimise wire crossings, then nudge nodes vertically so connected
    /// ports line up and wires run straight. See [`LayeredLayout`].
    fn auto_layout(&mut self) {
        let n = self.nodes.len();
        if n == 0 {
            return;
        }
        // Map node id → index in self.nodes for adjacency walks.
        let id_to_idx: ahash::AHashMap<u32, usize> = self
            .nodes
            .iter()
            .enumerate()
            .map(|(i, nv)| (nv.id, i))
            .collect();

        // Forward edges only. A wire terminating on a cycle-breaking node
        // (e.g. `node.feedback`) closes a per-frame feedback loop — `connect`
        // permits it and `topological_sort` ignores it, so layout must too,
        // else depth accumulates around the loop and consumers get pushed
        // thousands of pixels off-screen. Each surviving edge carries the
        // y-offset of its source output port and target input port so the
        // coordinate pass can line the two up rather than the boxes.
        struct FwdEdge {
            from: usize,
            to: usize,
            from_off: f32,
            to_off: f32,
        }
        let mut fwd: Vec<FwdEdge> = Vec::with_capacity(self.wires.len());
        for w in &self.wires {
            let (Some(&from), Some(&to)) =
                (id_to_idx.get(&w.from_node), id_to_idx.get(&w.to_node))
            else {
                continue;
            };
            if self.nodes[to].breaks_dependency_cycle {
                continue;
            }
            fwd.push(FwdEdge {
                from,
                to,
                from_off: self.nodes[from].output_port_offset(&w.from_port),
                to_off: self.nodes[to].input_port_offset(&w.to_port),
            });
        }

        // Phase 1 — layer assignment by longest path. With back-edges removed
        // the graph is a DAG, so this converges in ≤ n passes; cap at n+1 as a
        // safety net.
        let mut depth = vec![0i32; n];
        for _ in 0..=n {
            let mut changed = false;
            for e in &fwd {
                let candidate = depth[e.from] + 1;
                if candidate > depth[e.to] {
                    depth[e.to] = candidate;
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        let num_cols = (depth.iter().copied().max().unwrap_or(0) as usize) + 1;

        // Phase 2 — build layout vertices. Real nodes carry their column and
        // height; each edge spanning more than one column gets a chain of
        // virtual waypoints so it participates in ordering and alignment.
        let mut column: Vec<usize> = (0..n).map(|i| depth[i] as usize).collect();
        let mut height: Vec<f32> = self.nodes.iter().map(|nv| nv.height()).collect();
        let mut up_edges: Vec<Vec<(usize, f32, f32)>> = vec![Vec::new(); n];
        let mut down_edges: Vec<Vec<(usize, f32, f32)>> = vec![Vec::new(); n];
        for e in &fwd {
            let (c0, c1) = (column[e.from], column[e.to]);
            // c1 >= c0 + 1 is guaranteed by longest-path layering.
            if c1 == c0 + 1 {
                down_edges[e.from].push((e.to, e.from_off, e.to_off));
                up_edges[e.to].push((e.from, e.from_off, e.to_off));
                continue;
            }
            let mut prev = e.from;
            let mut prev_off = e.from_off;
            for c in (c0 + 1)..c1 {
                let d = column.len();
                column.push(c);
                height.push(LAYOUT_DUMMY_H);
                up_edges.push(Vec::new());
                down_edges.push(Vec::new());
                let mid = LAYOUT_DUMMY_H * 0.5;
                down_edges[prev].push((d, prev_off, mid));
                up_edges[d].push((prev, prev_off, mid));
                prev = d;
                prev_off = mid;
            }
            down_edges[prev].push((e.to, prev_off, e.to_off));
            up_edges[e.to].push((prev, prev_off, e.to_off));
        }

        // Initial column ordering: real nodes by id (deterministic, no
        // twitch on rebuild), waypoints after them — the sweep fixes both.
        let mut order: Vec<Vec<usize>> = vec![Vec::new(); num_cols];
        for (lvid, &c) in column.iter().enumerate() {
            order[c].push(lvid);
        }
        for col in &mut order {
            col.sort_by_key(|&lvid| {
                if lvid < n {
                    (0u8, self.nodes[lvid].id)
                } else {
                    (1u8, (lvid - n) as u32)
                }
            });
        }

        let mut layout = LayeredLayout {
            num_cols,
            column,
            height,
            order,
            up_edges,
            down_edges,
        };
        layout.minimise_crossings();
        let y = layout.assign_y();

        // Shift so the topmost real node sits at the layout origin, then
        // write back. Waypoints are dropped — only real nodes have a position.
        let min_y = y.iter().take(n).copied().fold(f32::INFINITY, f32::min);
        let y_shift = if min_y.is_finite() {
            LAYOUT_ORIGIN.1 - min_y
        } else {
            0.0
        };
        for (i, node) in self.nodes.iter_mut().enumerate() {
            let x = LAYOUT_ORIGIN.0 + layout.column[i] as f32 * COL_SPACING;
            node.pos_graph = (x, y[i] + y_shift);
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

    /// Hit-test which on-node param row (if any) is under the cursor,
    /// returning `(node_id, param_index)`. Works in screen space to match
    /// `draw_node`'s row layout exactly. Skips collapsed and param-less
    /// nodes, and walks topmost-first so overlapping nodes resolve like the
    /// draw order.
    fn param_row_under(&self, viewport: Rect, sx: f32, sy: f32) -> Option<(u32, usize)> {
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

    /// Screen-space rect of one on-node param row, by `(node_id,
    /// param_index)`. Mirrors `param_row_under`'s layout exactly so an
    /// anchored popover lines up with the row it was opened from. `None`
    /// for a missing node / out-of-range index.
    fn param_row_rect(&self, viewport: Rect, node_id: u32, pi: usize) -> Option<Rect> {
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

    /// The inner-param name of one on-node param row, by `(node_id,
    /// param_index)`. The app joins this with the snapshot's
    /// `node_handle` to look up the matching `UserParamBinding`.
    pub fn param_name_at(&self, node_id: u32, pi: usize) -> Option<String> {
        self.find_node(node_id)
            .and_then(|n| n.params.get(pi))
            .map(|p| p.name.clone())
    }

    /// Right-button press on the canvas. If it lands on an expanded
    /// param row, returns `(node_id, param_index)` so the app can resolve
    /// whether that inner param is exposed as a card binding and, if so,
    /// open the mapping popover via `open_mapping_popover`. Returns `None`
    /// for clicks that miss every param row (the app then leaves the
    /// canvas alone). A right-click anywhere first dismisses an open
    /// popover.
    pub fn on_right_button_down(&mut self, viewport: Rect, sx: f32, sy: f32) -> Option<(u32, usize)> {
        // A right-click outside the open popover dismisses it (and is
        // otherwise treated as a fresh hit-test).
        if self.mapping_popover.is_open() && !self.mapping_popover.contains_point(sx, sy) {
            self.mapping_popover.close();
        }
        self.param_row_under(viewport, sx, sy)
    }

    /// Open the mapping popover for a resolved binding, anchored on its
    /// param row. Called by the app after `on_right_button_down` reports
    /// a row AND the app has confirmed that row's inner param is exposed
    /// as a `UserParamBinding` (passing its current mapping in here). The
    /// canvas owns the anchor geometry; the app owns the binding lookup.
    #[allow(clippy::too_many_arguments)]
    #[allow(clippy::too_many_arguments)]
    pub fn open_mapping_popover(
        &mut self,
        viewport: Rect,
        node_id: u32,
        pi: usize,
        binding_id: String,
        label: String,
        min: f32,
        max: f32,
        invert: bool,
        curve: manifold_core::macro_bank::MacroCurve,
        scale: f32,
        offset: f32,
        range: Option<(f32, f32)>,
    ) {
        let Some(anchor) = self.param_row_rect(viewport, node_id, pi) else {
            return;
        };
        // Clip the popover to the canvas body (below the header strip).
        let clip = Rect::new(
            viewport.x,
            viewport.y + HEADER_HEIGHT,
            viewport.w,
            (viewport.h - HEADER_HEIGHT).max(0.0),
        );
        self.mapping_popover.open(
            binding_id, label, min, max, invert, curve, scale, offset, range, anchor, clip,
        );
    }

    /// Forward a left-button press to the open popover. Returns `true`
    /// when the popover consumed it (a handle/button hit, or any click
    /// inside the panel). A press outside the panel returns `false` and
    /// closes the popover, so the host can fall through to the normal
    /// canvas left-click path.
    pub fn popover_on_left_press(&mut self, sx: f32, sy: f32) -> bool {
        if !self.mapping_popover.is_open() {
            return false;
        }
        if self.mapping_popover.on_press(sx, sy) {
            true
        } else {
            self.mapping_popover.close();
            false
        }
    }

    /// Forward pointer motion to the open popover (drives the live range
    /// drag + handle hover). No-op when closed.
    pub fn popover_on_move(&mut self, sx: f32, sy: f32) {
        self.mapping_popover.on_move(sx, sy);
    }

    /// Forward a left-button release to the open popover (commits a range
    /// drag). No-op when closed.
    pub fn popover_on_left_release(&mut self) {
        self.mapping_popover.on_release();
    }

    /// `true` while the mapping popover is open. The host checks this so a
    /// left-click is routed to the popover first.
    pub fn popover_open(&self) -> bool {
        self.mapping_popover.is_open()
    }

    /// `true` while a popover value field is being typed into — the host routes
    /// keystrokes to it instead of firing canvas shortcuts.
    pub fn popover_is_editing(&self) -> bool {
        self.mapping_popover.is_editing()
    }

    /// Feed one typed character into the popover's active numeric field.
    pub fn popover_on_text_char(&mut self, c: char) {
        self.mapping_popover.on_text_char(c);
    }

    /// Delete the last typed character in the popover's active field.
    pub fn popover_on_backspace(&mut self) {
        self.mapping_popover.on_backspace();
    }

    /// Commit the popover's typed value (Enter).
    pub fn popover_commit_edit(&mut self) {
        self.mapping_popover.commit_edit();
    }

    /// Cancel the popover's numeric edit (Esc).
    pub fn popover_cancel_edit(&mut self) {
        self.mapping_popover.cancel_edit();
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
            DragMode::WireFrom { .. } | DragMode::Marquee { .. } => {
                // Cursor position is enough — render reads `self.cursor` for
                // both the ghost wire and the live marquee rect.
            }
            DragMode::ParamScrub {
                node_id,
                param_name,
                range,
                start_value,
                is_int,
                press_origin_x,
            } => {
                let node_id = *node_id;
                let param_name = param_name.clone();
                let (min, max) = *range;
                let start_value = *start_value;
                let is_int = *is_int;
                let press_origin_x = *press_origin_x;
                let span = (max - min).max(f32::EPSILON);
                let delta_px = sx - press_origin_x;
                let mut v =
                    (start_value + delta_px * (span / PARAM_SCRUB_FULL_RANGE_PX)).clamp(min, max);
                if is_int {
                    v = v.round();
                }
                self.pending_actions.push(PanelAction::SetGraphNodeParam {
                    node_id,
                    param_name,
                    new_value: manifold_core::effect_graph_def::SerializedParamValue::Float {
                        value: v,
                    },
                });
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

    /// Hit-test the collapse chevron in a node header (its right edge).
    /// Returns the node id when the cursor is over the chevron of a node
    /// that has params (param-less nodes draw no chevron). Checked before
    /// the header-drag test so toggling collapse doesn't also start a move.
    fn chevron_under(&self, viewport: Rect, sx: f32, sy: f32) -> Option<u32> {
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

    /// Left-mouse button down. Priority order:
    /// 1. "Reset to Default" header button (when graph is diverged).
    /// 2. Output port → start wire-drag.
    /// 3. Input port already wired → emit `DisconnectPorts` for the
    ///    incoming wire (one click breaks the connection).
    /// 4. Input port unwired → swallow (no action — wires only enter
    ///    inputs via drag-from-output).
    /// 5. Node header → start node-move drag.
    /// 6. Node body → select.
    /// 7. Empty canvas, double-click → open the node picker at the cursor.
    /// 8. Empty canvas, Shift+drag → rubber-band box select.
    /// 9. Empty canvas, plain drag → pan (trackpad-friendly); a click with no
    ///    drag clears the selection.
    ///
    /// `now` is a frame-monotonic wall-clock time in seconds, threaded in
    /// from the window event loop, used to distinguish a double-click on
    /// empty space from a pan-start single click. `shift` is the Shift
    /// modifier state: it makes node clicks toggle, and an empty-canvas drag
    /// a box-select instead of a pan.
    pub fn on_left_button_down(
        &mut self,
        viewport: Rect,
        sx: f32,
        sy: f32,
        now: f32,
        shift: bool,
    ) {
        // Breadcrumb bar (header chrome) — jump to a shallower scope. Gets
        // first crack like the reset button since it sits above the canvas
        // surface. No-op return value means the click wasn't on a crumb.
        if let Some(depth) = self.breadcrumb_hit(viewport, sx, sy) {
            self.set_scope_depth(depth);
            return;
        }
        // Header button has priority over everything else — it sits in
        // the chrome above the canvas surface.
        if self.has_graph_mod {
            let rect = self.reset_button_rect(viewport);
            if sx >= rect.x && sx <= rect.x + rect.w && sy >= rect.y && sy <= rect.y + rect.h {
                self.pending_actions.push(PanelAction::RevertEffectGraph);
                return;
            }
        }
        // Collapse chevron in a node header toggles that node's param rows.
        // Checked before ports/header so it doesn't start a wire or a move.
        if let Some(node_id) = self.chevron_under(viewport, sx, sy) {
            let now = !self.collapsed.get(&node_id).copied().unwrap_or(true);
            self.collapsed.insert(node_id, now);
            if let Some(node) = self.nodes.iter_mut().find(|n| n.id == node_id) {
                node.collapsed = now;
            }
            return;
        }
        if let Some(hit) = self.port_under(viewport, sx, sy) {
            if hit.is_output {
                self.drag_mode = DragMode::WireFrom {
                    from_node: hit.node_id,
                    from_port: hit.port_name,
                };
                return;
            }
            // Input port — if a wire feeds this port, breaking it on
            // click. Otherwise swallow so the click doesn't start a pan.
            if self.wire_into(hit.node_id, &hit.port_name).is_some() {
                self.pending_actions.push(PanelAction::DisconnectPorts {
                    to_node: hit.node_id,
                    to_port: hit.port_name,
                });
            }
            return;
        }
        // Param row on the node face → start a value scrub for numeric
        // params with a range; for non-scrubbable params just select the
        // node so the inspector sidebar can edit them.
        if let Some((node_id, pi)) = self.param_row_under(viewport, sx, sy) {
            let info = self
                .nodes
                .iter()
                .find(|n| n.id == node_id)
                .and_then(|n| n.params.get(pi).map(|p| (p.name.clone(), p.scrub)));
            if let Some((param_name, scrub)) = info {
                self.select_single(node_id);
                if let Some(s) = scrub {
                    self.drag_mode = DragMode::ParamScrub {
                        node_id,
                        param_name,
                        range: s.range,
                        start_value: s.current_value,
                        is_int: s.is_int,
                        press_origin_x: sx,
                    };
                }
                return;
            }
        }
        // Double-click on a group node descends into it. Checked before the
        // header-drag path so entering doesn't also start a move; a single
        // click on a group falls through to select / header-drag below.
        if let Some(node_id) = self.node_under(viewport, sx, sy) {
            let is_group = self
                .nodes
                .iter()
                .find(|n| n.id == node_id)
                .is_some_and(|n| n.is_group);
            if is_group {
                let dbl = self.is_double_click(sx, sy, now, Some(node_id));
                self.note_click(sx, sy, now, Some(node_id));
                if dbl {
                    self.last_click_time = None; // latch so a 3rd press is fresh
                    self.enter_group(node_id);
                    return;
                }
            }
        }
        if let Some(node_id) = self.header_under(viewport, sx, sy) {
            self.click_select(node_id, shift);
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
                self.click_select(id, shift);
            }
            None => {
                // Double-click on empty space opens the node picker at the
                // cursor. Two presses on empty space within the time +
                // distance window count as a double-click.
                let is_double = self.is_double_click(sx, sy, now, None);
                self.note_click(sx, sy, now, None);
                if is_double {
                    // Latch reset so a third press doesn't triple-fire.
                    self.last_click_time = None;
                    let (gx, gy) = self.to_graph(viewport, sx, sy);
                    self.pending_actions.push(PanelAction::OpenNodePicker {
                        screen_pos: (sx, sy),
                        graph_pos: (gx, gy),
                    });
                } else if shift {
                    // Shift+drag = rubber-band box select (replaces the
                    // selection with whatever the box covers). A shift-press
                    // with no drag is a no-op (guarded on release).
                    self.drag_mode = DragMode::Marquee {
                        origin_screen: (sx, sy),
                    };
                } else {
                    // Plain left-drag = pan, so the canvas stays navigable on a
                    // trackpad. A left-click with no drag clears the selection
                    // (handled on release).
                    self.drag_mode = DragMode::Pan;
                    self.drag_anchor = (sx, sy);
                    self.drag_pan_start = self.pan;
                }
            }
        }
    }

    /// Record a left-press for double-click detection. `node` is the node id
    /// under the press (`None` for empty space).
    fn note_click(&mut self, sx: f32, sy: f32, now: f32, node: Option<u32>) {
        self.last_click_time = Some(now);
        self.last_click_pos = (sx, sy);
        self.last_click_node = node;
    }

    /// True when the press at `(sx, sy, now)` over `node` completes a
    /// double-click of the previous press: same target, within the time and
    /// distance window.
    fn is_double_click(&self, sx: f32, sy: f32, now: f32, node: Option<u32>) -> bool {
        let dx = sx - self.last_click_pos.0;
        let dy = sy - self.last_click_pos.1;
        self.last_click_time
            .is_some_and(|t| now - t < DOUBLE_CLICK_SECONDS)
            && (dx * dx + dy * dy) < DOUBLE_CLICK_RADIUS_PX * DOUBLE_CLICK_RADIUS_PX
            && self.last_click_node == node
    }

    /// Apply a node click to the selection set. Shift toggles membership; a
    /// plain click on an unselected node selects just it; a plain click on an
    /// already-selected node leaves the (possibly multi-) selection intact so
    /// it can be dragged as a group.
    fn click_select(&mut self, id: u32, shift: bool) {
        if shift {
            if !self.selected.insert(id) {
                self.selected.remove(&id);
            }
        } else if !self.selected.contains(&id) {
            self.selected.clear();
            self.selected.insert(id);
        }
    }

    /// Replace the selection with exactly `id`. Used where multi-select
    /// doesn't apply (param-row focus).
    fn select_single(&mut self, id: u32) {
        self.selected.clear();
        self.selected.insert(id);
    }

    /// The selected node ids at the current scope, sorted for stable command
    /// payloads. Read by Layer 3's Ctrl+G to build the group selection.
    pub fn selected_ids(&self) -> Vec<u32> {
        let mut ids: Vec<u32> = self.selected.iter().copied().collect();
        ids.sort_unstable();
        ids
    }

    /// If exactly one node is selected and it's a group, its id — for
    /// Ctrl+Shift+G ungroup. `None` otherwise.
    pub fn single_selected_group(&self) -> Option<u32> {
        if self.selected.len() != 1 {
            return None;
        }
        let id = *self.selected.iter().next()?;
        self.nodes
            .iter()
            .find(|n| n.id == id && n.is_group)
            .map(|n| n.id)
    }

    /// Graph-space centroid of the current selection — the natural drop point
    /// for a new group node. Falls back to the layout origin when empty.
    pub fn selection_centroid(&self) -> (f32, f32) {
        let mut sx = 0.0;
        let mut sy = 0.0;
        let mut n = 0.0;
        for node in self.nodes.iter().filter(|nv| self.selected.contains(&nv.id)) {
            sx += node.pos_graph.0;
            sy += node.pos_graph.1;
            n += 1.0;
        }
        if n > 0.0 {
            (sx / n, sy / n)
        } else {
            LAYOUT_ORIGIN
        }
    }

    pub fn on_left_button_up(&mut self, viewport: Rect, sx: f32, sy: f32) {
        let prev = std::mem::replace(&mut self.drag_mode, DragMode::None);
        match prev {
            DragMode::None => {}
            DragMode::Pan => {
                // A left-press that didn't actually pan (cursor barely moved) is
                // a click on empty space — clear the selection. A real pan
                // leaves the selection alone.
                let moved = (sx - self.drag_anchor.0).hypot(sy - self.drag_anchor.1);
                if moved < CLICK_MOVE_SLOP_PX {
                    self.selected.clear();
                }
            }
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
            // The scrub emitted its value on each pointer move; nothing to
            // finalize on release.
            DragMode::ParamScrub { .. } => {}
            DragMode::Marquee { origin_screen } => {
                // A shift-press with no real drag leaves the selection alone —
                // don't let a zero-area box wipe it.
                let (ox, oy) = origin_screen;
                if (sx - ox).hypot(sy - oy) < CLICK_MOVE_SLOP_PX {
                    return;
                }
                // Build the graph-space rect from press to release; the nodes
                // it intersects become the selection (replace).
                let (gx0, gy0) = self.to_graph(viewport, ox.min(sx), oy.min(sy));
                let (gx1, gy1) = self.to_graph(viewport, ox.max(sx), oy.max(sy));
                let rect = (gx0, gy0, gx1 - gx0, gy1 - gy0);
                self.selected = marquee_hits(rect, &self.nodes).into_iter().collect();
                group_log!(
                    "marquee commit: {} node(s) selected {:?}",
                    self.selected.len(),
                    self.selected
                );
            }
        }
    }

    pub fn cursor(&self) -> (f32, f32) {
        self.cursor
    }

    /// Find the wire whose destination is `(to_node, to_port)`. Returns
    /// the wire's index in `self.wires`. Each input port has at most
    /// one incoming wire (enforced at graph-validate time), so this is
    /// unambiguous.
    fn wire_into(&self, to_node: u32, to_port: &str) -> Option<usize> {
        self.wires
            .iter()
            .position(|w| w.to_node == to_node && w.to_port == to_port)
    }

    /// Bounding rect of the "Reset to Default" header button. Single
    /// source of truth so render-side and click-hit-test use the same
    /// geometry.
    fn reset_button_rect(&self, viewport: Rect) -> Rect {
        let y = viewport.y + (HEADER_HEIGHT - RESET_BUTTON_H) * 0.5;
        let x = viewport.x + viewport.w - RESET_BUTTON_RIGHT_GAP - RESET_BUTTON_W;
        Rect {
            x,
            y,
            w: RESET_BUTTON_W,
            h: RESET_BUTTON_H,
        }
    }

    /// The single focused node id, or `None` when zero or several are
    /// selected. Read by the editor's right-sidebar panel to figure out which
    /// inner-node parameters to show as expose checkboxes — that surface only
    /// makes sense for one node, so a multi-selection reports `None`.
    pub fn selected_node_id(&self) -> Option<u32> {
        if self.selected.len() == 1 {
            self.selected.iter().copied().next()
        } else {
            None
        }
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

        // Live rubber-band rectangle while marquee-selecting.
        if let DragMode::Marquee { origin_screen } = &self.drag_mode {
            let (ox, oy) = *origin_screen;
            let (cx, cy) = self.cursor;
            let x = ox.min(cx);
            let y = oy.min(cy);
            let w = (cx - ox).abs();
            let h = (cy - oy).abs();
            ui.draw_bordered_rect(x, y, w, h, MARQUEE_FILL, 0.0, 1.0, MARQUEE_BORDER);
        }

        // Hover tooltip: the node's friendly summary, or — when the cursor is
        // over a param row — that param's help line. Drawn above the nodes but
        // below the popover, and only when the canvas is idle (a tooltip
        // chasing the cursor mid-drag would be noise).
        if matches!(self.drag_mode, DragMode::None) && !self.mapping_popover.is_open() {
            ui.push_layer(Layer::Tooltip);
            self.draw_hover_tooltip(ui, viewport, canvas);
            ui.pop_layer();
        }

        // Mapping popover floats above everything else so its handles and
        // buttons are never buried under a node it overlaps. It draws
        // unclipped (it may extend past the canvas lane) on the Overlay
        // layer; the lane clip is re-armed afterwards.
        ui.pop_immediate_clip();
        ui.push_layer(Layer::Overlay);
        self.mapping_popover.render(ui);
        ui.pop_layer();
        ui.push_immediate_clip(viewport.x, viewport.y, viewport.w, viewport.h);

        // Debug overlay last, on top of everything — it's a diagnostic HUD.
        if self.debug_overlay {
            ui.push_layer(Layer::Tooltip);
            self.draw_debug_overlay(ui, canvas);
            ui.pop_layer();
        }

        ui.pop_immediate_clip();
    }

    /// Floating help card near the cursor: a param's help line when the
    /// cursor is over a param row, otherwise the hovered node's friendly
    /// summary. Both come from the doc side-channels (`param_doc` and
    /// `NodeDescriptor`) resolved at snapshot time. No-op when there's
    /// nothing registered for whatever the cursor is over.
    fn draw_hover_tooltip(&self, ui: &mut UIRenderer, viewport: Rect, canvas: Rect) {
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
    fn draw_debug_overlay(&self, ui: &mut UIRenderer, canvas: Rect) {
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

    /// While dragging a wire from a port of colour `from_color`, classify the
    /// input port currently under the cursor: `Some(true)` compatible drop,
    /// `Some(false)` incompatible, `None` when the cursor isn't over a foreign
    /// input port. Compatibility is port-category equality (the canvas encodes
    /// category as colour); the real connect still validates server-side, so
    /// this is purely the live hint behind the ghost wire's green/red tint.
    fn wire_drop_compat(&self, viewport: Rect, from_node: u32, from_color: [f32; 4]) -> Option<bool> {
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

    /// Draw a node's primary-param sparkline (normalized 0..1 history) into the
    /// rect `(x, y, w, h)` as a soft polyline, y inverted so 1.0 sits at the top.
    /// Subtle by design — it signals "this knob is moving," it isn't a readout.
    fn draw_sparkline(
        &self,
        ui: &mut UIRenderer,
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

/// Walk `scope` (a path of group node ids) into `snap`, returning the
/// `(nodes, wires)` of the addressed level. Empty scope → the document root.
/// `None` if any id in the path isn't a group at its level — e.g. the group
/// was deleted or ungrouped out from under the canvas. Pure; unit-tested.
pub(crate) fn resolve_level<'a>(
    snap: &'a GraphSnapshot,
    scope: &[u32],
) -> Option<(&'a [NodeSnapshot], &'a [WireSnapshot])> {
    let mut nodes: &[NodeSnapshot] = &snap.nodes;
    let mut wires: &[WireSnapshot] = &snap.wires;
    for &gid in scope {
        let group = nodes.iter().find(|n| n.id == gid)?.group.as_deref()?;
        nodes = &group.nodes;
        wires = &group.wires;
    }
    Some((nodes, wires))
}

/// Locate a node by stable [`NodeId`](manifold_core::NodeId) anywhere in the
/// hierarchical snapshot. Returns the scope path of group runtime ids to its
/// level, those groups' titles (for the breadcrumb), and the node's runtime id.
/// `None` if no node carries that id. Used by jump-to-node to navigate the
/// canvas to a card param's defining node, even when it lives inside a group.
fn find_node_scope(
    snap: &GraphSnapshot,
    target: &manifold_core::NodeId,
) -> Option<(Vec<u32>, Vec<String>, u32)> {
    fn search(
        nodes: &[NodeSnapshot],
        target: &manifold_core::NodeId,
        path: &mut Vec<u32>,
        titles: &mut Vec<String>,
    ) -> Option<u32> {
        // Prefer a direct hit at this level over descending.
        if let Some(n) = nodes.iter().find(|n| &n.node_id == target) {
            return Some(n.id);
        }
        for n in nodes {
            if let Some(group) = n.group.as_deref() {
                path.push(n.id);
                titles.push(n.title.clone());
                if let Some(rid) = search(&group.nodes, target, path, titles) {
                    return Some(rid);
                }
                path.pop();
                titles.pop();
            }
        }
        None
    }
    if target.is_empty() {
        return None;
    }
    let mut path = Vec::new();
    let mut titles = Vec::new();
    search(&snap.nodes, target, &mut path, &mut titles).map(|rid| (path, titles, rid))
}

/// Resolve a card param id (a binding's `outer_param_id`) to the stable
/// [`NodeId`](manifold_core::NodeId) of the node it's exposed from, using the
/// snapshot's `outer_routings` (the same map for effects and generators). The
/// routing carries the node *handle*; we resolve that to the node's id so
/// jump-to-node addresses by the grouping-invariant identity. `None` when the
/// param isn't a routed binding or its node isn't in the snapshot.
pub(crate) fn resolve_card_param_node_id(
    snap: &GraphSnapshot,
    param_id: &str,
) -> Option<manifold_core::NodeId> {
    let handle = snap
        .outer_routings
        .iter()
        .find(|r| r.outer_param_id == param_id)
        .map(|r| r.node_handle.clone())?;
    node_id_for_handle(snap, &handle)
}

/// The stable `NodeId` of the node whose handle is `handle`, searched through
/// the full nested snapshot. `None` if no such (id-bearing) node exists.
fn node_id_for_handle(snap: &GraphSnapshot, handle: &str) -> Option<manifold_core::NodeId> {
    fn search(nodes: &[NodeSnapshot], handle: &str) -> Option<manifold_core::NodeId> {
        for n in nodes {
            if n.node_handle.as_deref() == Some(handle) && !n.node_id.is_empty() {
                return Some(n.node_id.clone());
            }
            if let Some(group) = n.group.as_deref()
                && let Some(id) = search(&group.nodes, handle)
            {
                return Some(id);
            }
        }
        None
    }
    search(&snap.nodes, handle)
}

/// Two ports can be wired iff they share a port category, which the canvas
/// encodes 1:1 as the port colour (Texture2D and its typed variant share one
/// colour, so they're treated as compatible — exactly the validator's view).
fn ports_compatible(from_color: [f32; 4], to_color: [f32; 4]) -> bool {
    from_color == to_color
}

/// Axis-aligned rectangle overlap, each `(x, y, w, h)`. Touching edges don't
/// count as overlapping (strict inequality), matching the marquee feel: a
/// node is grabbed only once the band actually crosses into it.
fn rects_overlap(a: (f32, f32, f32, f32), b: (f32, f32, f32, f32)) -> bool {
    a.0 < b.0 + b.2 && a.0 + a.2 > b.0 && a.1 < b.1 + b.3 && a.1 + a.3 > b.1
}

/// Ids of nodes whose box intersects the marquee `rect` (graph space). Pure;
/// unit-tested via `rects_overlap`.
fn marquee_hits(rect: (f32, f32, f32, f32), nodes: &[NodeView]) -> Vec<u32> {
    nodes
        .iter()
        .filter(|n| {
            rects_overlap(rect, (n.pos_graph.0, n.pos_graph.1, NODE_WIDTH, n.height()))
        })
        .map(|n| n.id)
        .collect()
}

/// Topology hash of one resolved level plus the scope path, so the canvas
/// re-runs layout when the displayed level changes (enter/leave a group)
/// even though the underlying snapshot document is byte-for-byte the same.
/// Param values are deliberately excluded — they refresh in place without a
/// relayout (see the param-only fast path in `set_snapshot`).
fn hash_level(scope: &[u32], nodes: &[NodeSnapshot], wires: &[WireSnapshot]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = ahash::AHasher::default();
    scope.hash(&mut h);
    nodes.len().hash(&mut h);
    for n in nodes {
        n.id.hash(&mut h);
        n.type_id.hash(&mut h);
    }
    wires.len().hash(&mut h);
    for w in wires {
        w.from_node.hash(&mut h);
        w.from_port.hash(&mut h);
        w.to_node.hash(&mut h);
        w.to_port.hash(&mut h);
    }
    h.finish()
}

#[cfg(test)]
mod tests {
    //! Pure-logic tests for the group-aware canvas. Everything that isn't
    //! pixels is exercised here so a misbehaving canvas points to rendering
    //! (eyes only), not logic. Per the handoff doc's debug-friendly mandate.
    use super::*;
    use manifold_renderer::node_graph::{
        GraphSnapshot, GroupSnapshot, NodeSnapshot, PortKindSnapshot, PortSnapshot, WireSnapshot,
    };

    fn port(name: &str) -> PortSnapshot {
        PortSnapshot {
            name: name.to_string(),
            kind: PortKindSnapshot::Texture2D,
        }
    }

    /// Build a plain (non-group) node snapshot with one `in` / one `out`.
    fn node(id: u32, type_id: &str, handle: Option<&str>) -> NodeSnapshot {
        NodeSnapshot {
            id,
            node_id: handle.map(manifold_core::NodeId::new).unwrap_or_default(),
            node_handle: handle.map(|h| h.to_string()),
            type_id: type_id.to_string(),
            title: handle.unwrap_or(type_id).to_string(),
            inputs: vec![port("in")],
            outputs: vec![port("out")],
            parameters: Vec::new(),
            editor_pos: None,
            breaks_dependency_cycle: false,
            group: None,
            wgsl_source: None,
        }
    }

    fn wire(fln: u32, fp: &str, tn: u32, tp: &str) -> WireSnapshot {
        WireSnapshot {
            from_node: fln,
            from_port: fp.to_string(),
            to_node: tn,
            to_port: tp.to_string(),
        }
    }

    /// Root: source(0) → group(10) → final(2). The group body is
    /// group_input(0) → inner(1) → group_output(2).
    fn grouped_snapshot() -> GraphSnapshot {
        let body = GroupSnapshot {
            nodes: vec![
                node(0, "system.group_input", None),
                node(1, "node.blur", Some("inner")),
                node(2, "system.group_output", None),
            ],
            wires: vec![wire(0, "src", 1, "in"), wire(1, "out", 2, "out")],
            tint: None,
        };
        let mut group = node(10, GROUP_TYPE_ID, Some("tweak"));
        group.inputs = vec![port("src")];
        group.outputs = vec![port("out")];
        group.group = Some(Box::new(body));
        GraphSnapshot {
            nodes: vec![
                node(0, "system.source", Some("source")),
                group,
                node(2, "system.final_output", Some("final")),
            ],
            wires: vec![wire(0, "out", 10, "src"), wire(10, "out", 2, "in")],
            outer_routings: Vec::new(),
        }
    }

    #[test]
    fn resolve_level_root_then_descend_then_invalid() {
        let snap = grouped_snapshot();

        // Empty scope → document root (3 nodes incl. the group).
        let (rn, rw) = resolve_level(&snap, &[]).expect("root resolves");
        assert_eq!(rn.len(), 3);
        assert_eq!(rw.len(), 2);
        assert!(rn.iter().any(|n| n.type_id == GROUP_TYPE_ID));

        // Into the group → its body (group_input, inner, group_output).
        let (bn, bw) = resolve_level(&snap, &[10]).expect("group body resolves");
        assert_eq!(bn.len(), 3);
        assert_eq!(bw.len(), 2);
        assert!(bn.iter().any(|n| n.node_handle.as_deref() == Some("inner")));

        // A non-group id (source) or a missing id → None.
        assert!(resolve_level(&snap, &[0]).is_none());
        assert!(resolve_level(&snap, &[999]).is_none());
    }

    #[test]
    fn set_snapshot_marks_groups_and_navigation_swaps_level() {
        let snap = grouped_snapshot();
        let mut canvas = GraphCanvas::new();

        // Root level: the group node is flagged and the inner node is hidden.
        canvas.set_snapshot(&snap);
        assert_eq!(canvas.nodes.len(), 3);
        let group = canvas.nodes.iter().find(|n| n.is_group).expect("group view");
        assert_eq!(group.id, 10);
        assert!(canvas.nodes.iter().all(|n| n.title != "inner"));

        // Descend → the canvas now shows the group body.
        canvas.enter_group(10);
        canvas.set_snapshot(&snap);
        assert_eq!(canvas.scope_path(), &[10]);
        assert!(canvas.nodes.iter().any(|n| n.title == "inner"));
        assert!(canvas.nodes.iter().all(|n| !n.is_group));

        // Exit → back to root.
        assert!(canvas.exit_group());
        canvas.set_snapshot(&snap);
        assert!(canvas.scope_path().is_empty());
        assert!(canvas.nodes.iter().any(|n| n.is_group));
    }

    #[test]
    fn visible_node_thumbnails_preview_image_nodes_and_groups_via_producer() {
        let snap = grouped_snapshot();
        let mut canvas = GraphCanvas::new();
        canvas.set_snapshot(&snap);
        // A viewport huge enough that nothing culls.
        let vp = Rect::new(-10_000.0, -10_000.0, 20_000.0, 20_000.0);
        let thumbs = canvas.visible_node_thumbnails(vp);
        // All three root nodes output an image (the fixture's ports are
        // Texture2D), so each gets a preview strip: source, final, and the
        // group — but the group is keyed to its OUTPUT PRODUCER ("inner"), not
        // its own id, since that inner node is the cell already in the atlas.
        assert_eq!(thumbs.len(), 3, "two image nodes + the group via its producer");
        assert!(
            thumbs.iter().any(|(id, ..)| id.as_str() == "inner"),
            "the group previews its inner output producer"
        );
        assert!(
            thumbs.iter().all(|(id, ..)| id.as_str() != "tweak"),
            "a group is never keyed to its own id — always its producer's"
        );
        // Each preview-strip rect is the 16:9 image area and has positive size.
        assert!(thumbs.iter().all(|(_, _, _, w, h)| *w > 0.0 && *h > 0.0));
    }

    #[test]
    fn stale_scope_falls_back_to_root() {
        let snap = grouped_snapshot();
        let mut canvas = GraphCanvas::new();
        canvas.set_snapshot(&snap);
        canvas.enter_group(10);
        canvas.set_snapshot(&snap);
        assert_eq!(canvas.scope_path(), &[10]);

        // The group vanishes (e.g. an undo dissolved it). Next push of a
        // snapshot without node 10 must drop the canvas back to root rather
        // than render an empty level.
        let mut flat = grouped_snapshot();
        flat.nodes.retain(|n| n.id != 10);
        flat.wires.clear();
        canvas.set_snapshot(&flat);
        assert!(canvas.scope_path().is_empty());
    }

    #[test]
    fn breadcrumb_segments_track_scope_titles() {
        let snap = grouped_snapshot();
        let mut canvas = GraphCanvas::new();
        let vp = Rect::new(0.0, 0.0, 1200.0, 800.0);

        // Root → no breadcrumb.
        canvas.set_snapshot(&snap);
        assert!(canvas.breadcrumb_segments(vp).is_empty());

        // Inside the group → [Root, tweak], with "tweak" current.
        canvas.enter_group(10);
        canvas.set_snapshot(&snap);
        let segs = canvas.breadcrumb_segments(vp);
        assert_eq!(segs.len(), 2);
        assert_eq!(segs[0].2, "Root");
        assert!(!segs[0].3, "root crumb is an ancestor, not current");
        assert_eq!(segs[1].2, "tweak");
        assert!(segs[1].3, "deepest crumb is current");

        // Breadcrumb jump back to root.
        canvas.set_scope_depth(0);
        assert!(canvas.scope_path().is_empty());
    }

    #[test]
    fn rects_overlap_is_strict_and_symmetric() {
        let a = (0.0, 0.0, 10.0, 10.0);
        // Overlapping.
        assert!(rects_overlap(a, (5.0, 5.0, 10.0, 10.0)));
        assert!(rects_overlap((5.0, 5.0, 10.0, 10.0), a));
        // Fully containing.
        assert!(rects_overlap(a, (2.0, 2.0, 1.0, 1.0)));
        // Touching edge only — not an overlap (strict).
        assert!(!rects_overlap(a, (10.0, 0.0, 5.0, 5.0)));
        // Disjoint.
        assert!(!rects_overlap(a, (20.0, 20.0, 5.0, 5.0)));
    }

    #[test]
    fn double_click_window_requires_same_target() {
        let mut canvas = GraphCanvas::new();
        // First press on node 7.
        canvas.note_click(100.0, 100.0, 1.0, Some(7));
        // Second press just after, same spot, same node → double.
        assert!(canvas.is_double_click(100.5, 100.0, 1.1, Some(7)));
        // Same timing but a different node → not a double.
        assert!(!canvas.is_double_click(100.5, 100.0, 1.1, Some(8)));
        // Same node but too far → not a double.
        assert!(!canvas.is_double_click(140.0, 100.0, 1.1, Some(7)));
        // Same node but too slow → not a double.
        assert!(!canvas.is_double_click(100.5, 100.0, 1.0 + 5.0, Some(7)));
    }

    #[test]
    fn wrap_text_breaks_on_spaces_within_limit() {
        let lines = wrap_text("the quick brown fox jumps", 11);
        // Every line is within the limit and nothing is dropped.
        assert!(lines.iter().all(|l| l.chars().count() <= 11));
        assert_eq!(lines.join(" "), "the quick brown fox jumps");
        assert!(lines.len() > 1);
    }

    #[test]
    fn wrap_text_keeps_an_overlong_word_whole() {
        // A single word past the limit isn't chopped mid-word; it gets its
        // own line and overflows the box slightly rather than corrupting.
        let lines = wrap_text("supercalifragilistic ok", 8);
        assert_eq!(lines[0], "supercalifragilistic");
        assert_eq!(lines[1], "ok");
    }

    #[test]
    fn wrap_text_empty_input_is_empty() {
        assert!(wrap_text("", 20).is_empty());
        assert!(wrap_text("   ", 20).is_empty());
    }

    // ── Layered auto-layout ─────────────────────────────────────────

    #[test]
    fn layout_uncrosses_a_simple_swap() {
        // Two columns, edges 0→3 and 1→2 — one crossing as ordered.
        let mut l = LayeredLayout {
            num_cols: 2,
            column: vec![0, 0, 1, 1],
            height: vec![40.0; 4],
            order: vec![vec![0, 1], vec![2, 3]],
            up_edges: vec![vec![], vec![], vec![(1, 20.0, 20.0)], vec![(0, 20.0, 20.0)]],
            down_edges: vec![vec![(3, 20.0, 20.0)], vec![(2, 20.0, 20.0)], vec![], vec![]],
        };
        assert_eq!(l.count_crossings(), 1);
        l.minimise_crossings();
        assert_eq!(l.count_crossings(), 0);
    }

    #[test]
    fn layout_straightens_a_chain() {
        // 0 → 1 → 2 across three columns: equal heights and port offsets,
        // so coordinate assignment should give all three the same top.
        let off = 25.0;
        let l = LayeredLayout {
            num_cols: 3,
            column: vec![0, 1, 2],
            height: vec![50.0; 3],
            order: vec![vec![0], vec![1], vec![2]],
            up_edges: vec![vec![], vec![(0, off, off)], vec![(1, off, off)]],
            down_edges: vec![vec![(1, off, off)], vec![(2, off, off)], vec![]],
        };
        let y = l.assign_y();
        assert!((y[0] - y[1]).abs() < 0.01, "y0 {} y1 {}", y[0], y[1]);
        assert!((y[1] - y[2]).abs() < 0.01, "y1 {} y2 {}", y[1], y[2]);
    }

    #[test]
    fn layout_threads_long_edge_straight_through_waypoint() {
        // node0 (col0) → node1 (col2), routed through waypoint lvid 2 in
        // col1. The two ports and the waypoint centre must end up colinear.
        let off = 30.0;
        let mid = LAYOUT_DUMMY_H * 0.5;
        let l = LayeredLayout {
            num_cols: 3,
            column: vec![0, 2, 1],
            height: vec![50.0, 50.0, LAYOUT_DUMMY_H],
            order: vec![vec![0], vec![2], vec![1]],
            up_edges: vec![vec![], vec![(2, mid, off)], vec![(0, off, mid)]],
            down_edges: vec![vec![(2, off, mid)], vec![], vec![(1, mid, off)]],
        };
        let y = l.assign_y();
        let p_out = y[0] + off; // node0 output port
        let p_mid = y[2] + mid; // waypoint centre
        let p_in = y[1] + off; // node1 input port
        assert!((p_out - p_mid).abs() < 0.01, "out {p_out} mid {p_mid}");
        assert!((p_in - p_mid).abs() < 0.01, "in {p_in} mid {p_mid}");
    }

    // ── Live values overlay ─────────────────────────────────────────

    /// A Float param snapshot over `[0, 1]`, current value `current`.
    fn float_param(name: &str, current: f32) -> manifold_renderer::node_graph::ParamSnapshot {
        manifold_renderer::node_graph::ParamSnapshot {
            name: name.to_string(),
            label: name.to_string(),
            kind: manifold_renderer::node_graph::ParamSnapshotKind::Float,
            default_value: 0.0,
            current_value: current,
            range: Some((0.0, 1.0)),
            enum_labels: None,
            exposed: false,
            summary: None,
            vec_value: None,
            string_value: None,
            table_value: None,
        }
    }

    /// One plain node (`id == 1`, given handle so its `node_id` is set) carrying
    /// `params`, wrapped in a root snapshot.
    fn snapshot_with_param_node(
        handle: &str,
        params: Vec<manifold_renderer::node_graph::ParamSnapshot>,
    ) -> GraphSnapshot {
        let mut n = node(1, "node.gain", Some(handle));
        n.parameters = params;
        GraphSnapshot {
            nodes: vec![n],
            wires: Vec::new(),
            outer_routings: Vec::new(),
        }
    }

    #[test]
    fn apply_live_values_refreshes_on_face_value_and_fill() {
        let mut canvas = GraphCanvas::new();
        canvas.set_snapshot(&snapshot_with_param_node("gain", vec![float_param("amount", 0.25)]));
        // The frozen snapshot value formats to 0.25.
        assert_eq!(canvas.find_node(1).unwrap().params[0].value, "0.25");

        // A live value of 0.80 overlays the frozen value, refreshing the string,
        // the fill bar, and the scrub anchor — matched by stable node_id.
        let live = vec![(manifold_core::NodeId::new("gain"), vec![("amount", 0.8_f32)])];
        canvas.apply_live_values(&live);
        let pv = &canvas.find_node(1).unwrap().params[0];
        assert_eq!(pv.value, "0.80");
        assert!((pv.fill.unwrap() - 0.8).abs() < 1e-3);
        assert!((pv.scrub.unwrap().current_value - 0.8).abs() < 1e-6);
    }

    #[test]
    fn apply_live_values_skips_the_actively_scrubbed_param() {
        let mut canvas = GraphCanvas::new();
        canvas.set_snapshot(&snapshot_with_param_node("gain", vec![float_param("amount", 0.25)]));
        // Mid-scrub on this exact (node, param): the drag stays the source of
        // truth, so the live feed must not overwrite it.
        canvas.drag_mode = DragMode::ParamScrub {
            node_id: 1,
            param_name: "amount".to_string(),
            range: (0.0, 1.0),
            start_value: 0.25,
            is_int: false,
            press_origin_x: 0.0,
        };
        let live = vec![(manifold_core::NodeId::new("gain"), vec![("amount", 0.8_f32)])];
        canvas.apply_live_values(&live);
        assert_eq!(canvas.find_node(1).unwrap().params[0].value, "0.25");
    }

    #[test]
    fn apply_live_values_empty_feed_is_a_noop() {
        let mut canvas = GraphCanvas::new();
        canvas.set_snapshot(&snapshot_with_param_node("gain", vec![float_param("amount", 0.25)]));
        canvas.apply_live_values(&Vec::new());
        assert_eq!(canvas.find_node(1).unwrap().params[0].value, "0.25");
    }

    #[test]
    fn apply_live_values_ignores_unmatched_node_ids() {
        let mut canvas = GraphCanvas::new();
        canvas.set_snapshot(&snapshot_with_param_node("gain", vec![float_param("amount", 0.25)]));
        // A live entry for a different node leaves this one untouched.
        let live = vec![(manifold_core::NodeId::new("other"), vec![("amount", 0.8_f32)])];
        canvas.apply_live_values(&live);
        assert_eq!(canvas.find_node(1).unwrap().params[0].value, "0.25");
    }

    #[test]
    fn apply_live_values_feeds_sparkline_history() {
        let mut canvas = GraphCanvas::new();
        canvas.set_snapshot(&snapshot_with_param_node("gain", vec![float_param("amount", 0.2)]));
        for v in [0.2_f32, 0.5, 0.8] {
            canvas.apply_live_values(&vec![(
                manifold_core::NodeId::new("gain"),
                vec![("amount", v)],
            )]);
        }
        let hist = canvas
            .spark_history
            .get(&manifold_core::NodeId::new("gain"))
            .expect("history recorded for the primary param");
        assert_eq!(hist.len(), 3);
        // Range is 0..1, so the stored normalized (fill) value equals the input.
        assert!((hist[0] - 0.2).abs() < 1e-4);
        assert!((hist[2] - 0.8).abs() < 1e-4);
    }

    #[test]
    fn sparkline_history_is_capped_at_capacity() {
        let mut canvas = GraphCanvas::new();
        canvas.set_snapshot(&snapshot_with_param_node("gain", vec![float_param("amount", 0.2)]));
        for i in 0..(SPARK_CAPACITY + 10) {
            let v = (i % 10) as f32 / 10.0;
            canvas.apply_live_values(&vec![(
                manifold_core::NodeId::new("gain"),
                vec![("amount", v)],
            )]);
        }
        let hist = canvas
            .spark_history
            .get(&manifold_core::NodeId::new("gain"))
            .unwrap();
        assert_eq!(hist.len(), SPARK_CAPACITY, "ring buffer holds the cap, no more");
    }

    #[test]
    fn topology_rebuild_prunes_stale_sparkline_history() {
        let mut canvas = GraphCanvas::new();
        canvas.set_snapshot(&snapshot_with_param_node("gain", vec![float_param("amount", 0.2)]));
        canvas.apply_live_values(&vec![(
            manifold_core::NodeId::new("gain"),
            vec![("amount", 0.5_f32)],
        )]);
        assert!(canvas.spark_history.contains_key(&manifold_core::NodeId::new("gain")));

        // A real topology change (different runtime id, so `hash_level` differs
        // and `set_snapshot` takes the full-rebuild path) evicts the old node's
        // trace so the history map can't accrete across a session.
        let mut n = node(2, "node.gain", Some("other"));
        n.parameters = vec![float_param("amount", 0.2)];
        let snap2 = GraphSnapshot {
            nodes: vec![n],
            wires: Vec::new(),
            outer_routings: Vec::new(),
        };
        canvas.set_snapshot(&snap2);
        assert!(!canvas.spark_history.contains_key(&manifold_core::NodeId::new("gain")));
    }

    // ── Jump-to-node ────────────────────────────────────────────────

    #[test]
    fn find_node_scope_locates_root_and_nested_nodes() {
        let snap = grouped_snapshot();
        // Root-level node: empty scope.
        let (path, _, rid) = find_node_scope(&snap, &manifold_core::NodeId::new("source")).unwrap();
        assert!(path.is_empty());
        assert_eq!(rid, 0);
        // Node inside the group: scope = [group runtime id], title carried.
        let (path, titles, rid) =
            find_node_scope(&snap, &manifold_core::NodeId::new("inner")).unwrap();
        assert_eq!(path, vec![10]);
        assert_eq!(titles, vec!["tweak".to_string()]);
        assert_eq!(rid, 1);
        // Unknown id.
        assert!(find_node_scope(&snap, &manifold_core::NodeId::new("nope")).is_none());
    }

    #[test]
    fn focus_node_descends_into_the_group_and_selects() {
        let snap = grouped_snapshot();
        let mut canvas = GraphCanvas::new();
        canvas.set_snapshot(&snap);
        assert!(canvas.focus_node(&snap, &manifold_core::NodeId::new("inner")));
        assert_eq!(canvas.scope_path(), &[10]);
        assert_eq!(canvas.selected_ids(), vec![1]);
        assert_eq!(canvas.pending_focus, Some(1));
    }

    #[test]
    fn focus_node_unknown_id_is_a_noop() {
        let snap = grouped_snapshot();
        let mut canvas = GraphCanvas::new();
        canvas.set_snapshot(&snap);
        assert!(!canvas.focus_node(&snap, &manifold_core::NodeId::new("nope")));
        assert!(canvas.scope_path().is_empty());
        assert_eq!(canvas.pending_focus, None);
    }

    #[test]
    fn resolve_card_param_node_id_via_outer_routing() {
        let mut snap = grouped_snapshot();
        snap.outer_routings = vec![manifold_renderer::node_graph::OuterParamRouting {
            outer_label: "Amount".into(),
            outer_param_id: "user.inner.amount.0".into(),
            node_handle: "inner".into(),
            inner_param: "amount".into(),
            source: manifold_renderer::node_graph::OuterParamSource::Static,
        }];
        let nid = resolve_card_param_node_id(&snap, "user.inner.amount.0").unwrap();
        assert_eq!(nid.as_str(), "inner");
        // Unrouted param id resolves to nothing.
        assert!(resolve_card_param_node_id(&snap, "user.nope.0").is_none());
    }

    // ── Group tint ──────────────────────────────────────────────────

    #[test]
    fn cycle_group_tint_emits_first_palette_colour_for_untinted_group() {
        let snap = grouped_snapshot();
        let mut canvas = GraphCanvas::new();
        canvas.set_snapshot(&snap);
        canvas.select_single(10); // the group node
        canvas.request_cycle_group_tint();
        let emitted = canvas.drain_actions().into_iter().find_map(|a| match a {
            PanelAction::SetGroupTint {
                group_id: 10, tint, ..
            } => Some(tint),
            _ => None,
        });
        assert_eq!(emitted, Some(Some(GROUP_TINT_PALETTE[0])));
    }

    #[test]
    fn cycle_group_tint_noop_without_a_selected_group() {
        let snap = grouped_snapshot();
        let mut canvas = GraphCanvas::new();
        canvas.set_snapshot(&snap);
        canvas.select_single(0); // a plain node, not a group
        canvas.request_cycle_group_tint();
        assert!(
            canvas
                .drain_actions()
                .iter()
                .all(|a| !matches!(a, PanelAction::SetGroupTint { .. })),
            "no tint action without a selected group"
        );
    }

    // ── Connection type feedback ────────────────────────────────────

    #[test]
    fn ports_compatible_is_colour_category_equality() {
        // Same category → compatible (the ghost wire reads green).
        assert!(ports_compatible(PORT_TEXTURE2D_COLOR, PORT_TEXTURE2D_COLOR));
        // Cross-category → incompatible (red), so a mis-wire is caught pre-drop.
        assert!(!ports_compatible(PORT_TEXTURE2D_COLOR, PORT_SCALAR_COLOR));
        assert!(!ports_compatible(PORT_SCALAR_COLOR, PORT_ARRAY_COLOR));
    }
}
