//! Read-only graph snapshot for the editor UI.
//!
//! [`GraphSnapshot`] is an owned, `Send`able view of a [`Graph`] that the
//! content thread can build once per frame and hand to the UI thread. It
//! deliberately holds no references back into the live graph, no GPU
//! resources, and no trait objects — just plain data the canvas can render.
//!
//! V1 graphs don't carry editor positions yet (the editor that would set
//! them is V2 work — see `docs/NODE_GRAPH_SYSTEM.md` §13–14), so the
//! snapshot exposes `editor_pos: Option<(f32, f32)>` and the canvas falls
//! back to auto-layout when it's `None`.
//!
//! Cost is bounded by graph size: one allocation per node + one per wire
//! plus a couple of small string clones per port. Cheap enough to rebuild
//! every frame for V1 (4-node test graph). A future optimization is to
//! gate snapshot generation on a topology version counter.

use crate::node_graph::graph::Graph;
use crate::node_graph::parameters::{ParamType, ParamValue};
use crate::node_graph::persistence::{EffectGraphDefExt, PrimitiveRegistry};
use crate::node_graph::ports::{ChannelElementType, MatchMode, PortKind, PortType};

use manifold_core::effect_graph_def::EffectGraphDef;

/// Owned, `Send`able view of a graph for the editor canvas.
#[derive(Debug, Clone)]
pub struct GraphSnapshot {
    pub nodes: Vec<NodeSnapshot>,
    pub wires: Vec<WireSnapshot>,
    /// Outer effect-card params whose value is routed into an
    /// inner-node param every frame. The editor inspector disables
    /// those inner rows so the user can see (a) which inner param an
    /// outer slider drives, and (b) why their inline edit doesn't
    /// stick — the outer routing overwrites it before the next
    /// render. Populated by the producer (effect-registry path for
    /// catalog graphs, content-thread for per-card defs).
    pub outer_routings: Vec<OuterParamRouting>,
}

/// One outer→inner routing entry. `outer_label` is the slider label
/// shown on the effect card ("Amount", "Mode"). `node_handle` and
/// `inner_param` together identify the inner-node param that gets
/// overwritten — they match the snapshot's `NodeSnapshot.node_handle`
/// and `ParamSnapshot.name`. `outer_param_id` is the stable
/// `ParamSpec::id` of the outer slot — UI code resolves this to a
/// static-block slot index via `EffectDef::id_to_index`, so the
/// per-node "Expose to card" checkbox can toggle the matching
/// `param_values[slot_index].exposed` directly instead of layering
/// a redundant user-binding on top of an already-routed param.
///
/// `source` tags whether the entry came from a registry-declared
/// static binding or from a per-instance user-exposed binding — the
/// runtime apply path doesn't branch on this (Phase 1 unification),
/// but the editor uses it to style the two tiers differently and to
/// pick the right command (`ToggleStaticParamExposeCommand` vs
/// `ToggleEffectParamExposeCommand`) when the user un-checks a row.
#[derive(Debug, Clone)]
pub struct OuterParamRouting {
    pub outer_label: String,
    pub outer_param_id: String,
    pub node_handle: String,
    pub inner_param: String,
    pub source: OuterParamSource,
}

/// Tier marker for an [`OuterParamRouting`]. Mirrors
/// [`crate::node_graph::BindingSource`] but lives in the snapshot
/// (a `Send`able, allocation-only data type with no renderer types
/// in scope) so the UI thread can read it without depending on
/// `param_binding`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OuterParamSource {
    /// Declared on the effect's `ChainSpec.bindings` at compile time.
    Static,
    /// Per-instance `EffectInstance.user_param_bindings` entry, added
    /// by the user via the graph editor's expose checkboxes.
    User,
}

/// One node in the snapshot.
#[derive(Debug, Clone)]
pub struct NodeSnapshot {
    /// Stable instance id within the graph. Matches `NodeInstanceId.0`.
    pub id: u32,
    /// Author-assigned stable string handle if this node was registered
    /// via `Graph::add_node_named`. Used by V2 user-exposed parameter
    /// bindings to address inner nodes across renderer refactors.
    /// `None` for anonymous nodes (boundary Source/FinalOutput, etc.).
    pub node_handle: Option<String>,
    /// `EffectNodeType` string — `node.mix`, `effect.bloom`, etc.
    pub type_id: String,
    /// Display title derived from `type_id` (e.g. `node.mix` → "Mix").
    pub title: String,
    pub inputs: Vec<PortSnapshot>,
    pub outputs: Vec<PortSnapshot>,
    /// Inner parameters on this node, exposed for the V2 user-exposed-
    /// param UI (right-sidebar checkbox list). Owned data — strings
    /// allocated from `ParamDef`'s `&'static str` fields once per
    /// snapshot build.
    pub parameters: Vec<ParamSnapshot>,
    /// Editor-saved position in graph-space, or `None` when the graph
    /// has never been opened in an editor (V1).
    pub editor_pos: Option<(f32, f32)>,
    /// Mirrors `EffectNode::breaks_dependency_cycle()`. Wires
    /// terminating on this node close a per-frame feedback loop and
    /// the canvas's auto-layout must skip them — otherwise depth
    /// propagation around the loop pushes consumers off-screen to
    /// the right (one extra column per relaxation pass × n+1 passes).
    pub breaks_dependency_cycle: bool,
}

/// Snapshot of one inner-node parameter, sized for the user-exposed-
/// parameter UI. Mirrors [`crate::node_graph::parameters::ParamDef`]
/// with owned strings + enum-flattened type info, so the data is
/// fully `Send`able and free of `'static` references back into the
/// live graph.
#[derive(Debug, Clone)]
pub struct ParamSnapshot {
    /// Stable parameter name. Used as `inner_param` when constructing
    /// a `UserParamBinding`.
    pub name: String,
    /// Display label. Used as the binding's `label` (initially) and
    /// as the row label in the right-sidebar list.
    pub label: String,
    pub kind: ParamSnapshotKind,
    /// Numeric default for slider initialization. Bool/Int/Enum
    /// flattened to f32 so the UI slider has one shape.
    pub default_value: f32,
    /// Current value on the live node — what the renderer is actually
    /// using this frame. Editor inspector reads this so users can see
    /// what each node is currently doing instead of just topology.
    pub current_value: f32,
    /// `(min, max)` for sliders. `None` when the underlying ParamDef
    /// didn't declare a range (e.g. Vec2/Color/Enum often omit it).
    pub range: Option<(f32, f32)>,
    /// For `Enum` kind: the option labels indexed by enum value, so
    /// the inspector can render "FoldX" instead of `6`. `None` for
    /// non-enum params.
    pub enum_labels: Option<Vec<String>>,
    /// Whether this param is currently exposed on the outer card.
    /// Mirrors `NodeInstance.exposed_params` — the graph editor's
    /// right-panel checkbox flips this through
    /// `ToggleNodeParamExposeCommand`. Works the same for Effect-
    /// hosted and Generator-hosted graphs (no fork on target type).
    pub exposed: bool,
    /// Free-form summary text for non-numeric params (currently only
    /// `Table` — rendered as `"6×5"` in the inspector). `None` for
    /// numeric params, which render `current_value` instead.
    pub summary: Option<String>,
}

/// Coarse-grained variant of `ParamType` — the user-exposed-param
/// surface only needs to know "is it a float / int / bool / enum"
/// to pick the right `ParamConvert` at expose time. Vec2/Vec3/
/// Vec4/Color are not user-exposable in the V2 surface (they need
/// multi-slot routing) and are flagged so the panel can skip them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamSnapshotKind {
    Float,
    /// Float-backed angle. Stored value is RADIANS (so wired modulation and
    /// preset math stay correct), but the UI displays and edits in DEGREES,
    /// converting at the slider boundary only. See [`ParamType::Angle`].
    Angle,
    /// Float-backed frequency. Stored value is RADIANS PER SECOND, but the UI
    /// displays and edits in HERTZ (rad/s ÷ 2π), at the slider boundary only.
    /// See [`ParamType::Frequency`].
    Frequency,
    Int,
    Bool,
    Enum,
    /// Momentary "fire once" button. See [`ParamType::Trigger`] in
    /// `manifold-renderer/.../parameters.rs` for the storage / cold-start
    /// contract; the outer-card click handler increments by one per press.
    Trigger,
    /// Multi-component types (Vec2/Vec3/Vec4/Color) — not exposable
    /// in the V2 user surface.
    Other,
}

/// One port (input or output) on a node snapshot.
#[derive(Debug, Clone)]
pub struct PortSnapshot {
    pub name: String,
    pub kind: PortKindSnapshot,
}

/// One named typed channel on an `Array` port, as the editor sees it.
/// Resolved from [`ChannelSpec`](crate::node_graph::ports::ChannelSpec) at
/// snapshot-build time so the canvas can render tooltips without depending
/// on the channel_names registry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelSnapshot {
    /// Source string for the channel name. Recovered from the
    /// `well_known` registry via `ChannelName::debug_name` when the
    /// name is registered there; falls back to a hex-formatted hash
    /// (`"0x{:016x}"`) for runtime-introduced names (e.g. `wgsl_compute`
    /// shader fields not in `well_known`).
    pub name: String,
    /// Display string for the channel element type — `"F32"`, `"U32"`,
    /// `"Vec3F"`, etc. Stable spellings the editor can pattern-match
    /// without depending on the `ChannelElementType` enum directly.
    pub ty: String,
}

/// Match-mode tag for the snapshot's `Array` variant. Mirrors
/// [`MatchMode`](crate::node_graph::ports::MatchMode); kept distinct so the
/// editor doesn't depend on the validator's enum directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArrayMatchMode {
    Exact,
    Permissive,
}

/// Simplified port type for snapshot — collapses scalar sub-types into
/// one bucket since the canvas colours by category, not by float vs vec3.
/// The `Array` variant carries owned channel metadata so the editor's
/// hover-tooltip can render the per-port Channels signature directly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PortKindSnapshot {
    Texture2D,
    /// Texture2D decorated with a four-slot named-channel signature
    /// (per `docs/CHANNEL_TYPE_SYSTEM.md` §17). The `slots` array
    /// carries the channel name strings in R, G, B, A order so the
    /// editor's hover-tooltip can render the per-port texture-channel
    /// layout directly.
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
}

fn channel_element_type_to_display(ty: ChannelElementType) -> &'static str {
    match ty {
        ChannelElementType::F32 => "F32",
        ChannelElementType::I32 => "I32",
        ChannelElementType::U32 => "U32",
        ChannelElementType::Vec2F => "Vec2F",
        ChannelElementType::Vec3F => "Vec3F",
        ChannelElementType::Vec4F => "Vec4F",
    }
}

impl From<PortType> for PortKindSnapshot {
    fn from(t: PortType) -> Self {
        match t {
            PortType::Texture2D => Self::Texture2D,
            PortType::Texture2DTyped(tc) => {
                let slots = tc.slots.map(|name| {
                    name.debug_name()
                        .map(String::from)
                        .unwrap_or_else(|| format!("{:#018x}", name.hash()))
                });
                Self::Texture2DTyped { slots }
            }
            PortType::Texture3D => Self::Texture3D,
            PortType::Scalar(_) => Self::Scalar,
            PortType::Array(at) => {
                let channels: Vec<ChannelSnapshot> = at
                    .specs
                    .iter()
                    .map(|spec| ChannelSnapshot {
                        name: spec
                            .name
                            .debug_name()
                            .map(String::from)
                            .unwrap_or_else(|| format!("{:#018x}", spec.name.hash())),
                        ty: channel_element_type_to_display(spec.ty).to_string(),
                    })
                    .collect();
                let match_mode = match at.match_mode {
                    MatchMode::Exact => ArrayMatchMode::Exact,
                    MatchMode::Permissive => ArrayMatchMode::Permissive,
                };
                Self::Array {
                    channels,
                    match_mode,
                    item_size: at.item_size,
                    item_align: at.item_align,
                }
            }
            PortType::Camera => Self::Camera,
            PortType::Light => Self::Light,
            PortType::Material => Self::Material,
        }
    }
}

/// One wire in the snapshot.
#[derive(Debug, Clone)]
pub struct WireSnapshot {
    pub from_node: u32,
    pub from_port: String,
    pub to_node: u32,
    pub to_port: String,
}

impl GraphSnapshot {
    /// Build a snapshot from a live graph. Walks every node and wire,
    /// allocates owned strings so the result is fully detached from the
    /// graph's `'static` port-name references.
    pub fn from_graph(graph: &Graph) -> Self {
        // Reverse the graph's handle map once, so we can look up
        // node_id → handle in O(1) per node. Handles are unique within
        // a graph (enforced by add_node_named's panic-on-dup).
        let id_to_handle: ahash::AHashMap<u32, String> = graph
            .handles()
            .map(|(h, id)| (id.0, h.to_string()))
            .collect();

        let mut nodes: Vec<NodeSnapshot> = graph
            .nodes()
            .map(|inst| {
                let type_id = inst.node.type_id().as_str().to_string();
                let title = match inst.node.display_title() {
                    Some(custom) => format!("{custom} (WGSL)"),
                    None => title_from_type_id(&type_id),
                };
                let inputs = inst
                    .node
                    .inputs()
                    .iter()
                    .filter(|p| matches!(p.kind, PortKind::Input))
                    .map(|p| PortSnapshot {
                        name: p.name.to_string(),
                        kind: PortKindSnapshot::from(p.ty),
                    })
                    .collect();
                let outputs = inst
                    .node
                    .outputs()
                    .iter()
                    .filter(|p| matches!(p.kind, PortKind::Output))
                    .map(|p| PortSnapshot {
                        name: p.name.to_string(),
                        kind: PortKindSnapshot::from(p.ty),
                    })
                    .collect();
                let parameters = inst
                    .node
                    .parameters()
                    .iter()
                    .map(|pd| {
                        // Read the live current value off the node
                        // instance's param map — falling back to the
                        // declared default if the key isn't present
                        // (shouldn't happen for properly-initialized
                        // graphs, but harmless if it does).
                        let current = inst
                            .params
                            .get(pd.name)
                            .cloned()
                            .unwrap_or_else(|| pd.default.clone());
                        let summary = match &current {
                            ParamValue::Table(t) => {
                                Some(format!("{}×{}", t.row_count(), t.col_count()))
                            }
                            _ => None,
                        };
                        ParamSnapshot {
                            name: pd.name.to_string(),
                            label: pd.label.to_string(),
                            kind: param_snapshot_kind(pd.ty),
                            default_value: param_default_to_f32(&pd.default),
                            current_value: param_default_to_f32(&current),
                            range: pd.range,
                            enum_labels: if matches!(pd.ty, ParamType::Enum) {
                                Some(
                                    pd.enum_values
                                        .iter()
                                        .map(|s| (*s).to_string())
                                        .collect(),
                                )
                            } else {
                                None
                            },
                            exposed: inst.exposed_params.contains(pd.name),
                            summary,
                        }
                    })
                    .collect();
                NodeSnapshot {
                    id: inst.id.0,
                    node_handle: id_to_handle.get(&inst.id.0).cloned(),
                    type_id,
                    title,
                    inputs,
                    outputs,
                    parameters,
                    editor_pos: None,
                    breaks_dependency_cycle: inst.node.breaks_dependency_cycle(),
                }
            })
            .collect();
        // Stable order so the canvas's auto-layout is deterministic across
        // snapshots (graph.nodes() iterates the underlying AHashMap in
        // arbitrary order).
        nodes.sort_by_key(|n| n.id);

        let wires = graph
            .wires()
            .iter()
            .map(|w| WireSnapshot {
                from_node: w.from.0.0,
                from_port: w.from.1.to_string(),
                to_node: w.to.0.0,
                to_port: w.to.1.to_string(),
            })
            .collect();

        Self {
            nodes,
            wires,
            // `from_graph` operates on a bare graph with no effect-
            // level context, so it can't know about outer→inner
            // routings. The effect-registry path fills this in
            // after calling `effect.graph_snapshot()`.
            outer_routings: Vec::new(),
        }
    }

    /// Build a snapshot from a serialized [`EffectGraphDef`]. Used by
    /// the editor pipeline when an `EffectInstance` carries a
    /// per-card graph override — we don't need to spin up GPU state
    /// just to draw the editor canvas, so route through
    /// [`EffectGraphDefExt::into_graph`] with the built-in
    /// [`PrimitiveRegistry`] and snapshot the temporary live graph.
    ///
    /// Returns `None` if the def references an unknown type id or
    /// is otherwise unloadable — the editor canvas treats `None`
    /// like "no active graph" rather than showing a partial state.
    pub fn from_def(def: &EffectGraphDef) -> Option<Self> {
        let registry = PrimitiveRegistry::with_builtin();
        let graph = match def.clone().into_graph(&registry) {
            Ok(g) => g,
            Err(e) => {
                eprintln!(
                    "[manifold-renderer] GraphSnapshot::from_def: \
                     failed to materialize per-instance graph: {e}. \
                     Editor canvas will treat this as empty."
                );
                return None;
            }
        };
        let mut snap = Self::from_graph(&graph);

        // Translate runtime ids back to doc ids. `into_graph` assigns
        // runtime ids sequentially (0..n) in def order, and `from_graph`
        // sorts snap.nodes by runtime id — so iterating both in parallel
        // lines up doc nodes with their snap entries. Editor commands
        // (`SetGraphNodeParamCommand`, `ConnectPortsCommand`, …) address
        // def nodes by doc id, so the snapshot must hand the UI doc ids,
        // not the temporary runtime ids of the throwaway graph we just
        // built. When the def's ids happen to be 0..n in declaration
        // order the two coincide; when nodes get appended out of order
        // (e.g. OilyFluid's `..., 10, 41, 11, 12, …, 27, 28, …`) they
        // don't, and command targeting silently writes to the wrong node.
        // Also overlay `editor_pos` while we're here — otherwise the
        // canvas auto-lays-out from scratch on every editor reopen.
        let runtime_to_doc: ahash::AHashMap<u32, u32> = snap
            .nodes
            .iter()
            .zip(def.nodes.iter())
            .map(|(s, d)| (s.id, d.id))
            .collect();
        for (snap_node, doc_node) in snap.nodes.iter_mut().zip(def.nodes.iter()) {
            snap_node.editor_pos = doc_node.editor_pos;
            snap_node.id = doc_node.id;
        }
        for wire in &mut snap.wires {
            if let Some(&doc) = runtime_to_doc.get(&wire.from_node) {
                wire.from_node = doc;
            }
            if let Some(&doc) = runtime_to_doc.get(&wire.to_node) {
                wire.to_node = doc;
            }
        }
        Some(snap)
    }
}

/// Convert a stable type id like `node.mix` or `composite.bloom` into
/// a short title suitable for display: "Mix", "Bloom". Falls back to the
/// raw id when there's no dot separator.
fn title_from_type_id(type_id: &str) -> String {
    let tail = type_id.rsplit_once('.').map(|(_, t)| t).unwrap_or(type_id);
    let mut chars = tail.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// Map [`ParamType`] onto the snapshot's coarse-grained kind enum.
/// Multi-component types (Vec2/Vec3/Vec4/Color) collapse to `Other`
/// — they're not exposable as user params in the V2 surface.
fn param_snapshot_kind(ty: ParamType) -> ParamSnapshotKind {
    match ty {
        ParamType::Float => ParamSnapshotKind::Float,
        ParamType::Angle => ParamSnapshotKind::Angle,
        ParamType::Frequency => ParamSnapshotKind::Frequency,
        ParamType::Int => ParamSnapshotKind::Int,
        ParamType::Bool => ParamSnapshotKind::Bool,
        ParamType::Enum => ParamSnapshotKind::Enum,
        ParamType::Trigger => ParamSnapshotKind::Trigger,
        _ => ParamSnapshotKind::Other,
    }
}

/// Flatten a [`ParamValue`] into an `f32` for the slider UI. Bool
/// becomes 0.0/1.0, Int/Enum cast to f32, multi-component types
/// collapse to 0.0 (their snapshot kind is `Other` and they're not
/// user-exposable, so the value is unused).
fn param_default_to_f32(value: &ParamValue) -> f32 {
    match value {
        ParamValue::Float(f) => *f,
        ParamValue::Bool(b) => {
            if *b {
                1.0
            } else {
                0.0
            }
        }
        ParamValue::Enum(u) => *u as f32,
        ParamValue::Vec2(_)
        | ParamValue::Vec3(_)
        | ParamValue::Vec4(_)
        | ParamValue::Color(_)
        | ParamValue::Table(_)
        | ParamValue::String(_) => 0.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::effect_node::{EffectNode, EffectNodeContext, EffectNodeType};
    use crate::node_graph::parameters::ParamDef;
    use crate::node_graph::ports::{NodeInput, NodeOutput, NodePort};

    struct StubNode {
        type_id: EffectNodeType,
        inputs: Vec<NodeInput>,
        outputs: Vec<NodeOutput>,
    }

    impl EffectNode for StubNode {
        fn type_id(&self) -> &EffectNodeType {
            &self.type_id
        }
        fn inputs(&self) -> &[NodeInput] {
            &self.inputs
        }
        fn outputs(&self) -> &[NodeOutput] {
            &self.outputs
        }
        fn parameters(&self) -> &[ParamDef] {
            &[]
        }
        fn evaluate(&mut self, _: &mut EffectNodeContext<'_, '_>) {}
    }

    fn input(name: &'static str) -> NodeInput {
        NodePort {
            name,
            ty: PortType::Texture2D,
            kind: PortKind::Input,
            required: true,
        }
    }
    fn output(name: &'static str) -> NodeOutput {
        NodePort {
            name,
            ty: PortType::Texture2D,
            kind: PortKind::Output,
            required: false,
        }
    }

    #[test]
    fn snapshot_captures_nodes_and_wires() {
        let mut g = Graph::new();
        let a = g.add_node(Box::new(StubNode {
            type_id: EffectNodeType::new("node.source"),
            inputs: vec![],
            outputs: vec![output("out")],
        }));
        let b = g.add_node(Box::new(StubNode {
            type_id: EffectNodeType::new("node.mix"),
            inputs: vec![input("a"), input("b")],
            outputs: vec![output("out")],
        }));
        g.connect((a, "out"), (b, "a")).unwrap();

        let snap = GraphSnapshot::from_graph(&g);
        assert_eq!(snap.nodes.len(), 2);
        assert_eq!(snap.wires.len(), 1);
        assert!(snap.nodes.iter().any(|n| n.title == "Source"));
        assert!(snap.nodes.iter().any(|n| n.title == "Mix"));
        let mix = snap.nodes.iter().find(|n| n.title == "Mix").unwrap();
        assert_eq!(mix.inputs.len(), 2);
        assert_eq!(mix.outputs.len(), 1);
    }

    #[test]
    fn title_lowercase_id_capitalizes() {
        assert_eq!(title_from_type_id("node.blur"), "Blur");
        assert_eq!(title_from_type_id("composite.bloom"), "Bloom");
        assert_eq!(title_from_type_id("oddball"), "Oddball");
    }

    struct ParamfulNode {
        type_id: EffectNodeType,
        outputs: Vec<NodeOutput>,
        params: Vec<ParamDef>,
    }
    impl EffectNode for ParamfulNode {
        fn type_id(&self) -> &EffectNodeType {
            &self.type_id
        }
        fn inputs(&self) -> &[NodeInput] {
            &[]
        }
        fn outputs(&self) -> &[NodeOutput] {
            &self.outputs
        }
        fn parameters(&self) -> &[ParamDef] {
            &self.params
        }
        fn evaluate(&mut self, _: &mut EffectNodeContext<'_, '_>) {}
    }

    #[test]
    fn snapshot_captures_node_handle_when_named() {
        use crate::node_graph::parameters::ParamType;
        let mut g = Graph::new();
        let _anon = g.add_node(Box::new(StubNode {
            type_id: EffectNodeType::new("node.source"),
            inputs: vec![],
            outputs: vec![output("out")],
        }));
        let _named = g.add_node_named(
            "uv_transform",
            Box::new(ParamfulNode {
                type_id: EffectNodeType::new("node.transform"),
                outputs: vec![output("out")],
                params: vec![
                    ParamDef {
                        name: "translate",
                        label: "Translate",
                        ty: ParamType::Float,
                        default: ParamValue::Float(0.0),
                        range: Some((-1.0, 1.0)),
                        enum_values: &[],
                    },
                    ParamDef {
                        name: "mode",
                        label: "Mode",
                        ty: ParamType::Enum,
                        default: ParamValue::Enum(0),
                        range: None,
                        enum_values: &["A", "B", "C"],
                    },
                ],
            }),
        );

        let snap = GraphSnapshot::from_graph(&g);
        let anon = snap.nodes.iter().find(|n| n.title == "Source").unwrap();
        assert_eq!(anon.node_handle, None);
        assert!(anon.parameters.is_empty());

        let named = snap.nodes.iter().find(|n| n.title == "Transform").unwrap();
        assert_eq!(named.node_handle.as_deref(), Some("uv_transform"));
        assert_eq!(named.parameters.len(), 2);

        let translate = named
            .parameters
            .iter()
            .find(|p| p.name == "translate")
            .unwrap();
        assert_eq!(translate.label, "Translate");
        assert_eq!(translate.kind, ParamSnapshotKind::Float);
        assert_eq!(translate.range, Some((-1.0, 1.0)));
        assert!((translate.default_value - 0.0).abs() < f32::EPSILON);

        let mode = named.parameters.iter().find(|p| p.name == "mode").unwrap();
        assert_eq!(mode.kind, ParamSnapshotKind::Enum);
        // Enum default flattens to f32 via `as f32` cast.
        assert!((mode.default_value - 0.0).abs() < f32::EPSILON);
    }

    /// `GraphSnapshot::from_def` builds a snapshot directly from an
    /// `EffectGraphDef`, matching the editor's per-card path.
    #[test]
    fn from_def_builds_snapshot_with_named_handles() {
        use crate::node_graph::EffectGraphDefExt;
        use manifold_core::effect_graph_def::EffectGraphDef;

        // Build a small named-handle graph via an existing composite
        // builder and serialize it to a def — the round-trip should
        // produce the same snapshot structure end-to-end.
        let mut g = Graph::new();
        let src = g.add_node_named(
            "source",
            Box::new(crate::node_graph::boundary_nodes::Source::new()),
        );
        let handle = crate::node_graph::composites::build_soft_focus(&mut g, (src, "out"))
            .expect("build_soft_focus");
        let _out = g.add_node_named(
            "final_output",
            Box::new(crate::node_graph::boundary_nodes::FinalOutput::new()),
        );
        let final_out_id = g.node_id_by_handle("final_output").unwrap();
        g.connect(handle.output(), (final_out_id, "in")).unwrap();

        let def = EffectGraphDef::from_graph(&g);
        let snap = GraphSnapshot::from_def(&def).expect("from_def succeeds");

        // Same number of nodes + wires as the live graph.
        // 4 nodes: Source, blur, mix, FinalOutput.
        // 4 wires: src→blur.source, src→mix.a, blur.out→mix.b, mix.out→final.in.
        assert_eq!(snap.nodes.len(), 4);
        assert_eq!(snap.wires.len(), 4);
        // Named handles survive.
        assert!(snap
            .nodes
            .iter()
            .any(|n| n.node_handle.as_deref() == Some("source")));
        assert!(snap
            .nodes
            .iter()
            .any(|n| n.node_handle.as_deref() == Some("blur")));
        assert!(snap
            .nodes
            .iter()
            .any(|n| n.node_handle.as_deref() == Some("mix")));
        assert!(snap
            .nodes
            .iter()
            .any(|n| n.node_handle.as_deref() == Some("final_output")));
    }

    /// Regression: `editor_pos` saved in the def must survive
    /// `from_def`. Without the overlay step the round-trip through a
    /// live `Graph` strips positions and the editor canvas
    /// auto-lays-out on every reopen.
    #[test]
    fn from_def_preserves_editor_pos_through_overlay() {
        use crate::node_graph::EffectGraphDefExt;
        use manifold_core::effect_graph_def::EffectGraphDef;

        // Build a minimal soft-focus graph with a moved Source node.
        let mut g = Graph::new();
        let src = g.add_node_named(
            "source",
            Box::new(crate::node_graph::boundary_nodes::Source::new()),
        );
        let handle = crate::node_graph::composites::build_soft_focus(&mut g, (src, "out"))
            .expect("build_soft_focus");
        let final_out = g.add_node_named(
            "final_output",
            Box::new(crate::node_graph::boundary_nodes::FinalOutput::new()),
        );
        g.connect(handle.output(), (final_out, "in")).unwrap();
        let mut def = EffectGraphDef::from_graph(&g);
        // Simulate a MoveGraphNodeCommand on the Source node (doc id 0).
        def.nodes[0].editor_pos = Some((123.0, 456.0));

        let snap = GraphSnapshot::from_def(&def).expect("from_def succeeds");
        let snap_source = snap
            .nodes
            .iter()
            .find(|n| n.node_handle.as_deref() == Some("source"))
            .unwrap();
        assert_eq!(snap_source.editor_pos, Some((123.0, 456.0)));
    }

    /// Regression: when a def's node ids are out of declaration order
    /// (e.g. ids appended later to an existing preset), `from_def` must
    /// still expose doc ids — not the throwaway runtime ids `into_graph`
    /// assigns sequentially in visit order — on both nodes and wires.
    /// Otherwise editor commands like `SetGraphNodeParamCommand` and
    /// `ConnectPortsCommand`, which address def nodes by their doc id,
    /// silently target the wrong node. (OilyFluid hit this once doc ids
    /// 41/42/43 were appended mid-graph.)
    #[test]
    fn from_def_exposes_doc_ids_when_def_ids_are_out_of_order() {
        use manifold_core::effect_graph_def::{
            EffectGraphDef, EffectGraphNode, EffectGraphWire, EFFECT_GRAPH_VERSION,
        };

        // Visit order [10, 99] — runtime would assign 0, 1; doc ids are 10, 99.
        let def = EffectGraphDef {
            version: EFFECT_GRAPH_VERSION,
            name: None,
            description: None,
            preset_metadata: None,
            nodes: vec![
                EffectGraphNode {
                    id: 10,
                    type_id: "system.source".to_string(),
                    handle: Some("source".to_string()),
                    params: Default::default(),
                    exposed_params: Default::default(),
                    editor_pos: None,
                    wgsl_source: None,
                    title: None,
                    output_formats: Default::default(),
                    output_canvas_scales: std::collections::BTreeMap::new(),
                },
                EffectGraphNode {
                    id: 99,
                    type_id: "system.final_output".to_string(),
                    handle: Some("final_output".to_string()),
                    params: Default::default(),
                    exposed_params: Default::default(),
                    editor_pos: None,
                    wgsl_source: None,
                    title: None,
                    output_formats: Default::default(),
                    output_canvas_scales: std::collections::BTreeMap::new(),
                },
            ],
            wires: vec![EffectGraphWire {
                from_node: 10,
                from_port: "out".to_string(),
                to_node: 99,
                to_port: "in".to_string(),
            }],
        };
        let snap = GraphSnapshot::from_def(&def).expect("from_def succeeds");
        let mut ids: Vec<u32> = snap.nodes.iter().map(|n| n.id).collect();
        ids.sort();
        assert_eq!(ids, vec![10, 99], "snap.nodes.id must be doc ids");
        assert_eq!(snap.wires.len(), 1);
        assert_eq!(snap.wires[0].from_node, 10);
        assert_eq!(snap.wires[0].to_node, 99);
    }

    #[test]
    fn from_def_returns_none_on_unknown_type_id() {
        use manifold_core::effect_graph_def::{EffectGraphDef, EffectGraphNode, EFFECT_GRAPH_VERSION};

        let bad_def = EffectGraphDef {
            version: EFFECT_GRAPH_VERSION,
            name: None,
            description: None,
            preset_metadata: None,
            nodes: vec![EffectGraphNode {
                id: 0,
                type_id: "node.does_not_exist".to_string(),
                handle: None,
                params: Default::default(),
                exposed_params: Default::default(),
                editor_pos: None,
                wgsl_source: None,
                title: None,
                output_formats: Default::default(),
                output_canvas_scales: std::collections::BTreeMap::new(),
            }],
            wires: Vec::new(),
        };
        assert!(GraphSnapshot::from_def(&bad_def).is_none());
    }

    // ─── Phase 6: snapshot carries Channels signature ────────────────

    #[test]
    fn array_port_snapshot_carries_channels_from_array_type() {
        use crate::node_graph::channel_names::well_known;
        use crate::node_graph::ports::{
            ArrayType, ChannelElementType, ChannelSpec, MatchMode, PortType,
        };

        // Synthesize an ArrayType with a known Channels signature and
        // round-trip it through the From<PortType> conversion.
        const SPECS: &[ChannelSpec] = &[
            ChannelSpec { name: well_known::POSITION, ty: ChannelElementType::Vec3F },
            ChannelSpec { name: well_known::COLOR,    ty: ChannelElementType::Vec4F },
        ];
        let port_type =
            PortType::Array(ArrayType::of_channels(SPECS, MatchMode::Exact));
        let snap = PortKindSnapshot::from(port_type);
        match snap {
            PortKindSnapshot::Array { channels, match_mode, item_size, item_align } => {
                assert_eq!(channels.len(), 2);
                assert_eq!(channels[0].name, "position");
                assert_eq!(channels[0].ty, "Vec3F");
                assert_eq!(channels[1].name, "color");
                assert_eq!(channels[1].ty, "Vec4F");
                assert_eq!(match_mode, ArrayMatchMode::Exact);
                // std430 stride for [Vec3F, Vec4F]: Vec3F at 0 (size 12,
                // align 16), Vec4F at 16 (align 16) → stride 32.
                assert_eq!(item_size, 32);
                assert_eq!(item_align, 16);
            }
            other => panic!("expected Array snapshot, got {other:?}"),
        }
    }

    #[test]
    fn permissive_array_port_snapshot_tags_match_mode() {
        use crate::node_graph::ports::{ArrayType, MatchMode, PortType};

        let port_type = PortType::Array(ArrayType {
            item_size: 0,
            item_align: 4,
            specs: &[],
            match_mode: MatchMode::Permissive,
        });
        let snap = PortKindSnapshot::from(port_type);
        match snap {
            PortKindSnapshot::Array { channels, match_mode, .. } => {
                assert!(channels.is_empty());
                assert_eq!(match_mode, ArrayMatchMode::Permissive);
            }
            other => panic!("expected Array snapshot, got {other:?}"),
        }
    }

    #[test]
    fn unknown_channel_name_falls_back_to_hex_hash() {
        use crate::node_graph::ports::{
            ArrayType, ChannelElementType, ChannelName, ChannelSpec, MatchMode, PortType,
        };

        // A channel name not in the well_known registry — debug_name
        // returns None and the snapshot formats the hash as hex.
        let local = ChannelName::from_str("internal_debug_counter");
        let specs: &'static [ChannelSpec] = Box::leak(Box::new([
            ChannelSpec { name: local, ty: ChannelElementType::U32 },
        ]));
        let port_type =
            PortType::Array(ArrayType::of_channels(specs, MatchMode::Exact));
        let snap = PortKindSnapshot::from(port_type);
        match snap {
            PortKindSnapshot::Array { channels, .. } => {
                assert_eq!(channels.len(), 1);
                assert!(
                    channels[0].name.starts_with("0x"),
                    "expected hex fallback name, got {:?}",
                    channels[0].name
                );
                assert_eq!(channels[0].ty, "U32");
            }
            other => panic!("expected Array snapshot, got {other:?}"),
        }
    }
}
