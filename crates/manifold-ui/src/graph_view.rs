//! UI-local view-model of an editable node graph — the shape the graph canvas
//! reads, mirroring `manifold_renderer::node_graph`'s snapshot surface without a
//! `manifold-renderer` dependency.
//!
//! Phase 8 of `docs/UI_ARCHITECTURE_OVERHAUL.md` (sub-design
//! `docs/CANVAS_API_DESIGN.md` §0): the canvas moved into `manifold-ui`, so it
//! can no longer name the renderer's `GraphSnapshot`. These are owned, plain-data
//! mirrors; the app is the sole translator (`manifold-app/src/ui_translate.rs`),
//! exactly the Phase-5 layering-inversion pattern.
//!
//! Two differences from the renderer snapshot, both *resolved at translation
//! time* so the catalog (a large, generated renderer-side table) stays put:
//! [`NodeSnapshot::category`] + [`NodeSnapshot::tooltip`] and
//! [`ParamSnapshot::tooltip`] carry values the renderer's `descriptor_for` /
//! `tooltip_for` would otherwise be queried for. The canvas reads them straight
//! off the snapshot.

use manifold_foundation::NodeId;

/// Live (post-modulation) per-node param values for one frame, keyed by stable
/// [`NodeId`]. Each entry is `(node_id, [(param_name, value), …])`. Type-alias
/// (not a newtype) so it is the *identical* type as
/// `manifold_renderer::node_graph::LiveNodeParams` — the app hands it straight to
/// the canvas with no conversion. Param names are `'static` registry strings.
pub type LiveNodeParams = Vec<(NodeId, Vec<(&'static str, f32)>)>;

/// `EffectGraphNode` type id for a group (subgraph) instance. Mirror of
/// `manifold_core::effect_graph_def::GROUP_TYPE_ID` — a stable wire identifier.
pub const GROUP_TYPE_ID: &str = "group";
/// Boundary node feeding a group's interface inputs. Mirror of
/// `manifold_core::effect_graph_def::GROUP_INPUT_TYPE_ID`.
pub const GROUP_INPUT_TYPE_ID: &str = "system.group_input";
/// Boundary node draining a group's interface outputs. Mirror of
/// `manifold_core::effect_graph_def::GROUP_OUTPUT_TYPE_ID`.
pub const GROUP_OUTPUT_TYPE_ID: &str = "system.group_output";

/// Owned, UI-local view of a graph for the editor canvas. Mirror of
/// `manifold_renderer::node_graph::GraphSnapshot`.
#[derive(Debug, Clone, Default)]
pub struct GraphSnapshot {
    pub nodes: Vec<NodeSnapshot>,
    pub wires: Vec<WireSnapshot>,
    /// Outer effect-card params routed into an inner-node param every frame.
    pub outer_routings: Vec<OuterParamRouting>,
}

/// One outer→inner routing entry. Mirror of
/// `manifold_renderer::node_graph::OuterParamRouting`.
#[derive(Debug, Clone)]
pub struct OuterParamRouting {
    pub outer_label: String,
    pub outer_param_id: String,
    pub node_handle: String,
    pub inner_param: String,
    pub source: OuterParamSource,
}

/// Tier marker for an [`OuterParamRouting`]. Mirror of
/// `manifold_renderer::node_graph::OuterParamSource`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OuterParamSource {
    /// Declared on the effect's `ChainSpec.bindings` at compile time.
    Static,
    /// Per-instance user-exposed binding added via the editor's expose checkbox.
    User,
}

/// One node in the snapshot. Mirror of
/// `manifold_renderer::node_graph::NodeSnapshot`, plus the resolved-at-translate
/// [`Self::category`] / [`Self::tooltip`].
#[derive(Debug, Clone)]
pub struct NodeSnapshot {
    /// Stable runtime instance id within the graph level.
    pub id: u32,
    /// Stable [`NodeId`] — invariant under group / ungroup / move / flatten.
    /// `Default` (empty) for anonymous boundary nodes.
    pub node_id: NodeId,
    /// Author-assigned stable handle, if any. `None` for anonymous nodes.
    pub node_handle: Option<String>,
    /// `EffectNodeType` string — `node.mix`, `effect.bloom`, `group`, etc.
    pub type_id: String,
    /// Display title derived from `type_id`.
    pub title: String,
    pub inputs: Vec<PortSnapshot>,
    pub outputs: Vec<PortSnapshot>,
    pub parameters: Vec<ParamSnapshot>,
    /// Editor-saved position in graph-space, or `None` (auto-layout).
    pub editor_pos: Option<(f32, f32)>,
    /// Wires terminating here close a feedback loop; auto-layout skips them.
    pub breaks_dependency_cycle: bool,
    /// `Some` when this node is a group instance (`type_id == GROUP_TYPE_ID`);
    /// holds the recursive body so the canvas can descend.
    pub group: Option<Box<GroupSnapshot>>,
    /// Per-node WGSL kernel source override; `Some` only for `wgsl_compute*`
    /// nodes that pin a custom kernel. Drives the sidebar's "Edit Code".
    pub wgsl_source: Option<String>,
    /// Node `Category` resolved from the renderer's `descriptor_for` at
    /// translation time — the canvas maps it to a header tint.
    /// [`Category::Uncategorized`] when there's no descriptor.
    pub category: Category,
    /// Friendly one-line node summary (the descriptor `summary`), resolved at
    /// translation time. `None` for groups / blank summaries. Shown on hover.
    pub tooltip: Option<String>,
}

/// The body of a group node. Mirror of
/// `manifold_renderer::node_graph::GroupSnapshot`. Recursive.
#[derive(Debug, Clone)]
pub struct GroupSnapshot {
    pub nodes: Vec<NodeSnapshot>,
    pub wires: Vec<WireSnapshot>,
    /// Group accent colour (`GroupDef::tint`); `None` for the default tint.
    pub tint: Option<[f32; 4]>,
}

/// Snapshot of one inner-node parameter. Mirror of
/// `manifold_renderer::node_graph::ParamSnapshot`, plus the
/// resolved-at-translate [`Self::tooltip`].
#[derive(Debug, Clone)]
pub struct ParamSnapshot {
    pub name: String,
    pub label: String,
    pub kind: ParamSnapshotKind,
    pub default_value: f32,
    /// Current value on the live node this frame.
    pub current_value: f32,
    /// `(min, max)` for sliders; `None` when no range was declared.
    pub range: Option<(f32, f32)>,
    /// Enum option labels indexed by enum value. `None` for non-enum params.
    pub enum_labels: Option<Vec<String>>,
    /// Whether this param is currently exposed on the outer card.
    pub exposed: bool,
    /// Free-form summary for non-numeric params (e.g. a `Table`'s `"6×5"`).
    pub summary: Option<String>,
    /// Live multi-component value for `Color` / `Vec2` / `Vec3` / `Vec4`, in
    /// RGBA / XYZW order (zero-padded tail). `None` for scalar kinds.
    pub vec_value: Option<[f32; 4]>,
    /// Raw untruncated value for `String` params. `None` for non-String params.
    pub string_value: Option<String>,
    /// Row-major cell values for `Table` params. `None` for non-Table params.
    pub table_value: Option<Vec<Vec<f32>>>,
    /// Plain-English help line for this param (the renderer's `tooltip_for`),
    /// resolved at translation time. `None` if the author registered none.
    pub tooltip: Option<String>,
}

/// Coarse-grained param type. Mirror of
/// `manifold_renderer::node_graph::ParamSnapshotKind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamSnapshotKind {
    Float,
    /// Float-backed angle: stored RADIANS, displayed/edited in DEGREES.
    Angle,
    /// Float-backed frequency: stored RAD/S, displayed/edited in HERTZ.
    Frequency,
    Int,
    Bool,
    Enum,
    /// Momentary "fire once" button.
    Trigger,
    /// RGBA colour; live value carried in [`ParamSnapshot::vec_value`].
    Color,
    /// 2-component vector.
    Vec2,
    /// 3-component vector.
    Vec3,
    /// 4-component vector.
    Vec4,
    /// Text / path string.
    String,
    /// Remaining structured types (Table), shown via `summary`.
    Other,
}

/// One port (input or output) on a node. Mirror of
/// `manifold_renderer::node_graph::PortSnapshot`.
#[derive(Debug, Clone)]
pub struct PortSnapshot {
    pub name: String,
    pub kind: PortKindSnapshot,
}

/// One named typed channel on an `Array` port. Mirror of
/// `manifold_renderer::node_graph::ChannelSnapshot`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelSnapshot {
    pub name: String,
    /// Display string for the channel element type — `"F32"`, `"Vec3F"`, etc.
    pub ty: String,
}

/// Match-mode tag for an `Array` port. Mirror of
/// `manifold_renderer::node_graph::ArrayMatchMode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArrayMatchMode {
    Exact,
    Permissive,
}

/// Simplified port type. Mirror of
/// `manifold_renderer::node_graph::PortKindSnapshot`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PortKindSnapshot {
    Texture2D,
    /// Texture2D with a four-slot named-channel signature (R, G, B, A).
    Texture2DTyped {
        slots: [String; 4],
    },
    Texture3D,
    Scalar,
    Array {
        channels: Vec<ChannelSnapshot>,
        match_mode: ArrayMatchMode,
        item_size: u32,
        item_align: u32,
    },
    Camera,
    Light,
    Material,
    Transform,
    Atmosphere,
    Object,
}

/// One wire. Mirror of `manifold_renderer::node_graph::WireSnapshot`.
#[derive(Debug, Clone)]
pub struct WireSnapshot {
    pub from_node: u32,
    pub from_port: String,
    pub to_node: u32,
    pub to_port: String,
}

/// Node taxonomy bucket — drives the canvas's header tint by family. Mirror of
/// `manifold_renderer::node_graph::Category`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    Uncategorized,
    ColorAndTone,
    BlurAndSharpen,
    DistortAndWarp,
    Stylize,
    Generate,
    Noise,
    Mask,
    Composite,
    Geometry3D,
    MaterialsAndLighting,
    Particles2D,
    Particles3D,
    Control,
    DetectionAndSampling,
    MathAndConvert,
    Routing,
    FieldsAndCoordinates,
}
