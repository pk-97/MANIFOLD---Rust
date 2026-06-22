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
// Re-exported so sibling submodules see both via `use super::*;`. The canvas
// emits `GraphEditCommand` (Phase 4.3); `PanelAction` remains for the mapping
// popover's `EffectMapping*` edits, which are a separate command family.
pub(crate) use manifold_ui::{GraphEditCommand, PanelAction};
use manifold_ui::transform::Axis;

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

// Re-export the macro so sibling submodules can use it via `use super::*;`.
// (The unused-import lint can't see macro use through a glob re-export.)
#[allow(unused_imports)]
pub(crate) use group_log;

// ── Submodules (one concern each). `GraphCanvas` is one struct whose impl
// blocks are split across these siblings; the view-model types, layout
// engine, and free functions live in their concern file. ──
mod camera;
mod hit;
mod interaction;
mod layout;
mod model;
mod render;

#[cfg(test)]
mod tests;

// Re-exports so every submodule (via `use super::*;`) and external callers
// (`crate::graph_canvas::X`) keep resolving the moved types and free fns
// unchanged. The PUBLIC surface other files depend on — `Rect`,
// `GraphCanvas`, `resolve_level`, `resolve_card_param_node_id`,
// `node_preview_target` — is re-exported here.
// Only the names referenced cross-module (or externally as
// `crate::graph_canvas::X`) are re-exported. Module-internal helpers stay
// private to their file; test-only items (`LayeredLayout`, `ports_compatible`,
// `rects_overlap`) are imported directly by `tests.rs` from their module.
pub(crate) use hit::marquee_hits;
pub(crate) use interaction::DragMode;
pub(crate) use model::{
    NodeView, ParamView, PortHit, WireView, find_node_scope, node_preview_target,
    resolve_card_param_node_id, resolve_level, spark_has_variation, wrap_text,
};

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

pub struct GraphCanvas {
    pub(crate) nodes: Vec<NodeView>,
    pub(crate) wires: Vec<WireView>,
    /// Hash of the current topology (node ids+types + wire endpoints).
    /// Compared on each `set_snapshot` to skip layout recomputation
    /// when only parameter values changed.
    pub(crate) topology_hash: u64,
    pub(crate) pan: (f32, f32),
    pub(crate) zoom: f32,
    pub(crate) cursor: (f32, f32),
    pub(crate) drag_mode: DragMode,
    pub(crate) drag_anchor: (f32, f32),
    pub(crate) drag_pan_start: (f32, f32),
    pub(crate) hovered: Option<u32>,
    /// Selected node ids at the current scope level. A set so the user can
    /// rubber-band or Shift-click several nodes before collapsing them into a
    /// group. A plain click selects exactly one; Shift toggles.
    pub(crate) selected: ahash::AHashSet<u32>,
    /// `instance.graph.is_some()` for the watched effect. Drives the
    /// "Reset to Default" affordance in the header — only shown when
    /// the user has diverged from the bundled preset.
    pub(crate) has_graph_mod: bool,
    /// Graph edits accumulated this frame from canvas interactions.
    /// Drained by the editor window's input loop after each event.
    pub(crate) pending_actions: Vec<GraphEditCommand>,
    /// Per-node collapse state (UI-only, keyed by runtime node id so it
    /// survives snapshot rebuilds like positions do). A collapsed node
    /// hides its on-face param rows but keeps its header and ports, so it
    /// can still be wired. Absent = expanded.
    pub(crate) collapsed: ahash::AHashMap<u32, bool>,
    /// In-place mapping editor for a card binding, anchored on the param
    /// row it was right-clicked from. Surface-agnostic widget; the canvas
    /// just hosts it, draws it on top of the nodes, and forwards pointer
    /// events to it while it's open. Closed by default.
    pub(crate) mapping_popover: MappingPopover,
    /// Wall-clock seconds at the last left-press, used to detect a
    /// double-click — on empty space (opens the node picker) or on a group
    /// node (descends into it). `None` until the first press, and reset to
    /// `None` after a double-click fires so a third press starts a fresh
    /// single-click rather than re-triggering.
    pub(crate) last_click_time: Option<f32>,
    /// Screen-space cursor at the last left-press. Paired with
    /// `last_click_time` so a double-click only registers when the two
    /// presses land within a few pixels of each other.
    pub(crate) last_click_pos: (f32, f32),
    /// Node id under the last left-press (`None` for empty space). A
    /// double-click only counts when both presses land on the *same* target,
    /// so dragging between two groups doesn't accidentally enter one.
    pub(crate) last_click_node: Option<u32>,
    /// Current view scope — a path of group node ids from the document root
    /// to the level being shown. Empty = root. Pushed on enter-group, popped
    /// on exit. The canvas re-resolves which level to render from the live
    /// snapshot each frame using this path, so navigation is purely UI-local
    /// (no command, no content round-trip).
    pub(crate) scope: Vec<u32>,
    /// Display titles of the groups in `scope`, captured at enter time (the
    /// ancestor group nodes aren't in the current level's views, so their
    /// names have to be remembered). Always the same length as `scope`; the
    /// breadcrumb bar reads `["Root", scope_titles…]`.
    pub(crate) scope_titles: Vec<String>,
    /// When true, draw the debug overlay (scope path, selection, hover, drag
    /// mode) in the canvas corner. Toggled by the backtick key. The handoff
    /// doc's mandate: let the canvas tell Peter what it thinks is happening
    /// without a debugger.
    pub(crate) debug_overlay: bool,
    /// Set when the view descends into a group; consumed by the next
    /// `set_snapshot`, which auto-formats the level *only if it has never been
    /// laid out* (every node's `editor_pos` is `None`). Preserves any manual
    /// arrangement — once a layout exists (hand-moved or a prior auto-format),
    /// this never fires for that group again.
    pub(crate) format_on_enter: bool,
    /// Per-node recent history (normalized 0..1) of the node's primary numeric
    /// param, keyed by stable `NodeId`. Pushed each frame by
    /// [`Self::apply_live_values`] from the live tap and drawn as a small
    /// sparkline on the collapsed node face, so a modulated knob reads as a
    /// moving trace — the design's "even the invisible math nodes become
    /// legible." Bounded to [`SPARK_CAPACITY`] points per node; pruned to the
    /// live node set on a topology rebuild. Empty when no editor is watching.
    pub(crate) spark_history: ahash::AHashMap<manifold_core::NodeId, std::collections::VecDeque<f32>>,
    /// Runtime id of a node the canvas should centre + select once it is laid
    /// out at the current scope — set by [`Self::focus_node`] (jump-to-node from
    /// a card param) and consumed by [`Self::resolve_pending_focus`] one frame
    /// later, after `set_snapshot` has rebuilt the (possibly newly-entered)
    /// level so the node's position is known. `None` when nothing is pending.
    pub(crate) pending_focus: Option<u32>,
    /// Request to frame the whole level (zoom-to-fit) on the next viewport-aware
    /// present. Set when the canvas is created (editor open) and on every scope
    /// change (group enter/exit, breadcrumb jump), consumed by
    /// [`Self::apply_pending_fit`] once the level has finite-positioned nodes —
    /// same retry-next-frame contract as [`Self::pending_focus`]. Non-destructive:
    /// it only moves the camera, never node positions.
    pub(crate) fit_pending: bool,
    /// Lowercased find-a-node query. Non-empty dims nodes whose title/handle
    /// doesn't contain it and brightens the matches, so a name jumps out of a
    /// busy graph. Empty = no search active. Set live by the editor's search box.
    pub(crate) node_search: String,
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
            fit_pending: true,
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
