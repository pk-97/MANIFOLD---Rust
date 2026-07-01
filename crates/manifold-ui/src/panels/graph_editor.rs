//! [`GraphEditorPanel`] — right-sidebar panel inside the graph-editor
//! window for V2 user-exposed parameters.
//!
//! Phase 3 of `docs/EFFECT_RUNTIME_UNIFICATION.md`. The first UITree
//! component to live inside the editor window. Renders a vertical
//! list of the currently-selected node's parameters; each row carries
//! a checkbox indicating whether that param is currently exposed on
//! the effect card. Click a checkbox → emit
//! [`GraphEditCommand::EffectParamExpose`] → content thread runs
//! `ToggleEffectParamExposeCommand` → `PresetInstance.user_param_bindings`
//! gains/loses the entry.
//!
//! ## Selection model
//!
//! The graph-canvas in the editor window owns the "selected node id"
//! state today. The panel is configured each frame with that id plus
//! the active `PresetInstance`'s effect-index and currently-exposed
//! `(node_handle, inner_param)` pairs; the panel rebuilds its UITree
//! subtree only when something material changed (selection,
//! parameters, or exposed-set).
//!
//! ## Dispatch model
//!
//! Two paths, split by gesture kind (UI Architecture Overhaul Phase 6.2):
//! - **Discrete clicks** fold onto the shared [`IntentRegistry`] — now generic,
//!   so this panel registers `IntentRegistry<GraphEditCommand>` intents at build
//!   via [`GraphEditorPanel::register_intents`] and the app resolves clicks
//!   through it, exactly like every chrome panel. This replaces the old
//!   per-row `handle_click_event` id-matching loop. The Phase 4.5 blocker —
//!   "the registry carries `PanelAction`, this panel emits `GraphEditCommand`" —
//!   is gone now that 6.1 made the registry generic over the action type.
//! - **Stateful drags** stay in `handle_event(&UIEvent)`: `DragBegin`/`Drag`/
//!   `DragEnd` scrub a numeric/colour value cell against a captured anchor.
//!   Drag is inherently stateful (it mutates `self.drag` across frames), so it
//!   does not fit the build-time intent model — the same boundary the registry
//!   draws everywhere (intent dispatch is for node→action gestures; drag/scroll
//!   stay in the stateful path). See `docs/CANVAS_API_DESIGN.md` §3.
//!
//! This panel stays imperative (it full-rebuilds each frame; there's no
//! `build()`/`update()` dual-write to collapse).

use std::collections::{HashMap, HashSet};

use crate::color;
use crate::input::UIEvent;
use crate::intent::{Gesture, IntentRegistry};
use crate::node::*;
use crate::tree::UITree;
use crate::types::{ParamConvert, SerializedParamValue};
// The stable graph-node identity (engine `NodeId`), aliased to avoid colliding
// with the UI tree's own `node::NodeId` (a `u32`) brought in via `node::*`.
use manifold_foundation::NodeId as GraphNodeId;

use crate::GraphEditCommand;

/// UI-facing kind for one inner-node parameter, mirroring the
/// renderer-side `ParamSnapshotKind` without making this crate depend
/// on `manifold-renderer`. The editor-window glue in `manifold-app`
/// converts at the boundary (since that crate sees both sides).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphEditorParamKind {
    Float,
    /// Float-backed angle. Behaves exactly like `Float` for storage, drag,
    /// and serialization (the stored value stays RADIANS so wired modulation
    /// and preset math stay correct), but the value cell displays DEGREES.
    /// Conversion happens only at the display boundary in
    /// `format_inner_param_value`.
    Angle,
    /// Float-backed frequency. Behaves exactly like `Float` for storage, drag,
    /// and serialization (stored value stays RADIANS PER SECOND), but the value
    /// cell displays HERTZ (rad/s ÷ 2π). Display-only, like `Angle`.
    Frequency,
    Int,
    Bool,
    Enum,
    /// Momentary "fire once" button. Renders as a click-once button on
    /// the outer card; click handler increments storage by one.
    Trigger,
    /// RGBA colour. Rendered as a swatch plus R/G/B/A channel sliders that scrub
    /// in place; the live value is carried in [`GraphEditorParam::vec_value`].
    /// Not single-slot card-exposable (a macro slot is one `f32`), but editable.
    Color,
    /// 2/3/4-component vector. Rendered as per-component (X/Y/Z/W) sliders that
    /// scrub in place, carried in [`GraphEditorParam::vec_value`]. Editable but
    /// not single-slot card-exposable.
    Vec2,
    Vec3,
    Vec4,
    /// Text / path string. Shown read-only as its value; a path-like param
    /// (name contains folder/path/file/dir) also gets a Browse button that
    /// opens a native folder picker. Free-text editing isn't inline yet.
    String,
    /// Remaining structured types (Table) with no dedicated inline editor yet —
    /// shown as a disabled row.
    Other,
}

// Path-param detection and table-cell formatting are shared with the node
// canvas (the on-node editors classify + format a param identically). One
// definition each in `graph_canvas::model`.
use crate::graph_canvas::{fmt_table_cell, is_path_param};

impl GraphEditorParamKind {
    /// Component count for the multi-component (colour / vector) kinds, else 0.
    fn vec_components(self) -> usize {
        match self {
            GraphEditorParamKind::Color | GraphEditorParamKind::Vec4 => 4,
            GraphEditorParamKind::Vec3 => 3,
            GraphEditorParamKind::Vec2 => 2,
            _ => 0,
        }
    }

    /// Per-channel labels for the multi-component editor (`Color` uses RGBA,
    /// vectors use XYZW). Empty for scalar kinds.
    fn channel_labels(self) -> &'static [&'static str] {
        match self {
            GraphEditorParamKind::Color => &["R", "G", "B", "A"],
            GraphEditorParamKind::Vec2 => &["X", "Y"],
            GraphEditorParamKind::Vec3 => &["X", "Y", "Z"],
            GraphEditorParamKind::Vec4 => &["X", "Y", "Z", "W"],
            _ => &[],
        }
    }
}

/// UI-facing description of one inner-node parameter, owned for
/// `Send`ability across the content/UI boundary.
#[derive(Debug, Clone)]
pub struct GraphEditorParam {
    /// Stable param name — used as `inner_param` in the resulting
    /// `UserParamBinding`.
    pub name: String,
    /// Display label.
    pub label: String,
    pub kind: GraphEditorParamKind,
    pub default_value: f32,
    /// Current value on the live node — what the renderer is actually
    /// using this frame. Drives the inspector's read-out so users can
    /// see what each node is configured to do.
    pub current_value: f32,
    /// `(min, max)` for sliders. `None` when the underlying ParamDef
    /// didn't declare a range.
    pub range: Option<(f32, f32)>,
    /// Enum option labels indexed by enum value, for rendering
    /// "FoldX" instead of `6`. `None` for non-enum params.
    pub enum_labels: Option<Vec<String>>,
    /// Free-form summary for non-numeric params (currently only
    /// `Table` — rendered as `"6×5"` in the inspector). `None` for
    /// numeric params, which render `current_value` instead.
    pub summary: Option<String>,
    /// Live multi-component value for `Color` / `Vec2` / `Vec3` / `Vec4` kinds,
    /// RGBA / XYZW order (`Vec2`/`Vec3` zero-pad the tail). `[0.0; 4]` for scalar
    /// kinds, which use `current_value`. Drives the swatch + channel editor.
    pub vec_value: [f32; 4],
    /// Raw untruncated value for `String` kinds — what the inline editor seeds
    /// with. `summary` is lossy (path tails, 24-char cap), so it can't
    /// round-trip an edit. `None` for non-String params.
    pub string_value: Option<String>,
    /// Row-major cell values for `Table` params — drives the inline grid
    /// editor. `None` for non-Table params (`summary` only carries "N×M").
    pub table_value: Option<Vec<Vec<f32>>>,
}

/// UI-facing view of the currently-selected node, decoupled from the
/// renderer's internal `NodeSnapshot`. The editor-window present path
/// builds this from the live snapshot; the panel only knows about
/// this shape.
#[derive(Debug, Clone)]
pub struct GraphEditorNodeView {
    /// Canvas-stable runtime id for the selected node. Used as the
    /// `node_id` when emitting `GraphEditCommand::SetGraphNodeParam` so the
    /// app-side handler can build a `SetGraphNodeParamCommand` keyed
    /// on the same stable id the canvas uses for selection.
    pub runtime_node_id: u32,
    /// Stable [`GraphNodeId`] of the node — the addressing
    /// identity the expose action stores, invariant under grouping.
    /// `Default` (empty) for anonymous boundary nodes.
    pub node_id: GraphNodeId,
    /// Stable handle if the node was registered with one. `None` for
    /// anonymous boundary nodes (Source / FinalOutput) — those have no
    /// user-exposable params. Display / id-readability only.
    pub node_handle: Option<String>,
    /// Display title for the node (header label fallback).
    pub title: String,
    pub parameters: Vec<GraphEditorParam>,
    /// WGSL kernel source when this node is a `wgsl_compute*` node carrying a
    /// custom kernel. Drives the inspector's "Edit Code" button; `None` for
    /// every other node (the button isn't shown).
    pub wgsl_source: Option<String>,
}

/// Value inspector for a previewed node that has no image output (control /
/// math / envelope). Replaces the black preview pane with what the node is and
/// the live signal flowing through it. Built by the host from the node's
/// descriptor + the live scalar I/O captured this frame.
#[derive(Debug, Clone, Default)]
pub struct NodeInspector {
    /// Node display title.
    pub title: String,
    /// One-line "what it does" (the descriptor summary, else purpose). May be
    /// empty if the node carries no descriptor text.
    pub description: String,
    /// Live scalar input port values `(port_name, value)` this frame.
    pub inputs: Vec<(String, f32)>,
    /// Live scalar output port values — the signal the node is producing.
    pub outputs: Vec<(String, f32)>,
}

/// Preview-sidebar DEFAULT width inside the graph-editor window — docks on the
/// LEFT. The live width is now owned by the editor workspace's [`manifold_ui::Dock`]
/// (the user can drag the divider); this constant is just the seed. The sidebar
/// is monitors-only (the inner-node param list lives under the card lane), so the
/// width sizes the two stacked 16:9 preview panes — wider means bigger Node
/// Output / Master Out monitors. Single source of the number: [`crate::dock`].
pub const SIDEBAR_WIDTH: f32 = crate::dock::EDITOR_LEFT_DEFAULT;

/// Card-lane DEFAULT width inside the graph-editor window — docks on the RIGHT
/// (same side as the main timeline's inspector). The lane renders the real
/// `ParamCardPanel` for the edited effect/generator. Wide enough to fit the full
/// card (label + slider + value + the E/→ row buttons). The live width is now
/// owned by the editor workspace's [`manifold_ui::Dock`]; render and input both
/// read `dock.rects(area)`, so the canvas origin and click hit-testing can no
/// longer desync (they used to share this constant by hand). Single source of
/// the number: [`crate::dock`].
pub const EDITOR_CARD_LANE_WIDTH: f32 = crate::dock::EDITOR_RIGHT_DEFAULT;

const ROW_H: f32 = 28.0;
const PADDING: f32 = 12.0;
const HEADER_H: f32 = 32.0;
const CHECKBOX_W: f32 = 22.0;
const CHECKBOX_H: f32 = 22.0;
const CHECKBOX_GAP: f32 = 10.0;
const FONT_SIZE: u16 = 12;
const HEADER_FONT_SIZE: u16 = 14;

/// A node-param value cell / inline action button — the kit's neutral chip, so the
/// graph editor's clickable cells carry the same one control outline + radius as
/// every other control in the app. `align` is Right for value readouts, Center for
/// action buttons (Edit Code / Browse) and table cells.
fn value_cell_style(align: TextAlign) -> UIStyle {
    crate::chrome::components::chip_style(
        crate::chrome::components::ChipSurface::Neutral,
        FONT_SIZE,
        align,
        color::CHIP_RADIUS,
        4.0,
    )
}

/// Per-row state captured during `build` so `register_intents` (clicks) and
/// `handle_event` (drags) can map a tree node id back to its parameter without
/// re-walking the snapshot. Inner-node rows track BOTH the expose checkbox and
/// the editable value cell, since each lands on a distinct tree node id and
/// emits a distinct `GraphEditCommand`.
///
/// The top "Effect Parameters" list is read-only after the V2
/// unification (any toggling lives on the per-node rows below), so it
/// produces no `RowState` entries — `RowState` exists only for clickable
/// inner-node rows now.
#[derive(Debug, Clone)]
enum RowState {
    /// The auto-gain toggle under the node-output preview. Click flips
    /// normalization on the preview pane via
    /// [`GraphEditCommand::SetNodePreviewNormalize`].
    PreviewNormalizeToggle { button_node_id: NodeId },
    /// A row backed by an inner-node param. Click semantics depend on
    /// whether the param is a target of one of the effect's
    /// static-block bindings (`static_block_slot: Some(i)`) or not.
    ///
    /// - Click on `checkbox_node_id` →
    ///   - If `static_block_slot.is_some()`: `EffectStaticParamExpose`
    ///     (flip `param_values[slot].exposed` — no second binding is
    ///     created, because the static-block routing already drives
    ///     this inner param every frame).
    ///   - Otherwise: `EffectParamExpose`
    ///     (add / remove a `UserParamBinding`).
    /// - Click / drag on `value_cell_node_id` → `SetGraphNodeParam`
    ///   (mutate the per-card graph through
    ///   `SetGraphNodeParamCommand`).
    InnerNode {
        checkbox_node_id: NodeId,
        /// Tree id of the editable value cell (rendered as a button
        /// so it receives drag events). `None` for `Other`-kind params
        /// that have no editable representation in V1.
        value_cell_node_id: Option<NodeId>,
        /// Canvas-stable id of the underlying graph node. Used as the
        /// `node_id` carried by `SetGraphNodeParam`.
        node_runtime_id: u32,
        /// Stable graph-node id — the addressing identity the expose
        /// action stores.
        node_id: GraphNodeId,
        node_handle: String,
        inner_param: String,
        label: String,
        kind: GraphEditorParamKind,
        min: f32,
        max: f32,
        default_value: f32,
        /// Current value before this row's pending edit. Drag-scrub
        /// uses this as the starting anchor (drag delta is applied
        /// relative to it).
        current_value: f32,
        /// Enum option count, snapshot from the live ParamDef. Click-
        /// cycle on an enum cell wraps modulo this count.
        enum_labels_count: usize,
        /// Enum option labels, snapshot from the live ParamDef. Carried onto
        /// the expose action so an exposed enum keeps its labelled card slider.
        /// Empty for non-enum params.
        enum_labels: Vec<String>,
        convert: ParamConvert,
        currently_exposed: bool,
        /// Slot index when this inner-param is the target of a
        /// static-block binding. `Some(i)` routes the expose toggle
        /// through `EffectStaticParamExpose` (flipping the slot's
        /// `exposed` flag) instead of adding a redundant
        /// `UserParamBinding`. `None` for inner params that have no
        /// static-block routing — the toggle adds / removes a
        /// user-binding through `EffectParamExpose`.
        static_block_slot: Option<usize>,
        /// `true` when this inner param is shadowed by a wire on the
        /// node's same-named scalar input port (port-shadows-param
        /// convention). The wire drives the param every frame, so the
        /// expose checkbox and the value cell are visually disabled
        /// and the click handler short-circuits to `Vec::new()` for
        /// both targets. Removing the wire is the only way to reclaim
        /// local control or expose the param on the card.
        wire_driven: bool,
    },
    /// One channel of a multi-component (`Color` / `Vec`) param's inline editor.
    /// Each channel cell scrubs its own component; the drag rebuilds the full
    /// value and emits a single `SetGraphNodeParam` carrying the whole
    /// colour/vector. Click is a no-op (channels edit by drag only).
    VecComponent {
        value_cell_node_id: NodeId,
        node_runtime_id: u32,
        inner_param: String,
        /// Color / Vec2 / Vec3 / Vec4 — picks the emitted `SerializedParamValue`.
        kind: GraphEditorParamKind,
        /// Which component (0 = R/X, 1 = G/Y, ...).
        channel: usize,
        /// The full current value; a drag overwrites `base[channel]` and emits
        /// the rebuilt whole.
        base: [f32; 4],
        /// Scrub range for this channel (0..1 for colour; the param's declared
        /// range or a sensible default for vectors).
        min: f32,
        max: f32,
    },
    /// Browse button on a path-like String param. Click opens a native folder
    /// picker (host-side) and sets the param to the chosen path.
    BrowseButton {
        button_node_id: NodeId,
        node_runtime_id: u32,
        param_name: String,
    },
    /// Clickable value cell on a free-text (non-path) String param. Click opens
    /// the inline text editor anchored over the cell, seeded with `current`.
    /// `rect` (x, y, w, h) is captured at build so the click can anchor without
    /// re-walking the tree.
    EditTextButton {
        button_node_id: NodeId,
        node_runtime_id: u32,
        param_name: String,
        current: String,
        rect: (f32, f32, f32, f32),
    },
    /// "Edit Code" button on a `wgsl_compute` node. Click opens the multiline
    /// WGSL editor seeded with the node's kernel `source`.
    WgslButton {
        button_node_id: NodeId,
        node_runtime_id: u32,
        source: String,
    },
    /// One clickable cell in a `Table` param's grid editor. Click opens the
    /// inline numeric editor over the cell. The full table is looked up by
    /// `param_name` at click time (off `selected_node`) so each cell row stays
    /// small. `rect` (x, y, w, h) is captured at build for the anchor.
    TableCell {
        button_node_id: NodeId,
        node_runtime_id: u32,
        param_name: String,
        row: usize,
        col: usize,
        rect: (f32, f32, f32, f32),
    },
}

/// In-progress drag scrub on a Float/Int value cell. Captured when
/// `DragBegin` lands on a value cell and consumed by `Drag` /
/// `DragEnd`. The panel only allows one drag at a time — `DragBegin`
/// while a drag is already active replaces the prior anchor.
#[derive(Debug, Clone, Copy)]
struct DragState {
    /// Tree id of the value-cell button being dragged.
    value_cell_node_id: NodeId,
    /// Canvas-stable graph node id — used to build the
    /// `SetGraphNodeParam` action.
    node_runtime_id: u32,
    /// Whether to emit Float or Int values during the drag.
    kind: GraphEditorParamKind,
    /// `(min, max)` for the param being dragged. Drag delta is scaled
    /// so a `DRAG_FULL_RANGE_PX` movement covers the full range.
    range: (f32, f32),
    /// Value at the start of the drag. Each `Drag` event applies the
    /// cumulative delta to this anchor — much steadier than chaining
    /// deltas through the live snapshot, which lags by one frame.
    start_value: f32,
    /// Screen-x at the press origin (from `DragBegin.origin.x`). Used
    /// together with `Drag.pos.x` to compute the cumulative drag
    /// delta in pixels, then mapped to value-space via
    /// `DRAG_FULL_RANGE_PX`.
    press_origin_x: f32,
    /// Multi-component context when scrubbing one channel of a `Color` / `Vec`
    /// param: the channel index and the full base value, so each `Drag` rebuilds
    /// the whole colour/vector and emits it as one `SetGraphNodeParam`. `None`
    /// for a plain scalar scrub.
    vec: Option<VecDrag>,
}

/// The colour/vector context of an in-progress channel scrub.
#[derive(Debug, Clone, Copy)]
struct VecDrag {
    /// Color / Vec2 / Vec3 / Vec4 — picks the emitted `SerializedParamValue`.
    kind: GraphEditorParamKind,
    /// Which component this drag moves.
    channel: usize,
    /// The full value at press time; the dragged channel is overwritten on
    /// each `Drag`, the rest carried through unchanged.
    base: [f32; 4],
}

/// Pixels of horizontal drag corresponding to a full param range
/// sweep. Slightly larger than the typical sidebar width so a single
/// dramatic drag covers the full range.
const DRAG_FULL_RANGE_PX: f32 = 240.0;

/// Right-sidebar panel inside the graph-editor window.
///
/// Owns no GPU state — it builds UITree nodes inside the editor's
/// `UIRoot` each time `build` is called. Lifecycle is per-frame rebuild
/// guarded by a "needs rebuild" flag set by `configure` whenever the
/// inputs change.
#[derive(Default)]
pub struct GraphEditorPanel {
    /// The effect this sidebar is editing — set when the editor
    /// opens for an effect chain. `None` when no effect is active.
    effect_index: Option<usize>,
    /// View of the currently-selected node, owned. `None` when no
    /// node is selected or the selection is anonymous (no
    /// `node_handle`, so its params are not user-exposable).
    selected_node: Option<GraphEditorNodeView>,
    /// Card-exposure lookup: `(node_handle, inner_param)` keys for
    /// every inner-node param currently exposed on the effect card,
    /// merging:
    /// - All `PresetInstance.user_param_bindings`.
    /// - Static-block routings whose slot has `param_values[i].exposed = true`.
    ///
    /// Drives the per-node checkbox state and lets the click handler
    /// emit the right action (Expose=true vs false) without consulting
    /// any other state.
    exposed_keys: HashSet<(String, String)>,
    /// `(node_handle, inner_param) → outer slider label` for every
    /// outer effect-card param that drives an inner-node param every
    /// frame. Rows in this map render with the value cell disabled
    /// and a "Driven by '<outer>'" hint — editing them from here is
    /// pointless because the routing overwrites the edit each frame.
    outer_driven: HashMap<(String, String), String>,
    /// `(node_handle, inner_param) → static-block slot index` for every
    /// inner-node param that is the target of one of the effect's
    /// static-block bindings. Built from the snapshot's
    /// `OuterParamRouting.outer_param_id` resolved through the
    /// effect-def's `id_to_index` table. Lets the per-node checkbox
    /// click route to `EffectStaticParamExpose` (toggling the slot's
    /// `exposed` flag) instead of stacking a redundant
    /// `UserParamBinding` on an already-routed inner param.
    static_block_targets: HashMap<(String, String), usize>,
    /// `(node_handle, inner_param)` keys for every inner param
    /// shadowed by a wire on the same-named scalar input port. Rows
    /// matching a key render the expose checkbox and value cell as
    /// disabled with a "← wired" hint after the label; the click
    /// handler short-circuits on the disabled targets. Built from
    /// the live `EffectGraphDef.wires` by `app_render`.
    wire_driven_keys: HashSet<(String, String)>,
    /// Whether the node-output preview pane is applying auto-gain /
    /// normalization. Mirrors the app-side state (pushed in each `configure`);
    /// drives the preview toggle's checkmark. Default off until the first
    /// `configure` lands the real value (app default is also off).
    normalize_preview: bool,
    /// Value inspector for a previewed non-image node, or `None` when the
    /// preview is an image (or nothing is previewed). When `Some`, the top of
    /// the sidebar shows the node's description + live I/O instead of the
    /// preview toggle. Set each frame by the host.
    node_inspector: Option<NodeInspector>,
    /// Per-row state, populated during `build`.
    rows: Vec<RowState>,
    /// Root container for everything this panel owns inside the tree.
    /// `None` until first build.
    root_id: Option<NodeId>,
    /// In-progress drag scrub on a Float/Int value cell. `None` when
    /// no drag is active. Tree rebuilds preserve this so a drag that
    /// began before a rebuild keeps emitting `SetGraphNodeParam`
    /// against the same anchor (otherwise the value would snap back
    /// to the live snapshot every frame).
    drag: Option<DragState>,
}

impl GraphEditorPanel {
    pub fn new() -> Self {
        Self::default()
    }

    /// Update the panel's input data. Bumps the rebuild fingerprint
    /// when anything changed so the next `build` actually rebuilds
    /// the UITree subtree.
    pub fn configure(
        &mut self,
        effect_index: Option<usize>,
        selected_node: Option<&GraphEditorNodeView>,
        exposed_keys: HashSet<(String, String)>,
        outer_driven: HashMap<(String, String), String>,
        static_block_targets: HashMap<(String, String), usize>,
        wire_driven_keys: HashSet<(String, String)>,
    ) {
        self.effect_index = effect_index;
        self.selected_node = selected_node.cloned();
        self.exposed_keys = exposed_keys;
        self.outer_driven = outer_driven;
        self.static_block_targets = static_block_targets;
        self.wire_driven_keys = wire_driven_keys;
    }

    /// Set whether the node-output preview pane shows auto-gain /
    /// normalization, so the toggle under the preview renders the right
    /// checkmark. Mirrors the app-side state; called each frame before
    /// [`Self::build`]. Separate from [`Self::configure`] so it doesn't churn
    /// the many param-inspector inputs.
    pub fn set_node_preview_normalize(&mut self, on: bool) {
        self.normalize_preview = on;
    }

    /// Set (or clear) the value inspector shown when a non-image node is
    /// previewed. Called each frame before [`Self::build`]; `None` restores the
    /// normal toggle + params layout.
    pub fn set_node_inspector(&mut self, inspector: Option<NodeInspector>) {
        self.node_inspector = inspector;
    }

    /// Build the UITree subtree at the given viewport. Idempotent
    /// w.r.t. tree state — clears the previous root and re-adds.
    pub fn build(&mut self, tree: &mut UITree, viewport: Rect) {
        // Wipe any previous subtree by detaching the old root. Cheap:
        // tree.clear_subtree drops the descendants. (Falls back to a
        // full tree.clear() — see ui_root invocation in app_render.)
        // Here we just rebuild from scratch each time.
        self.rows.clear();

        let bg_id = Some(tree.add_panel(
            None,
            viewport.x,
            viewport.y,
            viewport.width,
            viewport.height,
            UIStyle {
                bg_color: color::EFFECT_CARD_INNER_BG_C32,
                ..UIStyle::default()
            },
        ));
        self.root_id = bg_id;

        let mut y = viewport.y + PADDING;

        // The value inspector for a non-image node renders in the pinned
        // node-output pane at the sidebar top (see `render_node_inspector`),
        // not here — this region is the param list. Only the image-preview
        // auto-gain toggle precedes the list, and only when a node-output
        // *image* (not the value inspector) is on screen.
        if self.node_inspector.is_none() {
            // ── Node-preview "Smart preview" toggle ───────────────
            // Flips the semantic encoding on the node-output pane so
            // dark/signed intermediates are legible. Node preview only —
            // never touches the live render. Added before the early-returns
            // below so it's always clickable.
            let cb_x = viewport.x + PADDING;
            let cb_y = y + (ROW_H - CHECKBOX_H) * 0.5;
            let cb_id = tree.add_button(
                bg_id,
                cb_x,
                cb_y,
                CHECKBOX_W,
                CHECKBOX_H,
                checkbox_style(self.normalize_preview, true),
                if self.normalize_preview { "✓" } else { "" },
            );
            let label_x = cb_x + CHECKBOX_W + CHECKBOX_GAP;
            tree.add_label(
                bg_id,
                label_x,
                y,
                (viewport.x + viewport.width - PADDING - label_x).max(10.0),
                ROW_H,
                "Smart preview",
                UIStyle {
                    text_color: color::TEXT_WHITE_C32,
                    font_size: FONT_SIZE,
                    text_align: TextAlign::Left,
                    ..UIStyle::default()
                },
            );
            self.rows.push(RowState::PreviewNormalizeToggle {
                button_node_id: cb_id,
            });
            y += ROW_H;
        }

        // ── Selected Node section (the inspector) ─────────────────
        // The effect-card mirror lives in the editor's card lane (right
        // side); this panel renders docked under it and is purely the
        // clicked node's parameter inspector: every param of the selected
        // node with its live value, expose checkbox, and driver hints.
        tree.add_label(
            bg_id,
            viewport.x + PADDING,
            y,
            viewport.width - 2.0 * PADDING,
            HEADER_H - PADDING,
            "Inner-Node Parameters",
            UIStyle {
                text_color: color::TEXT_WHITE_C32,
                font_size: HEADER_FONT_SIZE,
                text_align: TextAlign::Left,
                ..UIStyle::default()
            },
        );
        y += HEADER_H;

        // Empty state — nothing selected, or selected node carries no
        // user-exposable parameters.
        let Some(node) = self.selected_node.clone() else {
            tree.add_label(
                bg_id,
                viewport.x + PADDING,
                y,
                viewport.width - 2.0 * PADDING,
                ROW_H,
                "Select a node to expose its parameters.",
                crate::chrome::components::panel_state_style(
                    crate::chrome::components::PanelStateKind::Empty,
                ),
            );
            return;
        };

        let Some(handle) = node.node_handle.clone() else {
            tree.add_label(
                bg_id,
                viewport.x + PADDING,
                y,
                viewport.width - 2.0 * PADDING,
                ROW_H,
                "This node has no stable handle.",
                crate::chrome::components::panel_state_style(
                    crate::chrome::components::PanelStateKind::Empty,
                ),
            );
            return;
        };

        // `wgsl_compute` nodes carry an editable kernel — surface an "Edit Code"
        // button that opens the multiline WGSL editor over the node's source.
        if let Some(src) = node.wgsl_source.clone() {
            let btn_w = (viewport.width - 2.0 * PADDING).max(40.0);
            let btn_id = tree.add_button(
                bg_id,
                viewport.x + PADDING,
                y,
                btn_w,
                ROW_H,
                value_cell_style(TextAlign::Center),
                "Edit Code…",
            );
            self.rows.push(RowState::WgslButton {
                button_node_id: btn_id,
                node_runtime_id: node.runtime_node_id,
                source: src,
            });
            y += ROW_H + 6.0;
        }

        for ps in &node.parameters {
            // Colour / vector params get a dedicated inline editor (swatch +
            // per-channel sliders) rather than the single-cell scalar row.
            if ps.kind.vec_components() > 0 {
                y = self.build_vec_param(tree, bg_id, viewport, y, node.runtime_node_id, ps);
                continue;
            }
            // String params show their value; path-like ones add a Browse button.
            if ps.kind == GraphEditorParamKind::String {
                y = self.build_string_param(tree, bg_id, viewport, y, node.runtime_node_id, ps);
                continue;
            }
            // Table params (gradient stops, numeric sequences) get an inline
            // grid of editable cells.
            if let Some(rows) = ps.table_value.as_ref().filter(|r| !r.is_empty()) {
                y = self.build_table_param(
                    tree,
                    bg_id,
                    viewport,
                    y,
                    node.runtime_node_id,
                    ps,
                    rows,
                );
                continue;
            }
            // Remaining unsupported types — show a row but
            // disabled. V2's single-slot surface can't carry them.
            let supported = matches!(
                ps.kind,
                GraphEditorParamKind::Float
                    | GraphEditorParamKind::Angle
                    | GraphEditorParamKind::Frequency
                    | GraphEditorParamKind::Int
                    | GraphEditorParamKind::Bool
                    | GraphEditorParamKind::Enum
                    | GraphEditorParamKind::Trigger
            );
            let is_exposed = self
                .exposed_keys
                .contains(&(handle.clone(), ps.name.clone()));
            // Outer-driven: an outer effect-card slider routes into
            // this inner param every frame. The row stays editable
            // — the binding apply path skips when the outer slot is
            // unchanged, so inline edits survive — but a "↳ <outer>"
            // hint after the label tells the user *which* outer
            // slider will reclaim control if they move it (or if it
            // has automation that does so each frame).
            let outer_driver = self
                .outer_driven
                .get(&(handle.clone(), ps.name.clone()))
                .cloned();
            // Wire-driven: the node's same-named scalar input port
            // has an incoming wire that shadows this param every
            // frame (port-shadows-param). The checkbox and value cell
            // become read-only — local edits and exposure toggles
            // would lie about what controls the param. Removing the
            // wire is the only way to reclaim either.
            let is_wire_driven = self
                .wire_driven_keys
                .contains(&(handle.clone(), ps.name.clone()));
            let editable = supported && !is_wire_driven;

            let cb_x = viewport.x + PADDING;
            let cb_y = y + (ROW_H - CHECKBOX_H) * 0.5;
            let cb_style = checkbox_style(is_exposed, supported && !is_wire_driven);
            let cb_id = tree.add_button(
                bg_id,
                cb_x,
                cb_y,
                CHECKBOX_W,
                CHECKBOX_H,
                cb_style,
                if is_exposed { "✓" } else { "" },
            );

            let label_x = cb_x + CHECKBOX_W + CHECKBOX_GAP;
            // Row split: label on the left, current value on the
            // right. Lets the user see what each inner param is
            // *currently set to*, not just what it's named.
            let row_remaining = (viewport.x + viewport.width - PADDING - label_x).max(10.0);
            let value_w = (row_remaining * 0.45).max(60.0);
            let label_w = (row_remaining - value_w).max(10.0);
            // Label + optional driver hint inline so the user can
            // see at a glance which surface controls this param:
            // "↳ Outer" for an outer card slider routing in every
            // frame, "← wired" for a same-name scalar input wire.
            // Wire wins when both are present (the wire short-circuits
            // the binding apply path), so we surface it first.
            let label_str = if is_wire_driven {
                format!("{}  ← wired", ps.label)
            } else if let Some(outer) = outer_driver.as_ref() {
                format!("{}  ↳ {outer}", ps.label)
            } else {
                ps.label.clone()
            };
            tree.add_label(
                bg_id,
                label_x,
                y,
                label_w,
                ROW_H,
                &label_str,
                UIStyle {
                    text_color: if supported {
                        color::TEXT_WHITE_C32
                    } else {
                        color::TEXT_DIMMED_C32
                    },
                    font_size: FONT_SIZE,
                    text_align: TextAlign::Left,
                    ..UIStyle::default()
                },
            );
            // Current-value cell. Editable kinds render as an
            // interactive button (Click/Drag → SetGraphNodeParam);
            // unsupported kinds (Vec/Color) render as a dimmed label
            // since V1 has no editor for them. Outer-driven status
            // doesn't affect editability anymore — the binding apply
            // path skips when the outer slot is unchanged.
            let value_str = format_inner_param_value(ps);
            let value_x = label_x + label_w;
            let value_cell_node_id = if editable {
                let id = tree.add_button(
                    bg_id,
                    value_x,
                    y,
                    value_w,
                    ROW_H,
                    value_cell_style(TextAlign::Right),
                    &value_str,
                );
                Some(id)
            } else {
                tree.add_label(
                    bg_id,
                    value_x,
                    y,
                    value_w,
                    ROW_H,
                    &value_str,
                    UIStyle {
                        text_color: color::TEXT_DIMMED_C32,
                        font_size: FONT_SIZE,
                        text_align: TextAlign::Right,
                        ..UIStyle::default()
                    },
                );
                None
            };

            if supported {
                let convert = match ps.kind {
                    GraphEditorParamKind::Float
                    | GraphEditorParamKind::Angle
                    | GraphEditorParamKind::Frequency => ParamConvert::Float,
                    GraphEditorParamKind::Int => ParamConvert::IntRound,
                    GraphEditorParamKind::Bool => ParamConvert::BoolThreshold,
                    GraphEditorParamKind::Enum => ParamConvert::EnumRound,
                    GraphEditorParamKind::Trigger => ParamConvert::Trigger,
                    // Colour / vector kinds never reach here (`supported` is
                    // false for them — they take the build_vec_param branch),
                    // and `Other` is the disabled fallback. Both unreachable.
                    GraphEditorParamKind::Color
                    | GraphEditorParamKind::Vec2
                    | GraphEditorParamKind::Vec3
                    | GraphEditorParamKind::Vec4
                    | GraphEditorParamKind::String
                    | GraphEditorParamKind::Other => ParamConvert::Float,
                };
                let (min, max) = ps.range.unwrap_or((0.0, 1.0));
                let static_block_slot = self
                    .static_block_targets
                    .get(&(handle.clone(), ps.name.clone()))
                    .copied();
                self.rows.push(RowState::InnerNode {
                    checkbox_node_id: cb_id,
                    value_cell_node_id,
                    node_runtime_id: node.runtime_node_id,
                    node_id: node.node_id.clone(),
                    node_handle: handle.clone(),
                    inner_param: ps.name.clone(),
                    label: ps.label.clone(),
                    kind: ps.kind,
                    min,
                    max,
                    default_value: ps.default_value,
                    current_value: ps.current_value,
                    enum_labels_count: ps.enum_labels.as_ref().map(|l| l.len()).unwrap_or(0),
                    enum_labels: ps.enum_labels.clone().unwrap_or_default(),
                    convert,
                    currently_exposed: is_exposed,
                    static_block_slot,
                    wire_driven: is_wire_driven,
                });
            }

            y += ROW_H;
        }
    }

    /// Build the inline editor for a `Color` / `Vec` param: a header row (a
    /// disabled expose checkbox — colours and vectors aren't single-slot
    /// card-exposable — the label, and for colours a live swatch), then one row
    /// per channel with a draggable value cell. Each channel cell pushes a
    /// [`RowState::VecComponent`] so a drag rebuilds the whole value and emits
    /// one `SetGraphNodeParam`. Returns the y cursor past the widget.
    fn build_vec_param(
        &mut self,
        tree: &mut UITree,
        bg_id: Option<NodeId>,
        viewport: Rect,
        mut y: f32,
        node_runtime_id: u32,
        ps: &GraphEditorParam,
    ) -> f32 {
        let components = ps.kind.vec_components();
        let labels = ps.kind.channel_labels();
        let is_color = matches!(ps.kind, GraphEditorParamKind::Color);
        // Channel scrub range: colours are physical 0..1; vectors take the
        // declared range or a symmetric default covering directions and UVs.
        let (cmin, cmax) = if is_color {
            (0.0, 1.0)
        } else {
            ps.range.unwrap_or((-1.0, 1.0))
        };

        // ── Header row: disabled checkbox + label + (colour) swatch ──
        let cb_x = viewport.x + PADDING;
        let cb_y = y + (ROW_H - CHECKBOX_H) * 0.5;
        tree.add_button(
            bg_id,
            cb_x,
            cb_y,
            CHECKBOX_W,
            CHECKBOX_H,
            checkbox_style(false, false),
            "",
        );
        let label_x = cb_x + CHECKBOX_W + CHECKBOX_GAP;
        let row_remaining = (viewport.x + viewport.width - PADDING - label_x).max(10.0);
        let swatch_w = if is_color { ROW_H } else { 0.0 };
        let label_w = (row_remaining - swatch_w - 6.0).max(10.0);
        tree.add_label(
            bg_id,
            label_x,
            y,
            label_w,
            ROW_H,
            &ps.label,
            UIStyle {
                text_color: color::TEXT_WHITE_C32,
                font_size: FONT_SIZE,
                text_align: TextAlign::Left,
                ..UIStyle::default()
            },
        );
        if is_color {
            let v = ps.vec_value;
            let to_u8 = |c: f32| (c.clamp(0.0, 1.0) * 255.0).round() as u8;
            let sw_x = viewport.x + viewport.width - PADDING - swatch_w;
            tree.add_panel(
                bg_id,
                sw_x,
                y + 3.0,
                swatch_w,
                ROW_H - 6.0,
                UIStyle {
                    bg_color: Color32::new(to_u8(v[0]), to_u8(v[1]), to_u8(v[2]), 255),
                    corner_radius: color::BUTTON_RADIUS,
                    border_color: color::TEXT_DIMMED_C32,
                    border_width: 1.0,
                    ..UIStyle::default()
                },
            );
        }
        y += ROW_H;

        // ── One channel row per component ──
        for (ch, ch_label) in labels.iter().enumerate().take(components) {
            let comp_x = viewport.x + PADDING + CHECKBOX_W + CHECKBOX_GAP;
            tree.add_label(
                bg_id,
                comp_x,
                y,
                16.0,
                ROW_H,
                ch_label,
                UIStyle {
                    text_color: color::TEXT_DIMMED_C32,
                    font_size: FONT_SIZE,
                    text_align: TextAlign::Left,
                    ..UIStyle::default()
                },
            );
            let cell_x = comp_x + 20.0;
            let cell_w = (viewport.x + viewport.width - PADDING - cell_x).max(40.0);
            let cell_id = tree.add_button(
                bg_id,
                cell_x,
                y,
                cell_w,
                ROW_H,
                value_cell_style(TextAlign::Right),
                &format!("{:.3}", ps.vec_value[ch]),
            );
            self.rows.push(RowState::VecComponent {
                value_cell_node_id: cell_id,
                node_runtime_id,
                inner_param: ps.name.clone(),
                kind: ps.kind,
                channel: ch,
                base: ps.vec_value,
                min: cmin,
                max: cmax,
            });
            y += ROW_H;
        }
        y
    }

    /// Build a String param row: a disabled expose checkbox (strings aren't
    /// single-slot card-exposable), the label, the current value, and — for a
    /// path-like param — a Browse button that opens a native folder picker.
    /// Free-text editing of plain strings isn't inline yet (no canvas text
    /// field), so non-path strings are read-only here. Returns the y past the row.
    fn build_string_param(
        &mut self,
        tree: &mut UITree,
        bg_id: Option<NodeId>,
        viewport: Rect,
        y: f32,
        node_runtime_id: u32,
        ps: &GraphEditorParam,
    ) -> f32 {
        let cb_x = viewport.x + PADDING;
        let cb_y = y + (ROW_H - CHECKBOX_H) * 0.5;
        tree.add_button(
            bg_id,
            cb_x,
            cb_y,
            CHECKBOX_W,
            CHECKBOX_H,
            checkbox_style(false, false),
            "",
        );
        let label_x = cb_x + CHECKBOX_W + CHECKBOX_GAP;
        let row_remaining = (viewport.x + viewport.width - PADDING - label_x).max(10.0);
        let is_path = is_path_param(&ps.name);
        // Reserve a Browse button at the right for path params.
        let browse_w = if is_path { 64.0 } else { 0.0 };
        let value_w = (row_remaining * 0.5).max(60.0);
        let label_w = (row_remaining - value_w - browse_w - 6.0).max(10.0);
        tree.add_label(
            bg_id,
            label_x,
            y,
            label_w,
            ROW_H,
            &ps.label,
            UIStyle {
                text_color: color::TEXT_WHITE_C32,
                font_size: FONT_SIZE,
                text_align: TextAlign::Left,
                ..UIStyle::default()
            },
        );
        let value_str = ps.summary.clone().unwrap_or_else(|| "—".to_string());
        let value_x = label_x + label_w;
        if is_path {
            // Read-only display; the Browse button drives the value.
            tree.add_label(
                bg_id,
                value_x,
                y,
                value_w,
                ROW_H,
                &value_str,
                UIStyle {
                    text_color: color::TEXT_DIMMED_C32,
                    font_size: FONT_SIZE,
                    text_align: TextAlign::Right,
                    ..UIStyle::default()
                },
            );
        } else {
            // Free-text: the value cell is itself a click target that opens the
            // inline editor.
            let cell_id = tree.add_button(
                bg_id,
                value_x,
                y,
                value_w,
                ROW_H,
                value_cell_style(TextAlign::Right),
                &value_str,
            );
            // Anchor the editor across the whole row (the value cell alone is
            // too narrow to type a sentence into).
            let editor_x = viewport.x + PADDING;
            let editor_w = (viewport.x + viewport.width - PADDING - editor_x).max(60.0);
            self.rows.push(RowState::EditTextButton {
                button_node_id: cell_id,
                node_runtime_id,
                param_name: ps.name.clone(),
                current: ps.string_value.clone().unwrap_or_default(),
                rect: (editor_x, y, editor_w, ROW_H),
            });
        }
        if is_path {
            let btn_id = tree.add_button(
                bg_id,
                viewport.x + viewport.width - PADDING - browse_w,
                y,
                browse_w,
                ROW_H,
                value_cell_style(TextAlign::Center),
                "Browse",
            );
            self.rows.push(RowState::BrowseButton {
                button_node_id: btn_id,
                node_runtime_id,
                param_name: ps.name.clone(),
            });
        }
        y + ROW_H
    }

    /// Render a `Table` param as an inline grid of clickable numeric cells —
    /// the gradient-stop / numeric-sequence editor. A header line carries the
    /// label + N×M shape, then one row of `cell_w`-wide cells per table row.
    /// Each cell click opens the inline numeric editor over it. Returns the new
    /// `y` below the grid. Columns are addressed by index (the table carries no
    /// per-column semantics); a richer swatch/draggable presentation is a later
    /// visual pass.
    #[allow(clippy::too_many_arguments)]
    fn build_table_param(
        &mut self,
        tree: &mut UITree,
        bg_id: Option<NodeId>,
        viewport: Rect,
        y: f32,
        node_runtime_id: u32,
        ps: &GraphEditorParam,
        rows: &[Vec<f32>],
    ) -> f32 {
        const CELL_H: f32 = 22.0;
        const CELL_GAP: f32 = 3.0;
        let cols = rows.iter().map(|r| r.len()).max().unwrap_or(0);
        let header = format!("{}  ({}×{})", ps.label, rows.len(), cols);
        tree.add_label(
            bg_id,
            viewport.x + PADDING,
            y,
            (viewport.width - 2.0 * PADDING).max(10.0),
            ROW_H,
            &header,
            UIStyle {
                text_color: color::TEXT_WHITE_C32,
                font_size: FONT_SIZE,
                text_align: TextAlign::Left,
                ..UIStyle::default()
            },
        );
        let mut yy = y + ROW_H;
        if cols == 0 {
            return yy;
        }
        let grid_x = viewport.x + PADDING;
        let grid_w = (viewport.width - 2.0 * PADDING).max(40.0);
        let cell_w = ((grid_w - CELL_GAP * (cols as f32 - 1.0)) / cols as f32).max(24.0);
        for (r, rowvals) in rows.iter().enumerate() {
            let mut cx = grid_x;
            for (c, &v) in rowvals.iter().enumerate() {
                let cell_id = tree.add_button(
                    bg_id,
                    cx,
                    yy,
                    cell_w,
                    CELL_H,
                    value_cell_style(TextAlign::Center),
                    &fmt_table_cell(v),
                );
                self.rows.push(RowState::TableCell {
                    button_node_id: cell_id,
                    node_runtime_id,
                    param_name: ps.name.clone(),
                    row: r,
                    col: c,
                    rect: (cx, yy, cell_w, CELL_H),
                });
                cx += cell_w + CELL_GAP;
            }
            yy += CELL_H + CELL_GAP;
        }
        yy + 4.0
    }

    /// Render the value inspector for the previewed node into the pinned
    /// node-output pane (`region`) when the node carries no image — its title,
    /// one-line "what it does", then the live OUTPUT signal and INPUT values.
    /// Returns `true` when it drew (a non-image node is selected), so the host
    /// knows the pane is occupied by text rather than an image and skips the
    /// generic "Node Output" title. No row state — nothing here is clickable.
    pub fn render_node_inspector(&self, tree: &mut UITree, region: Rect) -> bool {
        let Some(insp) = self.node_inspector.as_ref() else {
            return false;
        };
        Self::render_inspector_block(tree, None, region, insp);
        true
    }

    /// Draw the inspector block — title, description, OUTPUT/INPUT rows — into
    /// `region`, parented at `parent_id`. Coordinates are absolute; `region.x`
    /// is the left edge of the text (already padded by the caller).
    fn render_inspector_block(
        tree: &mut UITree,
        parent_id: Option<NodeId>,
        region: Rect,
        insp: &NodeInspector,
    ) {
        const DESC_LINE_H: f32 = 16.0;
        const IO_ROW_H: f32 = 20.0;
        let x = region.x;
        let w = region.width.max(10.0);
        let bg_id = parent_id;
        let mut y = region.y;

        // Title.
        tree.add_label(
            bg_id,
            x,
            y,
            w,
            HEADER_H - PADDING,
            &insp.title,
            UIStyle {
                text_color: color::TEXT_WHITE_C32,
                font_size: HEADER_FONT_SIZE,
                text_align: TextAlign::Left,
                ..UIStyle::default()
            },
        );
        y += HEADER_H;

        // "What it does", wrapped, capped at 4 lines.
        if !insp.description.is_empty() {
            let max_chars = ((w / 6.5).floor() as usize).max(8);
            for line in crate::graph_canvas::wrap_text(&insp.description, max_chars)
                .into_iter()
                .take(4)
            {
                tree.add_label(
                    bg_id,
                    x,
                    y,
                    w,
                    DESC_LINE_H,
                    &line,
                    UIStyle {
                        text_color: color::TEXT_DIMMED_C32,
                        font_size: FONT_SIZE,
                        text_align: TextAlign::Left,
                        ..UIStyle::default()
                    },
                );
                y += DESC_LINE_H;
            }
        }
        y += PADDING * 0.5;

        // OUTPUT then INPUT value sections. Output first — it's the live signal.
        for (heading, rows) in [("OUTPUT", &insp.outputs), ("INPUT", &insp.inputs)] {
            if rows.is_empty() {
                continue;
            }
            tree.add_label(
                bg_id,
                x,
                y,
                w,
                IO_ROW_H,
                heading,
                UIStyle {
                    text_color: color::TEXT_DIMMED_C32,
                    font_size: FONT_SIZE,
                    text_align: TextAlign::Left,
                    ..UIStyle::default()
                },
            );
            y += IO_ROW_H;
            for (name, value) in rows {
                let value_w = (w * 0.4).max(50.0);
                let name_w = (w - value_w).max(10.0);
                tree.add_label(
                    bg_id,
                    x,
                    y,
                    name_w,
                    IO_ROW_H,
                    name,
                    UIStyle {
                        text_color: color::TEXT_WHITE_C32,
                        font_size: FONT_SIZE,
                        text_align: TextAlign::Left,
                        ..UIStyle::default()
                    },
                );
                tree.add_label(
                    bg_id,
                    x + name_w,
                    y,
                    value_w,
                    IO_ROW_H,
                    &fmt_value(*value),
                    UIStyle {
                        text_color: color::TEXT_WHITE_C32,
                        font_size: FONT_SIZE,
                        text_align: TextAlign::Right,
                        ..UIStyle::default()
                    },
                );
                y += IO_ROW_H;
            }
            y += PADDING * 0.5;
        }
    }

    /// Register this panel's discrete-click intents into `reg` (UI Architecture
    /// Overhaul Phase 6.2). Each clickable row maps its tree node to the
    /// `GraphEditCommand` it fires; the app resolves clicks through the shared
    /// [`IntentRegistry`] — now generic over the action type (6.1) — folding the
    /// sidebar onto the same dispatch every chrome panel uses, replacing the old
    /// per-row `handle_click_event` id-matching loop.
    ///
    /// Call once per frame that carries events, after [`Self::build`]: the row
    /// table holds the node ids from the last build, which match this frame's
    /// events. Drags are NOT registered — they stay in [`Self::handle_event`].
    ///
    /// No `effect_index` guard, on purpose: post-unification the editor is one
    /// surface for both Effect- and Generator-hosted graphs (generators have no
    /// `effect_index`). An empty `self.rows` (nothing selected) registers
    /// nothing; the app-side `ToggleNodeParamExpose` handler gates on the watched
    /// target so no command fires without one.
    ///
    /// Click semantics by row:
    /// - Inner-param checkbox → `ToggleNodeParamExpose` (expose flips). One
    ///   command whether or not `(handle, param)` maps to a static-block slot —
    ///   the content-thread command dispatches internally.
    /// - Inner-param value cell → `SetGraphNodeParam` for Bool/Enum/Trigger;
    ///   Float/Int cells edit by drag, so they register no click intent.
    /// - Wire-driven rows register nothing — the wire wins each frame, so both
    ///   the checkbox and the value cell are dead (the old short-circuit).
    /// - Browse / EditText / Wgsl / Table-cell buttons → their open-editor command.
    /// - Preview-normalize toggle → `SetNodePreviewNormalize` (flipped).
    pub fn register_intents(&self, reg: &mut IntentRegistry<GraphEditCommand>) {
        for row in &self.rows {
            match row {
                RowState::PreviewNormalizeToggle { button_node_id } => {
                    reg.on(
                        *button_node_id,
                        Gesture::Click,
                        GraphEditCommand::SetNodePreviewNormalize(!self.normalize_preview),
                    );
                }
                RowState::InnerNode {
                    checkbox_node_id,
                    value_cell_node_id,
                    node_runtime_id,
                    node_id: row_node_id,
                    node_handle,
                    inner_param,
                    label,
                    kind,
                    min,
                    max,
                    default_value,
                    current_value,
                    enum_labels_count,
                    enum_labels,
                    convert,
                    currently_exposed,
                    static_block_slot,
                    wire_driven,
                } => {
                    // Wire-driven rows: clicks on either target are dead — the
                    // wire wins every frame, so neither the checkbox nor the
                    // value cell registers an intent.
                    if *wire_driven {
                        continue;
                    }
                    // Checkbox → unified expose toggle.
                    let _ = static_block_slot;
                    reg.on(
                        *checkbox_node_id,
                        Gesture::Click,
                        GraphEditCommand::ToggleNodeParamExpose {
                            node_id: row_node_id.clone(),
                            node_u32_id: *node_runtime_id,
                            node_handle: node_handle.clone(),
                            inner_param: inner_param.clone(),
                            expose: !currently_exposed,
                            label: label.clone(),
                            min: *min,
                            max: *max,
                            default_value: *default_value,
                            convert: *convert,
                            is_angle: matches!(*kind, GraphEditorParamKind::Angle),
                            value_labels: enum_labels.clone(),
                        },
                    );
                    // Value cell → SetGraphNodeParam for the click-editable kinds
                    // (Bool toggle, Enum cycle, Trigger fire). Float/Int return
                    // None and edit by drag, so they register no click intent.
                    if let Some(cell) = value_cell_node_id
                        && let Some(new_value) =
                            value_cell_click_to_param(*kind, *current_value, *enum_labels_count)
                    {
                        reg.on(
                            *cell,
                            Gesture::Click,
                            GraphEditCommand::SetGraphNodeParam {
                                node_id: *node_runtime_id,
                                param_name: inner_param.clone(),
                                new_value,
                            },
                        );
                    }
                }
                // Colour / vector channel cells edit by drag only — no click intent.
                RowState::VecComponent { .. } => {}
                RowState::BrowseButton {
                    button_node_id,
                    node_runtime_id,
                    param_name,
                } => {
                    reg.on(
                        *button_node_id,
                        Gesture::Click,
                        GraphEditCommand::BrowseGraphNodePath {
                            node_id: *node_runtime_id,
                            param_name: param_name.clone(),
                        },
                    );
                }
                RowState::EditTextButton {
                    button_node_id,
                    node_runtime_id,
                    param_name,
                    current,
                    rect,
                } => {
                    reg.on(
                        *button_node_id,
                        Gesture::Click,
                        GraphEditCommand::EditGraphNodeStringParam {
                            node_id: *node_runtime_id,
                            param_name: param_name.clone(),
                            current: current.clone(),
                            anchor: *rect,
                        },
                    );
                }
                RowState::WgslButton {
                    button_node_id,
                    node_runtime_id,
                    source,
                } => {
                    reg.on(
                        *button_node_id,
                        Gesture::Click,
                        GraphEditCommand::EditGraphNodeWgsl {
                            node_id: *node_runtime_id,
                            current: source.clone(),
                            anchor: (0.0, 0.0, 0.0, 0.0),
                        },
                    );
                }
                RowState::TableCell {
                    button_node_id,
                    node_runtime_id,
                    param_name,
                    row,
                    col,
                    rect,
                } => {
                    // Look up the full table off the selected node so the host can
                    // rebuild just the edited cell on commit.
                    if let Some(rows) = self
                        .selected_node
                        .as_ref()
                        .and_then(|n| n.parameters.iter().find(|p| &p.name == param_name))
                        .and_then(|p| p.table_value.clone())
                    {
                        let current = rows
                            .get(*row)
                            .and_then(|r| r.get(*col))
                            .copied()
                            .unwrap_or(0.0);
                        reg.on(
                            *button_node_id,
                            Gesture::Click,
                            GraphEditCommand::EditGraphNodeTableCell {
                                node_id: *node_runtime_id,
                                param_name: param_name.clone(),
                                row: *row,
                                col: *col,
                                current,
                                rows,
                                anchor: *rect,
                            },
                        );
                    }
                }
            }
        }
    }

    /// Stateful pointer-drag dispatch — the half of the old `handle_event` that
    /// can't be a build-time intent. `DragBegin` on a Float/Int (or colour /
    /// vector channel) value cell captures the scrub anchor; each `Drag` scales
    /// the cumulative pixel delta over `DRAG_FULL_RANGE_PX` and emits one
    /// `SetGraphNodeParam`; `DragEnd` clears the anchor. Discrete clicks are
    /// handled via [`Self::register_intents`].
    pub fn handle_event(&mut self, event: &UIEvent) -> Vec<GraphEditCommand> {
        match event {
            UIEvent::DragBegin {
                node_id, origin, ..
            } => self.handle_drag_begin(*node_id, origin.x),
            UIEvent::Drag { node_id, pos, .. } => self.handle_drag(*node_id, pos.x),
            UIEvent::DragEnd { node_id, .. } => {
                // End the scrub when this drag ends. If the cell node left the
                // build mid-drag (`node_id == None`), end it anyway — a drag that
                // began must end, and the anchor must not stick.
                if let Some(drag) = self.drag
                    && node_id.is_none_or(|n| drag.value_cell_node_id == n)
                {
                    self.drag = None;
                }
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    fn handle_drag_begin(&mut self, node_id: NodeId, origin_x: f32) -> Vec<GraphEditCommand> {
        // Numeric-value-cell drag opens a scrub anchor. Bool / Enum
        // edits happen on click, so drag on them is a no-op. Wire-
        // driven rows are also a no-op: the wire wins each frame,
        // so a scrub would be silently overwritten.
        for row in &self.rows {
            match row {
                RowState::InnerNode {
                    value_cell_node_id: Some(cell),
                    node_runtime_id,
                    kind,
                    min,
                    max,
                    current_value,
                    wire_driven,
                    ..
                } if *cell == node_id
                    && !*wire_driven
                    && matches!(
                        kind,
                        GraphEditorParamKind::Float
                            | GraphEditorParamKind::Angle
                            | GraphEditorParamKind::Frequency
                            | GraphEditorParamKind::Int
                    ) =>
                {
                    self.drag = Some(DragState {
                        value_cell_node_id: node_id,
                        node_runtime_id: *node_runtime_id,
                        kind: *kind,
                        range: (*min, *max),
                        start_value: *current_value,
                        press_origin_x: origin_x,
                        vec: None,
                    });
                    return Vec::new();
                }
                // One channel of a colour / vector editor: anchor on the channel's
                // current value and carry the full base so each Drag rebuilds the
                // whole value.
                RowState::VecComponent {
                    value_cell_node_id,
                    node_runtime_id,
                    kind,
                    channel,
                    base,
                    min,
                    max,
                    ..
                } if *value_cell_node_id == node_id => {
                    self.drag = Some(DragState {
                        value_cell_node_id: node_id,
                        node_runtime_id: *node_runtime_id,
                        kind: *kind,
                        range: (*min, *max),
                        start_value: base[*channel],
                        press_origin_x: origin_x,
                        vec: Some(VecDrag {
                            kind: *kind,
                            channel: *channel,
                            base: *base,
                        }),
                    });
                    return Vec::new();
                }
                _ => {}
            }
        }
        Vec::new()
    }

    fn handle_drag(&mut self, node_id: NodeId, pos_x: f32) -> Vec<GraphEditCommand> {
        let Some(drag) = self.drag else {
            return Vec::new();
        };
        if drag.value_cell_node_id != node_id {
            return Vec::new();
        }
        // Cumulative pixel-delta from the press anchor → value-delta
        // over `DRAG_FULL_RANGE_PX`. We anchor on `start_value` so
        // hand-drift across a long drag doesn't accumulate
        // floating-point error.
        let (min, max) = drag.range;
        let range_span = (max - min).max(f32::EPSILON);
        let delta_px = pos_x - drag.press_origin_x;
        let delta_value = delta_px * (range_span / DRAG_FULL_RANGE_PX);
        let mut new_v = (drag.start_value + delta_value).clamp(min, max);
        if matches!(drag.kind, GraphEditorParamKind::Int) {
            new_v = new_v.round();
        }

        // Colour / vector channel scrub: overwrite the dragged component in the
        // base value and emit the whole colour/vector as one edit, so the other
        // channels are carried through unchanged.
        if let Some(vd) = drag.vec {
            let mut full = vd.base;
            full[vd.channel] = new_v;
            let serialized = match vd.kind {
                GraphEditorParamKind::Color => SerializedParamValue::Color { value: full },
                GraphEditorParamKind::Vec4 => SerializedParamValue::Vec4 { value: full },
                GraphEditorParamKind::Vec3 => SerializedParamValue::Vec3 {
                    value: [full[0], full[1], full[2]],
                },
                GraphEditorParamKind::Vec2 => SerializedParamValue::Vec2 {
                    value: [full[0], full[1]],
                },
                _ => return Vec::new(),
            };
            let param_name = self.rows.iter().find_map(|r| match r {
                RowState::VecComponent {
                    value_cell_node_id,
                    inner_param,
                    ..
                } if *value_cell_node_id == node_id => Some(inner_param.clone()),
                _ => None,
            });
            let Some(param_name) = param_name else {
                return Vec::new();
            };
            return vec![GraphEditCommand::SetGraphNodeParam {
                node_id: drag.node_runtime_id,
                param_name,
                new_value: serialized,
            }];
        }

        // Numeric storage is `Float`-only now (Int collapsed). The `Int`
        // presentation hint still drives the rounding above; we just emit
        // the rounded value as a Float scalar.
        let serialized = match drag.kind {
            GraphEditorParamKind::Float
            | GraphEditorParamKind::Angle
            | GraphEditorParamKind::Frequency
            | GraphEditorParamKind::Int => SerializedParamValue::Float { value: new_v },
            _ => return Vec::new(),
        };
        // Look up the param name via the row table. The row layout
        // doesn't move mid-drag (the editor rebuilds the tree each
        // frame, but `build` preserves drag-state and re-emits the
        // same value-cell ids by the same shape).
        let inner_param = self.rows.iter().find_map(|r| match r {
            RowState::InnerNode {
                value_cell_node_id: Some(v),
                inner_param,
                ..
            } if *v == node_id => Some(inner_param.clone()),
            _ => None,
        });
        let Some(param_name) = inner_param else {
            return Vec::new();
        };
        vec![GraphEditCommand::SetGraphNodeParam {
            node_id: drag.node_runtime_id,
            param_name,
            new_value: serialized,
        }]
    }

}

/// Translate a click on an inner-param value cell into a
/// `SerializedParamValue` for the resulting `SetGraphNodeParam` —
/// `None` when click on this kind shouldn't emit anything (Float/Int
/// edits happen via drag, not click).
///
/// - **Bool** → toggled bool.
/// - **Enum** → `(current + 1) mod enum_count`. Empty `enum_count`
///   (zero) is a defensive no-op — should not occur for properly-
///   declared params but isn't worth panicking over.
/// - **Float / Int / Other** → `None`.
fn value_cell_click_to_param(
    kind: GraphEditorParamKind,
    current_value: f32,
    enum_count: usize,
) -> Option<SerializedParamValue> {
    match kind {
        GraphEditorParamKind::Bool => Some(SerializedParamValue::Bool {
            value: current_value < 0.5,
        }),
        GraphEditorParamKind::Enum => {
            if enum_count == 0 {
                return None;
            }
            let current = current_value.round() as i32;
            let next = (current + 1).rem_euclid(enum_count as i32);
            Some(SerializedParamValue::Enum { value: next as u32 })
        }
        GraphEditorParamKind::Trigger => Some(SerializedParamValue::Float {
            value: current_value + 1.0,
        }),
        // Float/Int edit by drag; colour/vector channels edit by drag in their
        // own row; Other has no editor. None of these emit on a click.
        GraphEditorParamKind::Float
        | GraphEditorParamKind::Angle
        | GraphEditorParamKind::Frequency
        | GraphEditorParamKind::Int
        | GraphEditorParamKind::Color
        | GraphEditorParamKind::Vec2
        | GraphEditorParamKind::Vec3
        | GraphEditorParamKind::Vec4
        | GraphEditorParamKind::String
        | GraphEditorParamKind::Other => None,
    }
}

/// Format the current value of an inner-node parameter for display
/// in the card lane. Enums resolve to their label (e.g., "FoldX"),
/// bools to "true"/"false", numerics to a short fixed-point form, and
/// `summary`-bearing params (Tables) render their summary string.
fn format_inner_param_value(p: &GraphEditorParam) -> String {
    if let Some(summary) = &p.summary {
        return summary.clone();
    }
    match p.kind {
        GraphEditorParamKind::Enum => p
            .enum_labels
            .as_ref()
            .and_then(|labels| labels.get(p.current_value as usize).cloned())
            .unwrap_or_else(|| format!("{}", p.current_value as i64)),
        GraphEditorParamKind::Bool => {
            if p.current_value >= 0.5 {
                "true".to_string()
            } else {
                "false".to_string()
            }
        }
        GraphEditorParamKind::Int => format!("{}", p.current_value as i64),
        GraphEditorParamKind::Float => format!("{:.2}", p.current_value),
        // Stored value is radians; the user always sees and edits degrees.
        GraphEditorParamKind::Angle => format!("{:.0}°", p.current_value.to_degrees()),
        // Stored value is rad/s; the user always sees and edits Hz.
        GraphEditorParamKind::Frequency => {
            format!("{:.2} Hz", p.current_value / std::f32::consts::TAU)
        }
        GraphEditorParamKind::Trigger => "▶ Fire".to_string(),
        // Colour / vector params render via the dedicated build_vec_param editor,
        // not this single-cell formatter; these arms are for completeness.
        GraphEditorParamKind::Color => format!(
            "#{:02X}{:02X}{:02X}",
            (p.vec_value[0].clamp(0.0, 1.0) * 255.0).round() as u8,
            (p.vec_value[1].clamp(0.0, 1.0) * 255.0).round() as u8,
            (p.vec_value[2].clamp(0.0, 1.0) * 255.0).round() as u8,
        ),
        GraphEditorParamKind::Vec2 => {
            format!("{:.2}, {:.2}", p.vec_value[0], p.vec_value[1])
        }
        GraphEditorParamKind::Vec3 => format!(
            "{:.2}, {:.2}, {:.2}",
            p.vec_value[0], p.vec_value[1], p.vec_value[2]
        ),
        GraphEditorParamKind::Vec4 => format!(
            "{:.2}, {:.2}, {:.2}, {:.2}",
            p.vec_value[0], p.vec_value[1], p.vec_value[2], p.vec_value[3]
        ),
        // String renders via build_string_param; this arm is for completeness.
        GraphEditorParamKind::String => p.summary.clone().unwrap_or_else(|| "—".to_string()),
        GraphEditorParamKind::Other => "—".to_string(),
    }
}


/// Format a live scalar value compactly: integers without decimals, otherwise
/// up to 3 places with trailing zeros trimmed.
fn fmt_value(v: f32) -> String {
    if v.is_finite() && (v - v.round()).abs() < 1e-4 && v.abs() < 1e6 {
        format!("{:.0}", v)
    } else {
        crate::fmt::fmt_trimmed(v, 3)
    }
}

fn checkbox_style(checked: bool, supported: bool) -> UIStyle {
    // The unchecked state needs a bg that's visibly distinct from the
    // panel bg behind it (which is `EFFECT_CARD_INNER_BG_C32`), or the
    // checkbox disappears into the panel and the user has to guess
    // where to click. Use the standard inactive-button gray.
    let bg_color = match (checked, supported) {
        (true, true) => color::ACCENT_BLUE_C32,
        (false, true) => color::BUTTON_INACTIVE_C32,
        (_, false) => color::BUTTON_INACTIVE_C32,
    };
    let mut style = UIStyle {
        bg_color,
        text_color: color::TEXT_WHITE_C32,
        font_size: HEADER_FONT_SIZE,
        text_align: TextAlign::Center,
        corner_radius: color::BUTTON_RADIUS,
        // Brighter border than CARD_BORDER_C32 so the checkbox edge
        // reads against the inactive-button gray.
        border_color: color::TEXT_DIMMED_C32,
        border_width: 1.0,
        ..UIStyle::default()
    };
    if !supported {
        // Suppress hover by removing INTERACTIVE flag at the call site;
        // here we only style the visual.
        style.text_color = color::TEXT_DIMMED_C32;
    }
    style
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap_node_with_params(handle: Option<&str>) -> GraphEditorNodeView {
        GraphEditorNodeView {
            runtime_node_id: 42,
            node_id: handle.map(GraphNodeId::new).unwrap_or_default(),
            node_handle: handle.map(|h| h.to_string()),
            title: "UV Transform".to_string(),
            parameters: vec![
                GraphEditorParam {
                    name: "translate".to_string(),
                    label: "Translate".to_string(),
                    kind: GraphEditorParamKind::Float,
                    default_value: 0.0,
                    current_value: 0.0,
                    range: Some((-1.0, 1.0)),
                    enum_labels: None,
                    summary: None,
                    vec_value: [0.0; 4],
                    string_value: None,
                    table_value: None,
                },
                GraphEditorParam {
                    name: "scale".to_string(),
                    label: "Scale".to_string(),
                    kind: GraphEditorParamKind::Float,
                    default_value: 1.0,
                    current_value: 1.0,
                    range: Some((0.0, 4.0)),
                    enum_labels: None,
                    summary: None,
                    vec_value: [0.0; 4],
                    string_value: None,
                    table_value: None,
                },
                GraphEditorParam {
                    name: "color".to_string(),
                    label: "Color".to_string(),
                    kind: GraphEditorParamKind::Other, // disabled — multi-component
                    default_value: 0.0,
                    current_value: 0.0,
                    range: None,
                    enum_labels: None,
                    summary: None,
                    vec_value: [0.0; 4],
                    string_value: None,
                    table_value: None,
                },
            ],
            wgsl_source: None,
        }
    }

    fn viewport() -> Rect {
        Rect::new(0.0, 0.0, SIDEBAR_WIDTH, 600.0)
    }

    /// Helper: pull the inner-param name from a row. Since the top
    /// "Effect Parameters" list became read-only labels (no row
    /// state), every entry in `panel.rows` is an `InnerNode` now.
    fn inner_param_of(row: &RowState) -> &str {
        let RowState::InnerNode { inner_param, .. } = row else {
            panic!("expected an InnerNode row, got {row:?}");
        };
        inner_param.as_str()
    }

    fn checkbox_id_of(row: &RowState) -> NodeId {
        let RowState::InnerNode {
            checkbox_node_id, ..
        } = row
        else {
            panic!("expected an InnerNode row, got {row:?}");
        };
        *checkbox_node_id
    }

    /// Inner-node rows only — skips the always-present preview-toggle row.
    fn inner_rows(panel: &GraphEditorPanel) -> Vec<&RowState> {
        panel
            .rows
            .iter()
            .filter(|r| matches!(r, RowState::InnerNode { .. }))
            .collect()
    }

    /// Drive a discrete click through the real runtime path (Phase 6.2): build
    /// the panel's intent registry, then resolve the hit node through it with the
    /// same fold-up the app uses. Returns the resolved `GraphEditCommand` (zero
    /// or one), keeping the old `Vec` shape so existing assertions stand.
    fn click(panel: &GraphEditorPanel, tree: &UITree, id: NodeId) -> Vec<GraphEditCommand> {
        let mut reg = IntentRegistry::<GraphEditCommand>::new();
        panel.register_intents(&mut reg);
        reg.resolve(tree, Some(id), Gesture::Click)
            .into_iter()
            .collect()
    }

    #[test]
    fn build_renders_rows_for_supported_params_only() {
        let mut tree = UITree::new();
        let mut panel = GraphEditorPanel::new();
        let node = snap_node_with_params(Some("uv_transform"));
        panel.configure(
            Some(0),
            Some(&node),
            HashSet::new(),
            HashMap::new(),
            HashMap::new(),
            HashSet::new(),
        );
        panel.build(&mut tree, viewport());
        // 2 supported params → 2 inner-node rows tracked. The Color row
        // exists visually but isn't clickable.
        let inner_rows: Vec<&RowState> = panel
            .rows
            .iter()
            .filter(|r| matches!(r, RowState::InnerNode { .. }))
            .collect();
        assert_eq!(inner_rows.len(), 2);
        assert_eq!(inner_param_of(inner_rows[0]), "translate");
        assert_eq!(inner_param_of(inner_rows[1]), "scale");
    }

    #[test]
    fn build_handles_no_selection_with_empty_state() {
        let mut tree = UITree::new();
        let mut panel = GraphEditorPanel::new();
        panel.configure(
            Some(0),
            None,
            HashSet::new(),
            HashMap::new(),
            HashMap::new(),
            HashSet::new(),
        );
        panel.build(&mut tree, viewport());
        // Only the always-present preview-toggle row; no inner-node rows.
        assert!(inner_rows(&panel).is_empty());
    }

    #[test]
    fn build_handles_anonymous_node_with_empty_state() {
        let mut tree = UITree::new();
        let mut panel = GraphEditorPanel::new();
        let node = snap_node_with_params(None); // no handle
        panel.configure(
            Some(0),
            Some(&node),
            HashSet::new(),
            HashMap::new(),
            HashMap::new(),
            HashSet::new(),
        );
        panel.build(&mut tree, viewport());
        assert!(
            inner_rows(&panel).is_empty(),
            "anonymous nodes don't expose user-exposable params"
        );
    }

    fn preview_toggle_id(panel: &GraphEditorPanel) -> NodeId {
        panel
            .rows
            .iter()
            .find_map(|r| match r {
                RowState::PreviewNormalizeToggle { button_node_id } => Some(*button_node_id),
                _ => None,
            })
            .expect("preview-toggle row present")
    }

    #[test]
    fn preview_toggle_click_emits_flipped_normalize() {
        let mut tree = UITree::new();
        let mut panel = GraphEditorPanel::new();
        // On by default → clicking requests off. The toggle is present even
        // with no node selected (it's a persistent preview preference).
        panel.set_node_preview_normalize(true);
        panel.configure(
            Some(0),
            None,
            HashSet::new(),
            HashMap::new(),
            HashMap::new(),
            HashSet::new(),
        );
        panel.build(&mut tree, viewport());
        let id = preview_toggle_id(&panel);
        let actions = click(&panel, &tree, id);
        assert!(
            matches!(
                actions.as_slice(),
                [GraphEditCommand::SetNodePreviewNormalize(false)]
            ),
            "clicking the on toggle must request off, got {actions:?}"
        );

        // Off → clicking requests on.
        panel.set_node_preview_normalize(false);
        panel.build(&mut tree, viewport());
        let id = preview_toggle_id(&panel);
        let actions = click(&panel, &tree, id);
        assert!(
            matches!(
                actions.as_slice(),
                [GraphEditCommand::SetNodePreviewNormalize(true)]
            ),
            "clicking the off toggle must request on, got {actions:?}"
        );
    }

    #[test]
    fn click_on_unchecked_emits_expose_true() {
        let mut tree = UITree::new();
        let mut panel = GraphEditorPanel::new();
        let node = snap_node_with_params(Some("uv_transform"));
        panel.configure(
            Some(0),
            Some(&node),
            HashSet::new(),
            HashMap::new(),
            HashMap::new(),
            HashSet::new(),
        );
        panel.build(&mut tree, viewport());

        let inner_rows: Vec<&RowState> = panel
            .rows
            .iter()
            .filter(|r| matches!(r, RowState::InnerNode { .. }))
            .collect();
        let translate_cb = checkbox_id_of(inner_rows[0]);
        let actions = click(&panel, &tree,translate_cb);
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            GraphEditCommand::ToggleNodeParamExpose {
                node_id,
                node_handle,
                inner_param,
                expose,
                label,
                min,
                max,
                default_value,
                convert,
                is_angle,
                ..
            } => {
                assert_eq!(node_id, "uv_transform");
                assert_eq!(node_handle, "uv_transform");
                assert_eq!(inner_param, "translate");
                assert!(*expose);
                assert_eq!(label, "Translate");
                assert!((*min - -1.0).abs() < f32::EPSILON);
                assert!((*max - 1.0).abs() < f32::EPSILON);
                assert!((*default_value - 0.0).abs() < f32::EPSILON);
                assert!(matches!(convert, ParamConvert::Float));
                assert!(!*is_angle);
            }
            other => panic!("unexpected action: {other:?}"),
        }
    }

    #[test]
    fn click_on_checked_emits_expose_false() {
        let mut tree = UITree::new();
        let mut panel = GraphEditorPanel::new();
        let node = snap_node_with_params(Some("uv_transform"));
        let mut exposed = HashSet::new();
        exposed.insert(("uv_transform".to_string(), "translate".to_string()));
        panel.configure(
            Some(0),
            Some(&node),
            exposed,
            HashMap::new(),
            HashMap::new(),
            HashSet::new(),
        );
        panel.build(&mut tree, viewport());

        let inner_rows: Vec<&RowState> = panel
            .rows
            .iter()
            .filter(|r| matches!(r, RowState::InnerNode { .. }))
            .collect();
        let translate_cb = checkbox_id_of(inner_rows[0]);
        let actions = click(&panel, &tree,translate_cb);
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            GraphEditCommand::ToggleNodeParamExpose { expose, .. } => {
                assert!(!expose, "click on checked checkbox emits expose: false");
            }
            other => panic!("unexpected action: {other:?}"),
        }
    }

    /// The Phase 6.2 contract, asserted directly on `register_intents` rather
    /// than through the `click` helper: a Float param's checkbox registers a
    /// `ToggleNodeParamExpose` Click intent, but its value cell registers *none*
    /// (Float edits by drag), and an unrelated node folds up to nothing.
    #[test]
    fn register_intents_wires_checkbox_but_not_float_value_cell() {
        let mut tree = UITree::new();
        let mut panel = GraphEditorPanel::new();
        let node = snap_node_with_params(Some("uv_transform"));
        panel.configure(
            Some(0),
            Some(&node),
            HashSet::new(),
            HashMap::new(),
            HashMap::new(),
            HashSet::new(),
        );
        panel.build(&mut tree, viewport());

        let mut reg = IntentRegistry::<GraphEditCommand>::new();
        panel.register_intents(&mut reg);

        let row = inner_rows(&panel)[0]; // "translate", a Float param
        let cb = checkbox_id_of(row);
        let cell = value_cell_id_of(row).expect("Float row has an editable value cell");

        assert!(
            matches!(
                reg.resolve(&tree, Some(cb), Gesture::Click),
                Some(GraphEditCommand::ToggleNodeParamExpose { .. })
            ),
            "checkbox click must resolve to a ToggleNodeParamExpose"
        );
        assert!(
            reg.resolve(&tree, Some(cell), Gesture::Click).is_none(),
            "a Float value cell edits by drag — it must register no click intent"
        );
        assert!(
            reg.resolve(&tree, Some(NodeId::PLACEHOLDER), Gesture::Click)
                .is_none(),
            "an unrelated node folds up to nothing"
        );
    }

    #[test]
    fn click_outside_panel_returns_empty() {
        let mut tree = UITree::new();
        let mut panel = GraphEditorPanel::new();
        let node = snap_node_with_params(Some("uv_transform"));
        panel.configure(
            Some(0),
            Some(&node),
            HashSet::new(),
            HashMap::new(),
            HashMap::new(),
            HashSet::new(),
        );
        panel.build(&mut tree, viewport());
        // Random unrelated node id.
        assert!(click(&panel, &tree, NodeId::PLACEHOLDER).is_empty());
    }

    /// Post-unification: the graph editor is one surface for both
    /// Effect-hosted and Generator-hosted graphs. Generators have no
    /// `effect_index` by definition — so a checkbox click on a
    /// generator's inner-node row MUST still emit a
    /// `ToggleNodeParamExpose` action. The app-side dispatcher
    /// resolves the `watched_graph_target` (Effect or Generator) and
    /// routes the command accordingly.
    ///
    /// Renamed + flipped from the original
    /// `handle_click_no_effect_index_returns_empty` which asserted
    /// the pre-unification (broken-for-generators) behaviour.
    #[test]
    fn handle_click_without_effect_index_still_emits_for_generator_graphs() {
        let mut tree = UITree::new();
        let mut panel = GraphEditorPanel::new();
        let node = snap_node_with_params(Some("uv_transform"));
        panel.configure(
            None, // No effect_index — simulating a Generator graph
            Some(&node),
            HashSet::new(),
            HashMap::new(),
            HashMap::new(),
            HashSet::new(),
        );
        panel.build(&mut tree, viewport());
        let cb_id = {
            let rows = inner_rows(&panel);
            let row = rows.first().expect("at least one inner-node row");
            checkbox_id_of(row)
        };
        let actions = click(&panel, &tree, cb_id);
        assert_eq!(
            actions.len(),
            1,
            "click on a generator inner-node row must emit one action"
        );
        assert!(
            matches!(actions[0], GraphEditCommand::ToggleNodeParamExpose { .. }),
            "must be ToggleNodeParamExpose, got {:?}",
            actions[0],
        );
    }

    /// Helper: pull a row's value-cell tree id, returning None for
    /// `Other`-kind params (which have no editable representation).
    fn value_cell_id_of(row: &RowState) -> Option<NodeId> {
        let RowState::InnerNode {
            value_cell_node_id, ..
        } = row
        else {
            panic!("expected an InnerNode row, got {row:?}");
        };
        *value_cell_node_id
    }

    /// Snapshot of a Transform-like node with a Mode enum + Bool +
    /// Float, so the click/drag/cycle tests can each exercise their
    /// own kind without re-stating the whole structure.
    fn snap_node_with_mixed_kinds() -> GraphEditorNodeView {
        GraphEditorNodeView {
            runtime_node_id: 7,
            node_id: GraphNodeId::new("uv_transform"),
            node_handle: Some("uv_transform".to_string()),
            title: "Transform".to_string(),
            parameters: vec![
                GraphEditorParam {
                    name: "scale".to_string(),
                    label: "Scale".to_string(),
                    kind: GraphEditorParamKind::Float,
                    default_value: 1.0,
                    current_value: 1.0,
                    range: Some((0.0, 4.0)),
                    enum_labels: None,
                    summary: None,
                    vec_value: [0.0; 4],
                    string_value: None,
                    table_value: None,
                },
                GraphEditorParam {
                    name: "enabled".to_string(),
                    label: "Enabled".to_string(),
                    kind: GraphEditorParamKind::Bool,
                    default_value: 0.0,
                    current_value: 0.0,
                    range: None,
                    enum_labels: None,
                    summary: None,
                    vec_value: [0.0; 4],
                    string_value: None,
                    table_value: None,
                },
                GraphEditorParam {
                    name: "mode".to_string(),
                    label: "Mode".to_string(),
                    kind: GraphEditorParamKind::Enum,
                    default_value: 0.0,
                    current_value: 1.0,
                    range: None,
                    enum_labels: Some(vec!["FoldX".into(), "FoldY".into(), "FoldBoth".into()]),
                    summary: None,
                    vec_value: [0.0; 4],
                    string_value: None,
                    table_value: None,
                },
            ],
            wgsl_source: None,
        }
    }

    #[test]
    fn bool_value_cell_click_toggles_via_set_graph_node_param() {
        let mut tree = UITree::new();
        let mut panel = GraphEditorPanel::new();
        let node = snap_node_with_mixed_kinds();
        panel.configure(
            Some(0),
            Some(&node),
            HashSet::new(),
            HashMap::new(),
            HashMap::new(),
            HashSet::new(),
        );
        panel.build(&mut tree, viewport());

        let bool_row = panel
            .rows
            .iter()
            .find(
                |r| matches!(r, RowState::InnerNode { inner_param, .. } if inner_param == "enabled"),
            )
            .expect("bool row exists");
        let cell = value_cell_id_of(bool_row).expect("bool row has value cell");
        let actions = click(&panel, &tree,cell);
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            GraphEditCommand::SetGraphNodeParam {
                node_id,
                param_name,
                new_value,
            } => {
                assert_eq!(*node_id, 7);
                assert_eq!(param_name, "enabled");
                assert_eq!(*new_value, SerializedParamValue::Bool { value: true });
            }
            other => panic!("unexpected action: {other:?}"),
        }
    }

    #[test]
    fn enum_value_cell_click_cycles_modulo_count() {
        let mut tree = UITree::new();
        let mut panel = GraphEditorPanel::new();
        let node = snap_node_with_mixed_kinds(); // mode current_value = 1
        panel.configure(
            Some(0),
            Some(&node),
            HashSet::new(),
            HashMap::new(),
            HashMap::new(),
            HashSet::new(),
        );
        panel.build(&mut tree, viewport());

        let mode_row = panel
            .rows
            .iter()
            .find(|r| matches!(r, RowState::InnerNode { inner_param, .. } if inner_param == "mode"))
            .expect("mode row exists");
        let cell = value_cell_id_of(mode_row).expect("mode row has value cell");
        let actions = click(&panel, &tree,cell);
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            GraphEditCommand::SetGraphNodeParam {
                param_name,
                new_value,
                ..
            } => {
                assert_eq!(param_name, "mode");
                assert_eq!(*new_value, SerializedParamValue::Enum { value: 2 });
            }
            other => panic!("unexpected action: {other:?}"),
        }
    }

    #[test]
    fn enum_wrap_around_from_last_option_returns_zero() {
        let mut tree = UITree::new();
        let mut panel = GraphEditorPanel::new();
        let mut node = snap_node_with_mixed_kinds();
        // Park on the last enum option so the cycle wraps.
        let mode = node
            .parameters
            .iter_mut()
            .find(|p| p.name == "mode")
            .unwrap();
        mode.current_value = 2.0;
        panel.configure(
            Some(0),
            Some(&node),
            HashSet::new(),
            HashMap::new(),
            HashMap::new(),
            HashSet::new(),
        );
        panel.build(&mut tree, viewport());

        let mode_row = panel
            .rows
            .iter()
            .find(|r| matches!(r, RowState::InnerNode { inner_param, .. } if inner_param == "mode"))
            .unwrap();
        let cell = value_cell_id_of(mode_row).unwrap();
        match &click(&panel, &tree,cell)[0] {
            GraphEditCommand::SetGraphNodeParam { new_value, .. } => {
                assert_eq!(*new_value, SerializedParamValue::Enum { value: 0 });
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn float_drag_emits_set_graph_node_param_for_each_drag_event() {
        use crate::input::{Modifiers, UIEvent};
        use crate::node::Vec2;

        let mut tree = UITree::new();
        let mut panel = GraphEditorPanel::new();
        let node = snap_node_with_mixed_kinds(); // scale: 1.0, range (0..4)
        panel.configure(
            Some(0),
            Some(&node),
            HashSet::new(),
            HashMap::new(),
            HashMap::new(),
            HashSet::new(),
        );
        panel.build(&mut tree, viewport());

        let scale_row = panel
            .rows
            .iter()
            .find(
                |r| matches!(r, RowState::InnerNode { inner_param, .. } if inner_param == "scale"),
            )
            .unwrap();
        let cell = value_cell_id_of(scale_row).unwrap();

        // DragBegin at x = 100. No action emitted yet.
        let begin = UIEvent::DragBegin {
            node_id: cell,
            pos: Vec2::new(100.0, 0.0),
            origin: Vec2::new(100.0, 0.0),
            modifiers: Modifiers::default(),
        };
        assert!(panel.handle_event(&begin).is_empty());

        // Drag right by 60 px → 0.25 of the (0..4) range → +1.0.
        // start_value 1.0 + 1.0 = 2.0.
        let drag = UIEvent::Drag {
            node_id: cell,
            pos: Vec2::new(160.0, 0.0),
            delta: Vec2::new(60.0, 0.0),
        };
        let acts = panel.handle_event(&drag);
        assert_eq!(acts.len(), 1);
        match &acts[0] {
            GraphEditCommand::SetGraphNodeParam {
                node_id,
                param_name,
                new_value,
            } => {
                assert_eq!(*node_id, 7);
                assert_eq!(param_name, "scale");
                match new_value {
                    SerializedParamValue::Float { value } => {
                        assert!((*value - 2.0).abs() < 0.01, "expected ~2.0, got {value}");
                    }
                    other => panic!("expected Float, got {other:?}"),
                }
            }
            other => panic!("unexpected action: {other:?}"),
        }

        // DragEnd clears state; subsequent drags on this cell are no-ops.
        let end = UIEvent::DragEnd {
            node_id: Some(cell),
            pos: Vec2::new(160.0, 0.0),
        };
        assert!(panel.handle_event(&end).is_empty());
        assert!(panel.drag.is_none());
    }

    #[test]
    fn float_drag_clamps_to_range() {
        use crate::input::{Modifiers, UIEvent};
        use crate::node::Vec2;

        let mut tree = UITree::new();
        let mut panel = GraphEditorPanel::new();
        let node = snap_node_with_mixed_kinds();
        panel.configure(
            Some(0),
            Some(&node),
            HashSet::new(),
            HashMap::new(),
            HashMap::new(),
            HashSet::new(),
        );
        panel.build(&mut tree, viewport());

        let cell = panel
            .rows
            .iter()
            .find_map(|r| match r {
                RowState::InnerNode {
                    value_cell_node_id: Some(v),
                    inner_param,
                    ..
                } if inner_param == "scale" => Some(*v),
                _ => None,
            })
            .unwrap();

        panel.handle_event(&UIEvent::DragBegin {
            node_id: cell,
            pos: Vec2::new(0.0, 0.0),
            origin: Vec2::new(0.0, 0.0),
            modifiers: Modifiers::default(),
        });
        // Drag way past the right edge — must clamp to max=4.0.
        let acts = panel.handle_event(&UIEvent::Drag {
            node_id: cell,
            pos: Vec2::new(10_000.0, 0.0),
            delta: Vec2::new(10_000.0, 0.0),
        });
        match &acts[0] {
            GraphEditCommand::SetGraphNodeParam {
                new_value: SerializedParamValue::Float { value },
                ..
            } => {
                assert!(
                    (*value - 4.0).abs() < 1e-3,
                    "expected clamp to 4.0, got {value}"
                );
            }
            _ => panic!("expected clamped Float"),
        }
    }

    #[test]
    fn outer_driven_row_stays_editable_so_inner_edits_emit_set_graph_node_param() {
        // The bidirectional model: even when an outer slider drives
        // this inner param every frame, the inner cell is still
        // editable. The renderer's binding apply skips writes when
        // the outer slot hasn't moved, so inline edits survive.
        let mut tree = UITree::new();
        let mut panel = GraphEditorPanel::new();
        let node = snap_node_with_mixed_kinds();
        let mut driven = HashMap::new();
        driven.insert(
            ("uv_transform".to_string(), "mode".to_string()),
            "Mode".to_string(),
        );
        panel.configure(
            Some(0),
            Some(&node),
            HashSet::new(),
            driven,
            HashMap::new(),
            HashSet::new(),
        );
        panel.build(&mut tree, viewport());

        let mode_cell = panel
            .rows
            .iter()
            .find_map(|r| match r {
                RowState::InnerNode {
                    value_cell_node_id: Some(v),
                    inner_param,
                    ..
                } if inner_param == "mode" => Some(*v),
                _ => None,
            })
            .expect("outer-driven row remains editable");

        // Clicking still emits a SetGraphNodeParam — the user can
        // override the outer.
        let actions = click(&panel, &tree,mode_cell);
        assert_eq!(actions.len(), 1);
        assert!(matches!(&actions[0], GraphEditCommand::SetGraphNodeParam { .. }));
    }

    #[test]
    fn inner_node_checkbox_for_static_block_target_emits_unified_toggle() {
        // After the exposure unification, the click handler emits ONE
        // PanelAction regardless of whether (handle, param) maps to a
        // static-block slot or a user-tail binding. The content-thread
        // command (`ToggleNodeParamExposeCommand`) figures out the
        // static-slot routing internally.
        let mut tree = UITree::new();
        let mut panel = GraphEditorPanel::new();
        let node = snap_node_with_mixed_kinds();
        let mut static_block_targets = HashMap::new();
        static_block_targets.insert(("uv_transform".to_string(), "scale".to_string()), 0_usize);
        let mut exposed_keys = HashSet::new();
        exposed_keys.insert(("uv_transform".to_string(), "scale".to_string()));
        panel.configure(
            Some(0),
            Some(&node),
            exposed_keys,
            HashMap::new(),
            static_block_targets,
            HashSet::new(),
        );
        panel.build(&mut tree, viewport());

        let scale_row = panel
            .rows
            .iter()
            .find(
                |r| matches!(r, RowState::InnerNode { inner_param, .. } if inner_param == "scale"),
            )
            .expect("scale row exists");
        let cb_id = checkbox_id_of(scale_row);
        let actions = click(&panel, &tree,cb_id);
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            GraphEditCommand::ToggleNodeParamExpose {
                node_handle,
                inner_param,
                expose,
                ..
            } => {
                assert_eq!(node_handle, "uv_transform");
                assert_eq!(inner_param, "scale");
                assert!(!expose, "click on a checked param emits expose: false",);
            }
            other => panic!("expected ToggleNodeParamExpose, got {other:?}"),
        }
    }

    #[test]
    fn inner_node_checkbox_for_unrouted_param_emits_unified_toggle() {
        let mut tree = UITree::new();
        let mut panel = GraphEditorPanel::new();
        let node = snap_node_with_mixed_kinds();
        panel.configure(
            Some(0),
            Some(&node),
            HashSet::new(),
            HashMap::new(),
            HashMap::new(),
            HashSet::new(),
        );
        panel.build(&mut tree, viewport());

        let enabled_row = panel
            .rows
            .iter()
            .find(
                |r| matches!(r, RowState::InnerNode { inner_param, .. } if inner_param == "enabled"),
            )
            .expect("enabled row exists");
        let cb_id = checkbox_id_of(enabled_row);
        let actions = click(&panel, &tree,cb_id);
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            GraphEditCommand::ToggleNodeParamExpose {
                node_handle,
                inner_param,
                expose,
                ..
            } => {
                assert_eq!(node_handle, "uv_transform");
                assert_eq!(inner_param, "enabled");
                assert!(*expose);
            }
            other => panic!("expected ToggleNodeParamExpose, got {other:?}"),
        }
    }

    /// Port-shadows-param: when a wire targets a node's same-named
    /// scalar input port, the row's checkbox click short-circuits to
    /// no-op and the value cell is rendered as a static label (no
    /// tracked tree id) — so neither the exposure nor a local edit
    /// can lie about what controls the param every frame. The user
    /// must disconnect the wire to reclaim either.
    #[test]
    fn wire_driven_row_disables_checkbox_and_value_cell() {
        let mut tree = UITree::new();
        let mut panel = GraphEditorPanel::new();
        let node = snap_node_with_params(Some("uv_transform"));
        let mut wire_driven = HashSet::new();
        wire_driven.insert(("uv_transform".to_string(), "translate".to_string()));
        panel.configure(
            Some(0),
            Some(&node),
            HashSet::new(),
            HashMap::new(),
            HashMap::new(),
            wire_driven,
        );
        panel.build(&mut tree, viewport());

        let translate_row = panel
            .rows
            .iter()
            .find(
                |r| matches!(r, RowState::InnerNode { inner_param, .. } if inner_param == "translate"),
            )
            .expect("translate row stays visible even when wire-driven");

        // Value cell is rendered as a read-only label rather than a
        // button — non-interactive both visually and at the tree level.
        assert!(
            value_cell_id_of(translate_row).is_none(),
            "wire-driven row drops the editable value-cell button in favour of a label",
        );

        // Checkbox click is a defensive no-op even though the style is
        // already disabled. Belt-and-braces in case the styling drifts.
        let cb_id = checkbox_id_of(translate_row);
        assert!(
            click(&panel, &tree,cb_id).is_empty(),
            "checkbox click on a wire-driven row must not emit any action",
        );
    }

    /// The non-wired sibling row stays interactive — wire-driven is a
    /// per-row gate, not a per-node one.
    #[test]
    fn wire_driven_row_does_not_disable_unrelated_rows() {
        let mut tree = UITree::new();
        let mut panel = GraphEditorPanel::new();
        let node = snap_node_with_params(Some("uv_transform"));
        let mut wire_driven = HashSet::new();
        wire_driven.insert(("uv_transform".to_string(), "translate".to_string()));
        panel.configure(
            Some(0),
            Some(&node),
            HashSet::new(),
            HashMap::new(),
            HashMap::new(),
            wire_driven,
        );
        panel.build(&mut tree, viewport());

        let scale_row = panel
            .rows
            .iter()
            .find(
                |r| matches!(r, RowState::InnerNode { inner_param, .. } if inner_param == "scale"),
            )
            .expect("scale row exists");
        let cb_id = checkbox_id_of(scale_row);
        let actions = click(&panel, &tree,cb_id);
        assert_eq!(actions.len(), 1, "non-wired sibling stays interactive");
    }

    // ── Inline colour / vector editor ───────────────────────────────

    /// A node with a single `Color` param carrying `initial` RGBA.
    fn snap_node_with_color(initial: [f32; 4]) -> GraphEditorNodeView {
        GraphEditorNodeView {
            runtime_node_id: 9,
            node_id: GraphNodeId::new("tint"),
            node_handle: Some("tint".to_string()),
            title: "Tint".to_string(),
            parameters: vec![GraphEditorParam {
                name: "color".to_string(),
                label: "Color".to_string(),
                kind: GraphEditorParamKind::Color,
                default_value: 0.0,
                current_value: 0.0,
                range: None,
                enum_labels: None,
                summary: None,
                vec_value: initial,
                string_value: None,
                table_value: None,
            }],
            wgsl_source: None,
        }
    }

    #[test]
    fn color_param_builds_one_vec_component_row_per_channel() {
        let mut tree = UITree::new();
        let mut panel = GraphEditorPanel::new();
        let node = snap_node_with_color([0.1, 0.2, 0.3, 1.0]);
        panel.configure(
            None,
            Some(&node),
            HashSet::new(),
            HashMap::new(),
            HashMap::new(),
            HashSet::new(),
        );
        panel.build(&mut tree, viewport());
        let channels = panel
            .rows
            .iter()
            .filter(|r| matches!(r, RowState::VecComponent { .. }))
            .count();
        assert_eq!(channels, 4, "RGBA produces four channel rows");
    }

    #[test]
    fn color_channel_drag_emits_full_color_with_other_channels_held() {
        use crate::input::{Modifiers, UIEvent};
        use crate::node::Vec2;

        let mut tree = UITree::new();
        let mut panel = GraphEditorPanel::new();
        let node = snap_node_with_color([0.2, 0.4, 0.6, 1.0]);
        panel.configure(
            None,
            Some(&node),
            HashSet::new(),
            HashMap::new(),
            HashMap::new(),
            HashSet::new(),
        );
        panel.build(&mut tree, viewport());

        // Drag the green channel cell (index 1).
        let g_cell = panel
            .rows
            .iter()
            .find_map(|r| match r {
                RowState::VecComponent {
                    value_cell_node_id,
                    channel: 1,
                    ..
                } => Some(*value_cell_node_id),
                _ => None,
            })
            .expect("green channel cell");

        panel.handle_event(&UIEvent::DragBegin {
            node_id: g_cell,
            pos: Vec2::new(100.0, 0.0),
            origin: Vec2::new(100.0, 0.0),
            modifiers: Modifiers::default(),
        });
        // A full-range drag right pushes green from 0.4 past 1.0 → clamped to 1.0.
        let acts = panel.handle_event(&UIEvent::Drag {
            node_id: g_cell,
            pos: Vec2::new(100.0 + DRAG_FULL_RANGE_PX, 0.0),
            delta: Vec2::new(DRAG_FULL_RANGE_PX, 0.0),
        });
        assert_eq!(acts.len(), 1);
        match &acts[0] {
            GraphEditCommand::SetGraphNodeParam {
                node_id,
                param_name,
                new_value,
            } => {
                assert_eq!(*node_id, 9);
                assert_eq!(param_name, "color");
                match new_value {
                    SerializedParamValue::Color { value } => {
                        // R, B, A carried through unchanged; G driven to max.
                        assert!((value[0] - 0.2).abs() < 1e-4);
                        assert!((value[1] - 1.0).abs() < 1e-4, "green clamps to 1.0");
                        assert!((value[2] - 0.6).abs() < 1e-4);
                        assert!((value[3] - 1.0).abs() < 1e-4);
                    }
                    other => panic!("expected Color, got {other:?}"),
                }
            }
            other => panic!("unexpected action: {other:?}"),
        }
    }

    // ── String params + path picker ─────────────────────────────────

    fn snap_node_with_string(name: &str, value: &str) -> GraphEditorNodeView {
        GraphEditorNodeView {
            runtime_node_id: 11,
            node_id: GraphNodeId::new("img"),
            node_handle: Some("img".to_string()),
            title: "Image Folder".to_string(),
            parameters: vec![GraphEditorParam {
                name: name.to_string(),
                label: name.to_string(),
                kind: GraphEditorParamKind::String,
                default_value: 0.0,
                current_value: 0.0,
                range: None,
                enum_labels: None,
                summary: Some(value.to_string()),
                vec_value: [0.0; 4],
                string_value: Some(value.to_string()),
                table_value: None,
            }],
            wgsl_source: None,
        }
    }

    #[test]
    fn path_string_param_gets_a_browse_button_that_emits_browse() {
        let mut tree = UITree::new();
        let mut panel = GraphEditorPanel::new();
        let node = snap_node_with_string("folder", "/clips/seq_01");
        panel.configure(
            None,
            Some(&node),
            HashSet::new(),
            HashMap::new(),
            HashMap::new(),
            HashSet::new(),
        );
        panel.build(&mut tree, viewport());
        let browse = panel
            .rows
            .iter()
            .find_map(|r| match r {
                RowState::BrowseButton {
                    button_node_id,
                    param_name,
                    ..
                } if param_name == "folder" => Some(*button_node_id),
                _ => None,
            })
            .expect("a path-like String param has a Browse button");
        match click(&panel, &tree,browse).as_slice() {
            [GraphEditCommand::BrowseGraphNodePath {
                node_id,
                param_name,
            }] => {
                assert_eq!(*node_id, 11);
                assert_eq!(param_name.as_str(), "folder");
            }
            other => panic!("expected BrowseGraphNodePath, got {other:?}"),
        }
    }

    #[test]
    fn plain_text_string_param_has_no_browse_button() {
        let mut tree = UITree::new();
        let mut panel = GraphEditorPanel::new();
        let node = snap_node_with_string("text", "HELLO");
        panel.configure(
            None,
            Some(&node),
            HashSet::new(),
            HashMap::new(),
            HashMap::new(),
            HashSet::new(),
        );
        panel.build(&mut tree, viewport());
        assert!(
            panel
                .rows
                .iter()
                .all(|r| !matches!(r, RowState::BrowseButton { .. })),
            "a non-path String param is read-only (no Browse button)"
        );
    }

    #[test]
    fn plain_text_string_value_cell_click_emits_string_edit() {
        let mut tree = UITree::new();
        let mut panel = GraphEditorPanel::new();
        let node = snap_node_with_string("text", "HELLO");
        panel.configure(
            None,
            Some(&node),
            HashSet::new(),
            HashMap::new(),
            HashMap::new(),
            HashSet::new(),
        );
        panel.build(&mut tree, viewport());
        let cell = panel
            .rows
            .iter()
            .find_map(|r| match r {
                RowState::EditTextButton {
                    button_node_id,
                    param_name,
                    ..
                } if param_name == "text" => Some(*button_node_id),
                _ => None,
            })
            .expect("a non-path String param has a clickable value cell");
        match click(&panel, &tree,cell).as_slice() {
            [GraphEditCommand::EditGraphNodeStringParam {
                node_id,
                param_name,
                current,
                ..
            }] => {
                assert_eq!(*node_id, 11);
                assert_eq!(param_name, "text");
                assert_eq!(current, "HELLO");
            }
            other => panic!("expected EditGraphNodeStringParam, got {other:?}"),
        }
    }

    // ── Table param grid editor ─────────────────────────────────────

    fn snap_node_with_table(rows: Vec<Vec<f32>>) -> GraphEditorNodeView {
        let cols = rows.first().map(|r| r.len()).unwrap_or(0);
        GraphEditorNodeView {
            runtime_node_id: 13,
            node_id: GraphNodeId::new("ramp"),
            node_handle: Some("ramp".to_string()),
            title: "Gradient Ramp".to_string(),
            parameters: vec![GraphEditorParam {
                name: "stops".to_string(),
                label: "Stops".to_string(),
                kind: GraphEditorParamKind::Other,
                default_value: 0.0,
                current_value: 0.0,
                range: None,
                enum_labels: None,
                summary: Some(format!("{}×{}", rows.len(), cols)),
                vec_value: [0.0; 4],
                string_value: None,
                table_value: Some(rows),
            }],
            wgsl_source: None,
        }
    }

    #[test]
    fn table_param_builds_one_clickable_cell_per_entry() {
        let mut tree = UITree::new();
        let mut panel = GraphEditorPanel::new();
        let node = snap_node_with_table(vec![vec![0.0, 1.0, 0.0, 0.0], vec![1.0, 0.0, 0.0, 1.0]]);
        panel.configure(
            None,
            Some(&node),
            HashSet::new(),
            HashMap::new(),
            HashMap::new(),
            HashSet::new(),
        );
        panel.build(&mut tree, viewport());
        let cells = panel
            .rows
            .iter()
            .filter(|r| matches!(r, RowState::TableCell { .. }))
            .count();
        assert_eq!(cells, 8, "a 2×4 table yields eight editable cells");
    }

    #[test]
    fn table_cell_click_emits_edit_with_coords_and_full_table() {
        let mut tree = UITree::new();
        let mut panel = GraphEditorPanel::new();
        let rows = vec![vec![0.0, 0.25, 0.5, 0.75], vec![1.0, 0.1, 0.2, 0.3]];
        let node = snap_node_with_table(rows.clone());
        panel.configure(
            None,
            Some(&node),
            HashSet::new(),
            HashMap::new(),
            HashMap::new(),
            HashSet::new(),
        );
        panel.build(&mut tree, viewport());
        // The cell at row 1, col 2 carries value 0.2.
        let btn = panel
            .rows
            .iter()
            .find_map(|r| match r {
                RowState::TableCell {
                    button_node_id,
                    row: 1,
                    col: 2,
                    ..
                } => Some(*button_node_id),
                _ => None,
            })
            .expect("row 1 col 2 cell exists");
        match click(&panel, &tree,btn).as_slice() {
            [GraphEditCommand::EditGraphNodeTableCell {
                node_id,
                param_name,
                row,
                col,
                current,
                rows: got,
                ..
            }] => {
                assert_eq!(*node_id, 13);
                assert_eq!(param_name, "stops");
                assert_eq!((*row, *col), (1, 2));
                assert!((*current - 0.2).abs() < 1e-6, "current is the clicked cell");
                assert_eq!(got, &rows, "the full table rides along for rebuild");
            }
            other => panic!("expected EditGraphNodeTableCell, got {other:?}"),
        }
    }
}
