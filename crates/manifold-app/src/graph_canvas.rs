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

use manifold_renderer::node_graph::{GraphSnapshot, NodeSnapshot, PortKindSnapshot, WireSnapshot};
use manifold_renderer::ui_renderer::UIRenderer;
use manifold_ui::PanelAction;

use manifold_core::effect_graph_def::GROUP_TYPE_ID;

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
const NODE_WIDTH: f32 = 168.0;
const NODE_HEADER_HEIGHT: f32 = 22.0;
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
const PORT_ARRAY_COLOR: [f32; 4] = [0.50, 1.00, 0.62, 1.0];
const PORT_CAMERA_COLOR: [f32; 4] = [1.00, 0.55, 0.55, 1.0];
const PORT_LIGHT_COLOR: [f32; 4] = [1.00, 0.95, 0.55, 1.0];
const PORT_MATERIAL_COLOR: [f32; 4] = [0.95, 0.65, 0.40, 1.0];
/// Group node tint. A group reads as a distinct, slightly heavier box than an
/// atom so a complex graph shows its structure at a glance — teal-leaning
/// header + a faint teal body wash, the colour we reserve for "container".
const GROUP_HEADER_BG: [f32; 4] = [0.18, 0.34, 0.40, 1.0];
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
const TEXT_PRIMARY: [u8; 4] = [220, 220, 230, 255];
const TEXT_SECONDARY: [u8; 4] = [150, 150, 165, 255];
const TEXT_HEADER: [u8; 4] = [240, 240, 250, 255];
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
}

impl NodeView {
    fn height(&self) -> f32 {
        let port_rows = self.inputs.len().max(self.outputs.len()) as f32;
        NODE_HEADER_HEIGHT + self.body_h() + port_rows * PORT_ROW_HEIGHT + 6.0
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

    /// Y offset where port rows start, below the header and the body block.
    fn ports_y_offset(&self) -> f32 {
        NODE_HEADER_HEIGHT + self.body_h()
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
    let value = match p.kind {
        ParamSnapshotKind::Enum => p
            .enum_labels
            .as_ref()
            .and_then(|labels| labels.get(p.current_value as usize).cloned())
            .unwrap_or_else(|| format!("{}", p.current_value as i64)),
        ParamSnapshotKind::Bool => {
            if p.current_value >= 0.5 { "On" } else { "Off" }.to_string()
        }
        ParamSnapshotKind::Int => format!("{}", p.current_value as i64),
        ParamSnapshotKind::Float => format!("{:.2}", p.current_value),
        // Stored radians, shown as degrees (see ParamType::Angle).
        ParamSnapshotKind::Angle => format!("{:.0}°", p.current_value.to_degrees()),
        // Stored rad/s, shown as Hz (see ParamType::Frequency).
        ParamSnapshotKind::Frequency => {
            format!("{:.2} Hz", p.current_value / std::f32::consts::TAU)
        }
        ParamSnapshotKind::Trigger => format!("{}", p.current_value as i64),
        ParamSnapshotKind::Other => p.summary.clone().unwrap_or_else(|| "—".to_string()),
    };
    let fill = match p.kind {
        ParamSnapshotKind::Float
        | ParamSnapshotKind::Angle
        | ParamSnapshotKind::Frequency
        | ParamSnapshotKind::Int => p.range.map(|(lo, hi)| {
            if hi > lo {
                ((p.current_value - lo) / (hi - lo)).clamp(0.0, 1.0)
            } else {
                0.0
            }
        }),
        _ => None,
    };
    let scrub = match p.kind {
        ParamSnapshotKind::Float
        | ParamSnapshotKind::Angle
        | ParamSnapshotKind::Frequency
        | ParamSnapshotKind::Int => p.range.map(|(lo, hi)| ScrubInfo {
            range: (lo, hi),
            current_value: p.current_value,
            is_int: matches!(p.kind, ParamSnapshotKind::Int),
        }),
        _ => None,
    };
    ParamView {
        name: p.name.clone(),
        label: p.label.clone(),
        value,
        fill,
        scrub,
    }
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
                    node.params = sn.parameters.iter().map(format_param_for_node).collect();
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
                handle: n.node_handle.clone(),
                title: n.title.clone(),
                params: n.parameters.iter().map(format_param_for_node).collect(),
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

        // Iterative relaxation. Wires terminating on a cycle-breaking
        // node (e.g. `node.feedback`) close a per-frame feedback loop —
        // `Graph::connect` permits them and `topological_sort` ignores
        // them. The layout must do the same; otherwise depth accumulates
        // around the loop one column per relaxation pass and consumers
        // get pushed thousands of pixels off-screen to the right.
        // With back-edges removed the topology is a DAG, so this
        // converges in ≤ n passes; we cap at n+1 as a safety net.
        for _ in 0..=n {
            let mut changed = false;
            for w in &self.wires {
                let (Some(&from_i), Some(&to_i)) =
                    (id_to_idx.get(&w.from_node), id_to_idx.get(&w.to_node))
                else {
                    continue;
                };
                if self.nodes[to_i].breaks_dependency_cycle {
                    continue;
                }
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
            let block_top = ny + header_h;
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
        let row_top = ny + header_h + pi as f32 * row_h;
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
        // build their own scissored batches on top in `render_overlay_additive`.
        ui.set_immediate_clip(viewport.x, viewport.y, viewport.w, viewport.h);
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
            return;
        }

        self.draw_grid(ui, canvas);

        // Wires in two passes so the focused node's connections read clearly
        // over the rest: dim/normal wires first, then focus wires on top.
        for wire in &self.wires {
            if !self.wire_touches_focus(wire) {
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

        // Mapping popover floats above everything else so its handles and
        // buttons are never buried under a node it overlaps.
        self.mapping_popover.render(ui);

        // Debug overlay last, on top of everything — it's a diagnostic HUD.
        if self.debug_overlay {
            self.draw_debug_overlay(ui, canvas);
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
        // ghost. 0.55 alpha keeps it readable as "in flight".
        let port_color = node.outputs[idx].color;
        let ghost_color = [port_color[0], port_color[1], port_color[2], 0.55];
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
            GROUP_HEADER_BG
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

        let row_h = PARAM_ROW_H * self.zoom;
        let text_size = (9.0 * self.zoom).max(7.0);
        let pad_x = 8.0 * self.zoom;
        let inner_w = sw - 2.0 * pad_x;

        // Collapsed: one summary line ("Mode: FoldX"), so a folded node still
        // shows its key value without the full param wall.
        if show_text
            && node.collapsed
            && let Some(summary) = node.summary.as_deref()
        {
            let text_y = sy + header_h + 2.0 * self.zoom;
            let max_chars = (inner_w / (text_size * 0.55)) as usize;
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

        // Expanded: every param row — label + value with a fill bar under
        // ranged values, each draggable in place (see ParamScrub).
        let expanded_params: &[ParamView] = if show_text && !node.collapsed {
            &node.params
        } else {
            &[]
        };
        for (i, p) in expanded_params.iter().enumerate() {
            let row_y = sy + header_h + i as f32 * row_h;
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

        // Wire takes its colour from the from-port's kind (matching the
        // port circles). Control/value wires (scalar, orange) fan out from
        // driver nodes and dominate the spaghetti, so they fade to a faint
        // baseline unless their node is focused; data wires stay readable;
        // and any wire touching the focused node lights up over the rest.
        let port_color = from.outputs[from_idx].color;
        let focused = self.wire_touches_focus(wire);
        let is_control = from.outputs[from_idx].is_control;
        let alpha = if focused {
            0.95
        } else if is_control {
            0.16
        } else {
            0.7
        };
        let wire_color = [port_color[0], port_color[1], port_color[2], alpha];

        // Sample the bezier into ~30 short line segments. Step count
        // scales with screen-space length so close-up curves stay smooth.
        let approx_len = ((sx1 - sx0).abs() + (sy1 - sy0).abs() + 2.0 * dx).max(40.0);
        let steps = (approx_len / 12.0).clamp(16.0, 64.0) as i32;
        let thickness = (1.6 * self.zoom).clamp(1.2, 2.4) * if focused { 1.5 } else { 1.0 };
        let mut prev = cubic_bezier(0.0, sx0, sy0, cx0, cy0, cx1, cy1, sx1, sy1);
        for i in 1..=steps {
            let t = i as f32 / steps as f32;
            let curr = cubic_bezier(t, sx0, sy0, cx0, cy0, cx1, cy1, sx1, sy1);
            ui.draw_line(prev.0, prev.1, curr.0, curr.1, thickness, wire_color);
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
fn resolve_level<'a>(
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
            node_handle: handle.map(|h| h.to_string()),
            type_id: type_id.to_string(),
            title: handle.unwrap_or(type_id).to_string(),
            inputs: vec![port("in")],
            outputs: vec![port("out")],
            parameters: Vec::new(),
            editor_pos: None,
            breaks_dependency_cycle: false,
            group: None,
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
}
