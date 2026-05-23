//! Graph-JSON persistence — converts between live [`Graph`]s and the
//! on-disk schema in [`manifold_core::effect_graph_def`].
//!
//! The schema itself (`EffectGraphDef`, `EffectGraphNode`,
//! `EffectGraphWire`, `SerializedParamValue`) lives in `manifold-core`
//! so [`manifold_core::effects::EffectInstance`] can carry a
//! per-instance graph by value. This module owns the runtime-coupled
//! pieces: the [`PrimitiveRegistry`] that maps `type_id` strings to
//! node constructors, the [`ParamValue`] ↔ [`SerializedParamValue`]
//! conversions, and the [`from_graph`](EffectGraphDefExt::from_graph)
//! / [`into_graph`](EffectGraphDefExt::into_graph) entry points.
//!
//! Both bundled effect presets (`assets/effect-presets/*.json`) and
//! user-authored per-instance overrides use this same shape. The
//! format is intentionally minimal: a list of nodes (each with a
//! stable `type_id`, parameter values, and optional editor position)
//! plus a list of wires. No execution metadata, no resource ids —
//! those are recomputed from the live graph at runtime.
//!
//! ## Round-trip
//!
//! [`EffectGraphDefExt::from_graph`] serializes a live [`Graph`].
//! [`EffectGraphDefExt::into_graph`] materializes a def back into a
//! live [`Graph`] using a [`PrimitiveRegistry`] to look up node
//! constructors by `type_id`. The two are inverses (modulo
//! constructor-supplied defaults vs. the explicit per-param values the
//! def records).
//!
//! ## Versioning
//!
//! `EffectGraphDef::version` starts at `1`. When the schema needs a
//! breaking change, bump
//! [`manifold_core::effect_graph_def::EFFECT_GRAPH_VERSION`] and add a
//! migrator.

use std::collections::BTreeMap;

use ahash::AHashMap;

use manifold_core::effect_graph_def::{
    EFFECT_GRAPH_VERSION, EFFECT_GRAPH_VERSION_WITH_METADATA, EffectGraphDef, EffectGraphNode,
    EffectGraphWire,
};

use crate::node_graph::effect_node::{EffectNode, NodeInstanceId};
use crate::node_graph::graph::Graph;
use crate::node_graph::parameters::ParamValue;

// Re-export the core schema under the legacy renderer-side names so
// existing call sites keep compiling. New code should prefer the
// `EffectGraphDef` names directly from `manifold_core`. The
// `SerializedParamValue` re-export is also what brings the type into
// scope for this module's `From<ParamValue>` impls below.
pub use manifold_core::effect_graph_def::SerializedParamValue;
pub use manifold_core::effect_graph_def::{
    EFFECT_GRAPH_VERSION as GRAPH_DOCUMENT_VERSION, EffectGraphDef as GraphDocument,
    EffectGraphNode as NodeDocument, EffectGraphWire as WireDocument,
};

impl From<ParamValue> for SerializedParamValue {
    fn from(v: ParamValue) -> Self {
        // Always emit `Float` for numerics — `Int` lives in the schema for
        // back-compat reads only (see the reverse impl). New saves are
        // type-collapsed; readers should expect `Float` everywhere.
        match v {
            ParamValue::Float(value) => Self::Float { value },
            ParamValue::Bool(value) => Self::Bool { value },
            ParamValue::Vec2(value) => Self::Vec2 { value },
            ParamValue::Vec3(value) => Self::Vec3 { value },
            ParamValue::Vec4(value) => Self::Vec4 { value },
            ParamValue::Color(value) => Self::Color { value },
            ParamValue::Enum(value) => Self::Enum { value },
            ParamValue::Table(t) => Self::Table {
                rows: t.rows().to_vec(),
            },
            ParamValue::String(s) => Self::String {
                value: (*s).clone(),
            },
        }
    }
}

impl From<SerializedParamValue> for ParamValue {
    fn from(v: SerializedParamValue) -> Self {
        // `Int` is preserved in the wire schema for back-compat with
        // existing saved projects and bundled-preset JSON. Coerce to
        // `Float` on read — in-memory storage no longer has a separate
        // integer variant. Precision is lossless for our param ranges.
        match v {
            SerializedParamValue::Float { value } => Self::Float(value),
            SerializedParamValue::Int { value } => Self::Float(value as f32),
            SerializedParamValue::Bool { value } => Self::Bool(value),
            SerializedParamValue::Vec2 { value } => Self::Vec2(value),
            SerializedParamValue::Vec3 { value } => Self::Vec3(value),
            SerializedParamValue::Vec4 { value } => Self::Vec4(value),
            SerializedParamValue::Color { value } => Self::Color(value),
            SerializedParamValue::Enum { value } => Self::Enum(value),
            SerializedParamValue::Table { rows } => {
                // Dimensionally-invalid tables fall back to a sentinel
                // 1×1 zero table — the load path already logs invalid
                // params; we don't want to crash a partially-valid graph.
                let data = crate::node_graph::parameters::TableData::new(rows)
                    .unwrap_or_else(|| {
                        crate::node_graph::parameters::TableData::new(vec![vec![0.0]]).unwrap()
                    });
                Self::Table(std::sync::Arc::new(data))
            }
            SerializedParamValue::String { value } => Self::String(std::sync::Arc::new(value)),
        }
    }
}

// ---------------------------------------------------------------------------
// Primitive registry
// ---------------------------------------------------------------------------

/// Constructor closure for one node kind. Returns a fresh boxed
/// [`EffectNode`] with default parameter values.
pub type NodeConstructor = Box<dyn Fn() -> Box<dyn EffectNode> + Send + Sync>;

/// Inventory entry for one primitive's factory.
///
/// Every shipping primitive submits exactly one of these via
/// `inventory::submit!`. [`register_builtin`] iterates the inventory
/// at startup to populate the [`PrimitiveRegistry`]; adding a new
/// primitive is a one-line `inventory::submit!` block in its own
/// file, no central list to update.
///
/// The `primitive!` macro emits this submission automatically — only
/// the hand-written primitives (multi-primitive files like
/// `primitives/color.rs`, plus the system boundary nodes and the
/// monolithic legacy wrappers) hand-write the submit block.
pub struct PrimitiveFactory {
    pub type_id: &'static str,
    pub create: fn() -> Box<dyn EffectNode>,
    /// Static picker metadata. `Some(_)` opts the primitive into the
    /// editor palette under the declared category and label; `None`
    /// hides it (boundary nodes, aesthetic effects shipped as cards,
    /// internal composite building blocks). The editor's
    /// `palette_atoms()` walks this inventory and surfaces every
    /// entry that opted in.
    pub picker: Option<crate::node_graph::palette::PickerInfo>,
}

inventory::collect!(PrimitiveFactory);

/// Registry mapping stable `type_id` strings to constructors. Built
/// once at startup via [`PrimitiveRegistry::with_builtin`] (covers
/// every shipping primitive + the `system.source` / `system.final_output`
/// boundaries); callers can layer additional constructors via
/// [`PrimitiveRegistry::register`] for tests.
pub struct PrimitiveRegistry {
    constructors: AHashMap<String, NodeConstructor>,
}

impl PrimitiveRegistry {
    /// An empty registry. Useful for tests; production code wants
    /// [`PrimitiveRegistry::with_builtin`].
    pub fn new() -> Self {
        Self {
            constructors: AHashMap::default(),
        }
    }

    /// A registry pre-populated with every shipping primitive and the
    /// two system boundary nodes. The single source of truth for which
    /// `type_id` strings are loadable.
    pub fn with_builtin() -> Self {
        let mut r = Self::new();
        register_builtin(&mut r);
        r
    }

    /// Add (or replace) a constructor for one `type_id`. Returns `self`
    /// so the builder pattern flows.
    pub fn register(
        &mut self,
        type_id: impl Into<String>,
        ctor: impl Fn() -> Box<dyn EffectNode> + Send + Sync + 'static,
    ) -> &mut Self {
        self.constructors.insert(type_id.into(), Box::new(ctor));
        self
    }

    /// Look up a constructor and build a fresh node, or `None` if the
    /// `type_id` isn't registered.
    pub fn construct(&self, type_id: &str) -> Option<Box<dyn EffectNode>> {
        self.constructors.get(type_id).map(|ctor| ctor())
    }

    /// Iterate every registered `type_id`. Order is unspecified.
    pub fn known_type_ids(&self) -> impl Iterator<Item = &str> + '_ {
        self.constructors.keys().map(|s| s.as_str())
    }

    pub fn contains(&self, type_id: &str) -> bool {
        self.constructors.contains_key(type_id)
    }
}

impl Default for PrimitiveRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Populate `r` with every shipping primitive + system boundaries.
///
/// Walks the [`PrimitiveFactory`] inventory channel. Every primitive
/// — macro-authored or hand-written — registers itself via a one-line
/// `inventory::submit!` block, so adding a new primitive needs no
/// edits here. The `primitive!` macro emits the submission
/// automatically; multi-primitive files and the system boundaries
/// hand-write theirs alongside the struct definition.
fn register_builtin(r: &mut PrimitiveRegistry) {
    for factory in inventory::iter::<PrimitiveFactory> {
        r.register(factory.type_id, factory.create);
    }
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Errors raised by [`GraphDocument::into_graph`].
#[derive(Debug, Clone, PartialEq)]
pub enum LoadError {
    /// Document `version` is newer than this binary understands.
    UnsupportedVersion { found: u32, max: u32 },
    /// Two nodes share the same document id.
    DuplicateNodeId(u32),
    /// Node's `type_id` isn't registered in the [`PrimitiveRegistry`].
    UnknownTypeId { node_id: u32, type_id: String },
    /// Wire references a node id that doesn't exist in the document.
    UnknownNodeRef {
        wire_index: usize,
        node_id: u32,
        side: WireSide,
    },
    /// Parameter override targets a name the node doesn't declare.
    UnknownParam {
        node_id: u32,
        type_id: String,
        param: String,
    },
    /// Parameter override's value type disagrees with the declared
    /// type (e.g., setting a `Float` param with an `Int` payload).
    ParamTypeMismatch {
        node_id: u32,
        type_id: String,
        param: String,
        expected: &'static str,
        got: &'static str,
    },
    /// Wire targets a port that doesn't exist or has the wrong kind /
    /// type on the receiving node.
    InvalidWire { wire_index: usize, reason: String },
    /// An `outputFormats` entry references a format string this build
    /// doesn't know about. The set of valid strings tracks
    /// [`manifold_gpu::GpuTextureFormat`] — see [`format_from_str`].
    UnknownOutputFormat {
        node_id: u32,
        type_id: String,
        port: String,
        format: String,
    },
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedVersion { found, max } => write!(
                f,
                "graph document version {found} is newer than this build supports (max {max})"
            ),
            Self::DuplicateNodeId(id) => write!(f, "duplicate node id {id} in document"),
            Self::UnknownTypeId { node_id, type_id } => write!(
                f,
                "node {node_id}: unknown type id '{type_id}' (not in PrimitiveRegistry)"
            ),
            Self::UnknownNodeRef {
                wire_index,
                node_id,
                side,
            } => write!(
                f,
                "wire #{wire_index}: {side:?} references unknown node id {node_id}"
            ),
            Self::UnknownParam {
                node_id,
                type_id,
                param,
            } => write!(f, "node {node_id} ({type_id}): unknown parameter '{param}'"),
            Self::ParamTypeMismatch {
                node_id,
                type_id,
                param,
                expected,
                got,
            } => write!(
                f,
                "node {node_id} ({type_id}): parameter '{param}' expected {expected}, got {got}"
            ),
            Self::InvalidWire { wire_index, reason } => {
                write!(f, "wire #{wire_index}: {reason}")
            }
            Self::UnknownOutputFormat {
                node_id,
                type_id,
                port,
                format,
            } => write!(
                f,
                "node {node_id} ({type_id}): output '{port}' declares unknown format '{format}'"
            ),
        }
    }
}

/// Map a [`manifold_gpu::GpuTextureFormat`] to the JSON wire string.
/// Inverse of [`format_from_str`]. Keep the two functions in sync —
/// they're the round-trip contract for the `outputFormats` map.
pub fn format_to_str(fmt: manifold_gpu::GpuTextureFormat) -> &'static str {
    use manifold_gpu::GpuTextureFormat::*;
    match fmt {
        Rgba16Float => "rgba16float",
        Rgba32Float => "rgba32float",
        Rgba8Unorm => "rgba8unorm",
        R32Float => "r32float",
        Rg32Float => "rg32float",
        R16Float => "r16float",
        Rg16Float => "rg16float",
        R32Uint => "r32uint",
        Rgba8UnormSrgb => "rgba8unorm_srgb",
        Bgra8Unorm => "bgra8unorm",
        Bgra8UnormSrgb => "bgra8unorm_srgb",
        R8Unorm => "r8unorm",
        Depth32Float => "depth32float",
    }
}

/// Parse the JSON `outputFormats` value string. Returns `None` for an
/// unrecognized name — the loader surfaces a clean `LoadError` instead
/// of silently falling back to a default. Strings match WGSL /
/// `texture_storage_2d<...>` conventions.
pub fn format_from_str(s: &str) -> Option<manifold_gpu::GpuTextureFormat> {
    use manifold_gpu::GpuTextureFormat::*;
    Some(match s {
        "rgba16float" => Rgba16Float,
        "rgba32float" => Rgba32Float,
        "rgba8unorm" => Rgba8Unorm,
        "r32float" => R32Float,
        "rg32float" => Rg32Float,
        "r16float" => R16Float,
        "rg16float" => Rg16Float,
        "r32uint" => R32Uint,
        "rgba8unorm_srgb" => Rgba8UnormSrgb,
        "bgra8unorm" => Bgra8Unorm,
        "bgra8unorm_srgb" => Bgra8UnormSrgb,
        "r8unorm" => R8Unorm,
        "depth32float" => Depth32Float,
        _ => return None,
    })
}

impl std::error::Error for LoadError {}

/// Which side of a wire failed to resolve.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WireSide {
    From,
    To,
}

// ---------------------------------------------------------------------------
// Save: Graph → EffectGraphDef
// Load: EffectGraphDef → Graph
//
// `EffectGraphDef` is a foreign type (in `manifold-core`), so we expose
// these via an extension trait. Existing call sites that use
// `GraphDocument::from_graph(&g)` / `doc.into_graph(&registry)` continue
// to work because the trait is re-exported from this module.
// ---------------------------------------------------------------------------

/// Extension trait that adds [`Graph`] round-trip methods to
/// [`EffectGraphDef`]. The trait must be in scope at the call site —
/// it's re-exported from this module's parent.
pub trait EffectGraphDefExt: Sized {
    /// Serialize a live [`Graph`] to a definition. Captures every
    /// node's current parameter values (defaults that haven't been
    /// overridden are still written — round-trip equality matters more
    /// than document terseness).
    fn from_graph(graph: &Graph) -> Self;

    /// Materialize a definition into a live [`Graph`] using `registry`
    /// to look up node constructors by `type_id`.
    ///
    /// On failure, returns the first [`LoadError`] encountered.
    /// Partial graphs are never returned — the document is parsed in
    /// two passes (build nodes, then wire) so a wire error doesn't
    /// leak half-built state.
    fn into_graph(self, registry: &PrimitiveRegistry) -> Result<Graph, LoadError>;
}

impl EffectGraphDefExt for EffectGraphDef {
    fn from_graph(graph: &Graph) -> Self {
        let id_to_handle: AHashMap<u32, String> = graph
            .handles()
            .map(|(h, id)| (id.0, h.to_string()))
            .collect();

        let mut nodes: Vec<EffectGraphNode> = graph
            .nodes()
            .map(|inst| {
                let mut params = BTreeMap::new();
                for def in inst.node.parameters() {
                    let value = inst
                        .params
                        .get(def.name)
                        .cloned()
                        .unwrap_or_else(|| def.default.clone());
                    params.insert(def.name.to_string(), value.into());
                }
                // Walk outputs and serialize any node-declared format
                // overrides. Most nodes return `None` and the map stays
                // empty — only `node.wgsl_compute_*` nodes that an
                // author explicitly configured for native precision
                // carry entries here.
                let mut output_formats = BTreeMap::new();
                for out in inst.node.outputs() {
                    if let Some(fmt) = inst.node.output_format(out.name) {
                        output_formats.insert(out.name.to_string(), format_to_str(fmt).to_string());
                    }
                }
                EffectGraphNode {
                    id: inst.id.0,
                    type_id: inst.node.type_id().as_str().to_string(),
                    handle: id_to_handle.get(&inst.id.0).cloned(),
                    params,
                    exposed_params: inst
                        .exposed_params
                        .iter()
                        .map(|s| (*s).to_string())
                        .collect(),
                    editor_pos: None,
                    wgsl_source: inst.node.wgsl_source().map(|s| s.to_string()),
                    output_formats,
                }
            })
            .collect();
        // Stable order so saved documents diff cleanly across rebuilds.
        nodes.sort_by_key(|n| n.id);

        let mut wires: Vec<EffectGraphWire> = graph
            .wires()
            .iter()
            .map(|w| EffectGraphWire {
                from_node: w.from.0.0,
                from_port: w.from.1.to_string(),
                to_node: w.to.0.0,
                to_port: w.to.1.to_string(),
            })
            .collect();
        wires.sort_by(|a, b| {
            a.to_node
                .cmp(&b.to_node)
                .then(a.to_port.cmp(&b.to_port))
                .then(a.from_node.cmp(&b.from_node))
                .then(a.from_port.cmp(&b.from_port))
        });

        EffectGraphDef {
            version: EFFECT_GRAPH_VERSION,
            name: None,
            description: None,
            preset_metadata: None,
            nodes,
            wires,
        }
    }

    fn into_graph(self, registry: &PrimitiveRegistry) -> Result<Graph, LoadError> {
        if self.version > EFFECT_GRAPH_VERSION_WITH_METADATA {
            return Err(LoadError::UnsupportedVersion {
                found: self.version,
                max: EFFECT_GRAPH_VERSION_WITH_METADATA,
            });
        }

        let mut graph = Graph::new();
        // doc_id → runtime NodeInstanceId
        let mut id_map: AHashMap<u32, NodeInstanceId> = AHashMap::default();

        for node_doc in &self.nodes {
            if id_map.contains_key(&node_doc.id) {
                return Err(LoadError::DuplicateNodeId(node_doc.id));
            }
            let Some(boxed) = registry.construct(&node_doc.type_id) else {
                return Err(LoadError::UnknownTypeId {
                    node_id: node_doc.id,
                    type_id: node_doc.type_id.clone(),
                });
            };

            // Find the static param names + types so we can validate
            // overrides against the declared shape and resolve the
            // `&'static str` keys `Graph::set_param` expects.
            //
            // Validation reads the declared `ParamType`, not the default
            // value's variant. Table-typed params ship a `Float(0.0)`
            // sentinel default (Arc isn't const-constructible) — falling
            // back to the variant would reject every real Table value
            // at load time.
            let param_defs: Vec<(&'static str, crate::node_graph::parameters::ParamType)> = boxed
                .parameters()
                .iter()
                .map(|p| (p.name, p.ty))
                .collect();

            let runtime_id = if let Some(handle) = node_doc.handle.as_deref() {
                // `add_node_named` needs a `&'static str`; we can't
                // promote a runtime String, so we lean on `Box::leak`
                // here. Handles are document-author-defined and
                // bounded — at most one leak per inner node per
                // loaded preset, never on the per-frame path.
                let static_handle: &'static str = Box::leak(handle.to_string().into_boxed_str());
                graph.add_node_named(static_handle, boxed)
            } else {
                graph.add_node(boxed)
            };
            id_map.insert(node_doc.id, runtime_id);

            // Apply parameter overrides.
            for (key, value) in &node_doc.params {
                let Some(&(name_static, expected_ty)) =
                    param_defs.iter().find(|(n, _)| *n == key.as_str())
                else {
                    return Err(LoadError::UnknownParam {
                        node_id: node_doc.id,
                        type_id: node_doc.type_id.clone(),
                        param: key.clone(),
                    });
                };
                let pv: ParamValue = value.clone().into();
                if !param_value_matches_type(&pv, expected_ty) {
                    return Err(LoadError::ParamTypeMismatch {
                        node_id: node_doc.id,
                        type_id: node_doc.type_id.clone(),
                        param: key.clone(),
                        expected: param_type_name(expected_ty),
                        got: param_type_label(&pv),
                    });
                }
                // set_param can only fail with NodeNotFound (we just
                // added it) or ParamNotFound (we just validated the
                // name). Both impossible here.
                graph
                    .set_param(runtime_id, name_static, pv)
                    .expect("validated above");
            }

            // Install the WGSL kernel source if the document carries one.
            // No-op for every node whose shader is fixed at compile time.
            if let Some(source) = node_doc.wgsl_source.as_deref() {
                graph
                    .set_wgsl_source(runtime_id, source)
                    .expect("node was just added");
            }

            // Apply exposed_params from the document. Unknown param
            // names are silently ignored — a renamed param shouldn't
            // refuse to load just because an old saved exposure points
            // at a defunct name. Validates via the param_defs scan so
            // the static-lifetime name lookup matches what the live
            // node declares.
            for exposed_name in &node_doc.exposed_params {
                if let Some(&(name_static, _)) =
                    param_defs.iter().find(|(n, _)| *n == exposed_name.as_str())
                {
                    graph
                        .set_param_exposed(runtime_id, name_static, true)
                        .expect("validated above");
                }
            }

            // Install any per-output format overrides. Most documents
            // carry no entries; the native-precision escape-hatch
            // family is where these matter.
            for (port_name, fmt_str) in &node_doc.output_formats {
                let Some(fmt) = format_from_str(fmt_str) else {
                    return Err(LoadError::UnknownOutputFormat {
                        node_id: node_doc.id,
                        type_id: node_doc.type_id.clone(),
                        port: port_name.clone(),
                        format: fmt_str.clone(),
                    });
                };
                graph
                    .set_output_format(runtime_id, port_name, fmt)
                    .expect("node was just added");
            }
        }

        for (i, wire) in self.wires.iter().enumerate() {
            let from_runtime = *id_map
                .get(&wire.from_node)
                .ok_or(LoadError::UnknownNodeRef {
                    wire_index: i,
                    node_id: wire.from_node,
                    side: WireSide::From,
                })?;
            let to_runtime = *id_map.get(&wire.to_node).ok_or(LoadError::UnknownNodeRef {
                wire_index: i,
                node_id: wire.to_node,
                side: WireSide::To,
            })?;

            let from_port: &'static str =
                leak_port_name(&wire.from_port, &graph, from_runtime, true)?.ok_or_else(|| {
                    LoadError::InvalidWire {
                        wire_index: i,
                        reason: format!(
                            "from node {} has no output port '{}'",
                            wire.from_node, wire.from_port
                        ),
                    }
                })?;
            let to_port: &'static str = leak_port_name(&wire.to_port, &graph, to_runtime, false)?
                .ok_or_else(|| LoadError::InvalidWire {
                wire_index: i,
                reason: format!(
                    "to node {} has no input port '{}'",
                    wire.to_node, wire.to_port
                ),
            })?;

            graph
                .connect((from_runtime, from_port), (to_runtime, to_port))
                .map_err(|e| LoadError::InvalidWire {
                    wire_index: i,
                    reason: format!("{e:?}"),
                })?;
        }

        // Step 6 (exposure unification): seed `exposed_params` on the
        // live graph from `preset_metadata.bindings`. Each binding's
        // target param becomes exposed by default.
        //
        // This is the implicit-default path: it ONLY fires when the
        // document carries NO explicit `exposed_params` entries on
        // any node. Once the user toggles an exposure via
        // `ToggleNodeParamExposeCommand`, the command materialises
        // the full preset-driven defaults into the def
        // (`materialize_binding_exposures`) and the def becomes its
        // own source of truth. At that point this backfill becomes a
        // no-op (the explicit set wins), so a user's UNCHECK on a
        // preset-bound param sticks across save/reload.
        let has_explicit_exposures = self
            .nodes
            .iter()
            .any(|n| !n.exposed_params.is_empty());
        if !has_explicit_exposures
            && let Some(meta) = self.preset_metadata.as_ref()
        {
            use manifold_core::effect_graph_def::BindingTarget;
            for binding in &meta.bindings {
                if let BindingTarget::HandleNode { handle, param } = &binding.target
                    && let Some(node_id) = graph.node_id_by_handle(handle)
                {
                    let static_name = graph.get_node(node_id).and_then(|inst| {
                        inst.node
                            .parameters()
                            .iter()
                            .find(|p| p.name == param.as_str())
                            .map(|p| p.name)
                    });
                    if let Some(name_static) = static_name {
                        let _ = graph.set_param_exposed(node_id, name_static, true);
                    }
                }
            }
        }

        Ok(graph)
    }
}

/// Tag for `LoadError::ParamTypeMismatch` `expected`/`got` fields.
fn param_type_label(v: &ParamValue) -> &'static str {
    match v {
        ParamValue::Float(_) => "Float",
        ParamValue::Bool(_) => "Bool",
        ParamValue::Vec2(_) => "Vec2",
        ParamValue::Vec3(_) => "Vec3",
        ParamValue::Vec4(_) => "Vec4",
        ParamValue::Color(_) => "Color",
        ParamValue::Enum(_) => "Enum",
        ParamValue::Table(_) => "Table",
        ParamValue::String(_) => "String",
    }
}

/// Tag for the declared `ParamType` side of a mismatch error.
fn param_type_name(ty: crate::node_graph::parameters::ParamType) -> &'static str {
    use crate::node_graph::parameters::ParamType;
    match ty {
        ParamType::Float => "Float",
        ParamType::Int => "Int",
        ParamType::Bool => "Bool",
        ParamType::Vec2 => "Vec2",
        ParamType::Vec3 => "Vec3",
        ParamType::Vec4 => "Vec4",
        ParamType::Color => "Color",
        ParamType::Enum => "Enum",
        ParamType::Table => "Table",
        ParamType::String => "String",
    }
}

/// Whether an incoming `ParamValue` is a valid override for a declared
/// [`ParamType`]. The Int collapse means `ParamType::Int` accepts a
/// `ParamValue::Float` (storage is `Float` only since the Int variant
/// was removed). Everything else is variant-for-variant.
fn param_value_matches_type(v: &ParamValue, ty: crate::node_graph::parameters::ParamType) -> bool {
    use crate::node_graph::parameters::ParamType;
    matches!(
        (ty, v),
        (ParamType::Float, ParamValue::Float(_))
            | (ParamType::Int, ParamValue::Float(_))
            | (ParamType::Bool, ParamValue::Bool(_))
            | (ParamType::Vec2, ParamValue::Vec2(_))
            | (ParamType::Vec3, ParamValue::Vec3(_))
            | (ParamType::Vec4, ParamValue::Vec4(_))
            | (ParamType::Color, ParamValue::Color(_))
            | (ParamType::Enum, ParamValue::Enum(_))
            | (ParamType::Table, ParamValue::Table(_))
            | (ParamType::String, ParamValue::String(_))
    )
}

/// Resolve a port name on a node by matching against the declared
/// `&'static str` ports, so the resulting reference is the `'static`
/// the [`Graph::connect`] API requires. Returns `None` if the named
/// port doesn't exist on the node.
///
/// Note: the returned reference is the node's own declared static
/// string, NOT a copy of `requested` — no leaks here.
fn leak_port_name(
    requested: &str,
    graph: &Graph,
    node_id: NodeInstanceId,
    output: bool,
) -> Result<Option<&'static str>, LoadError> {
    let Some(inst) = graph.get_node(node_id) else {
        // The caller already mapped doc_id → runtime id from
        // `id_map`, so this branch is unreachable for well-formed
        // loaders. Return None and let the caller raise InvalidWire.
        return Ok(None);
    };
    let port = if output {
        inst.node.outputs().iter().find(|p| p.name == requested)
    } else {
        inst.node.inputs().iter().find(|p| p.name == requested)
    };
    Ok(port.map(|p| p.name))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::boundary_nodes::{
        FINAL_OUTPUT_TYPE_ID, FinalOutput, SOURCE_TYPE_ID, Source,
    };
    use crate::node_graph::primitives::{self, Blur, MipChain, Threshold};
    use crate::node_graph::{compile, validate};

    fn registry() -> PrimitiveRegistry {
        PrimitiveRegistry::with_builtin()
    }

    #[test]
    fn builtin_registry_covers_every_shipped_primitive() {
        let r = registry();
        // Spot-check every category. Each primitive declares a public
        // `*_TYPE_ID` constant; if a new primitive is added without a
        // register call, this test fails.
        let expected: &[&str] = &[
            SOURCE_TYPE_ID,
            FINAL_OUTPUT_TYPE_ID,
            primitives::BRIGHTNESS_TYPE_ID,
            primitives::CHANNEL_MIX_TYPE_ID,
            primitives::COLOR_RAMP_TYPE_ID,
            primitives::MIX_TYPE_ID,
            primitives::BLEND_TYPE_ID,
            primitives::THRESHOLD_TYPE_ID,
            primitives::BLUR_TYPE_ID,
            primitives::MIP_CHAIN_TYPE_ID,
            primitives::GAUSSIAN_BLUR_TYPE_ID,
            primitives::TRANSFORM_TYPE_ID,
            primitives::SAMPLE_TYPE_ID,
            primitives::FEEDBACK_TYPE_ID,
            primitives::WET_DRY_TYPE_ID,
            primitives::BLOOM_TYPE_ID,
            primitives::HALATION_TYPE_ID,
            primitives::WATERCOLOR_TYPE_ID,
            primitives::DEPTH_OF_FIELD_TYPE_ID,
            primitives::AUTO_GAIN_TYPE_ID,
            primitives::BLOB_TRACKING_TYPE_ID,
            primitives::WIREFRAME_DEPTH_TYPE_ID,
            primitives::INFRARED_TYPE_ID,
            primitives::QUAD_MIRROR_TYPE_ID,
        ];
        for id in expected {
            assert!(
                r.contains(id),
                "PrimitiveRegistry missing constructor for '{id}'"
            );
        }
    }

    #[test]
    fn round_trip_bloom_like_three_node_graph() {
        let mut g = Graph::new();
        let src = g.add_node(Box::new(Source::new()));
        let thresh = g.add_node_named("thresh", Box::new(Threshold::new()));
        let out = g.add_node(Box::new(FinalOutput::new()));
        g.connect((src, "out"), (thresh, "source")).unwrap();
        g.connect((thresh, "out"), (out, "in")).unwrap();
        g.set_param(thresh, "level", ParamValue::Float(0.8))
            .unwrap();
        g.set_param(thresh, "softness", ParamValue::Float(0.05))
            .unwrap();

        // Serialize.
        let doc = GraphDocument::from_graph(&g);
        assert_eq!(doc.version, GRAPH_DOCUMENT_VERSION);
        assert_eq!(doc.nodes.len(), 3);
        assert_eq!(doc.wires.len(), 2);

        // Round-trip through JSON to catch missing serde derives.
        let json = serde_json::to_string(&doc).unwrap();
        let parsed: GraphDocument = serde_json::from_str(&json).unwrap();

        let g2 = parsed.into_graph(&registry()).unwrap();
        assert_eq!(g2.node_count(), 3);
        assert_eq!(g2.wires().len(), 2);
        // Named handle survives.
        let thresh2 = g2
            .node_id_by_handle("thresh")
            .expect("handle 'thresh' round-tripped");
        let inst = g2.get_node(thresh2).unwrap();
        // Param value survives.
        assert_eq!(
            inst.params.get("level").cloned().unwrap(),
            ParamValue::Float(0.8)
        );
        assert_eq!(
            inst.params.get("softness").cloned().unwrap(),
            ParamValue::Float(0.05)
        );

        // Reloaded graph validates + compiles.
        validate(&g2).unwrap();
        let plan = compile(&g2).unwrap();
        assert!(!plan.steps().is_empty());
    }

    /// `exposed_params` on each `EffectGraphNode` round-trips through
    /// the JSON document. Confirms the graph editor's "Expose to card"
    /// state survives save → reload — the unified exposure source of
    /// truth (Step 1 + Step 2 of the exposure-unification plan).
    #[test]
    fn exposed_params_round_trip_through_json() {
        let mut g = Graph::new();
        let _src = g.add_node(Box::new(Source::new()));
        let thresh = g.add_node_named("thresh", Box::new(Threshold::new()));
        let out = g.add_node(Box::new(FinalOutput::new()));
        g.connect((_src, "out"), (thresh, "source")).unwrap();
        g.connect((thresh, "out"), (out, "in")).unwrap();
        // Expose two params on the threshold node.
        g.set_param_exposed(thresh, "level", true).unwrap();
        g.set_param_exposed(thresh, "softness", true).unwrap();
        assert!(g.is_param_exposed(thresh, "level"));
        assert!(g.is_param_exposed(thresh, "softness"));

        // Serialize → deserialize.
        let doc = GraphDocument::from_graph(&g);
        let json = serde_json::to_string(&doc).unwrap();
        let parsed: GraphDocument = serde_json::from_str(&json).unwrap();

        // Confirm the document carries the exposure set.
        let thresh_doc = parsed
            .nodes
            .iter()
            .find(|n| n.handle.as_deref() == Some("thresh"))
            .unwrap();
        assert!(thresh_doc.exposed_params.contains("level"));
        assert!(thresh_doc.exposed_params.contains("softness"));

        // Confirm the live graph mirror picks them back up.
        let g2 = parsed.into_graph(&registry()).unwrap();
        let thresh2 = g2.node_id_by_handle("thresh").unwrap();
        assert!(g2.is_param_exposed(thresh2, "level"));
        assert!(g2.is_param_exposed(thresh2, "softness"));
        // Unset params remain unexposed.
        let final2 = g2.nodes().find(|n| n.id != thresh2).unwrap();
        assert!(!g2.is_param_exposed(final2.id, "any_param"));
    }

    /// WGSL escape-hatch primitives round-trip their `wgsl_source` field
    /// through the JSON document — the field is identity-level config of
    /// the node (alongside `editor_pos`), NOT a parameter. Confirms that
    /// `from_graph` reads it via `EffectNode::wgsl_source()` and
    /// `into_graph` re-installs it via `Graph::set_wgsl_source`.
    #[test]
    fn wgsl_compute_source_field_round_trips_through_json() {
        use crate::node_graph::primitives::{
            DEFAULT_WGSL_1TEX_1TEX, WgslCompute1Tex1Tex,
        };

        let mut g = Graph::new();
        let src = g.add_node(Box::new(Source::new()));
        let wgsl = g.add_node_named("kernel", Box::new(WgslCompute1Tex1Tex::new()));
        let out = g.add_node(Box::new(FinalOutput::new()));
        g.connect((src, "out"), (wgsl, "in")).unwrap();
        g.connect((wgsl, "out"), (out, "in")).unwrap();

        // Install a custom source. (Any valid WGSL kernel matching the
        // 1tex_1tex contract.)
        let custom_source = "// agent-authored kernel\n".to_string()
            + DEFAULT_WGSL_1TEX_1TEX;
        g.set_wgsl_source(wgsl, &custom_source).unwrap();

        // Round-trip through JSON.
        let doc = GraphDocument::from_graph(&g);
        let json = serde_json::to_string(&doc).unwrap();
        assert!(
            json.contains("wgslSource"),
            "JSON must carry the wgslSource field; got: {json}"
        );
        let parsed: GraphDocument = serde_json::from_str(&json).unwrap();
        let g2 = parsed.into_graph(&registry()).unwrap();

        // Source survives the round trip.
        let kernel_id = g2.node_id_by_handle("kernel").expect("handle survived");
        let inst = g2.get_node(kernel_id).unwrap();
        assert_eq!(
            inst.node.wgsl_source(),
            Some(custom_source.as_str()),
            "wgsl_source must round-trip through into_graph",
        );
    }

    /// `outputFormats` on a `node.wgsl_compute_*` node carries through
    /// `from_graph` → JSON → `into_graph` and lands on the rebuilt
    /// node's `output_format(port)` accessor. Validates the full D5
    /// path end to end at the persistence layer.
    #[test]
    fn output_format_override_round_trips_through_json() {
        use crate::node_graph::primitives::WgslCompute1Tex1Tex;

        let mut g = Graph::new();
        let src = g.add_node(Box::new(Source::new()));
        let wgsl = g.add_node_named("kernel", Box::new(WgslCompute1Tex1Tex::new()));
        let out = g.add_node(Box::new(FinalOutput::new()));
        g.connect((src, "out"), (wgsl, "in")).unwrap();
        g.connect((wgsl, "out"), (out, "in")).unwrap();

        // Configure native-precision output.
        g.set_output_format(wgsl, "out", manifold_gpu::GpuTextureFormat::Rgba32Float)
            .unwrap();

        let doc = GraphDocument::from_graph(&g);
        let json = serde_json::to_string(&doc).unwrap();
        assert!(
            json.contains("outputFormats") && json.contains("rgba32float"),
            "JSON must carry the outputFormats map with the format string; got: {json}"
        );

        let parsed: GraphDocument = serde_json::from_str(&json).unwrap();
        let g2 = parsed.into_graph(&registry()).unwrap();
        let kernel_id = g2.node_id_by_handle("kernel").expect("handle survived");
        let inst = g2.get_node(kernel_id).unwrap();
        assert_eq!(
            inst.node.output_format("out"),
            Some(manifold_gpu::GpuTextureFormat::Rgba32Float),
            "output format override must round-trip through into_graph",
        );
    }

    /// An unknown format string surfaces as a clean `LoadError` rather
    /// than silently falling back to the default — defends against
    /// typos and lets the editor surface the bad node to the author.
    #[test]
    fn unknown_output_format_surfaces_as_load_error() {
        let mut output_formats = BTreeMap::new();
        output_formats.insert("out".to_string(), "made_up_format".to_string());
        let doc = GraphDocument {
            version: 1,
            name: None,
            description: None,
            preset_metadata: None,
            nodes: vec![NodeDocument {
                id: 0,
                type_id: "node.wgsl_compute_0in_1tex".to_string(),
                handle: None,
                params: BTreeMap::new(),
                exposed_params: Default::default(),
                editor_pos: None,
                wgsl_source: None,
                output_formats,
            }],
            wires: vec![],
        };
        let err = expect_err(doc.into_graph(&registry()));
        match err {
            LoadError::UnknownOutputFormat { format, port, .. } => {
                assert_eq!(format, "made_up_format");
                assert_eq!(port, "out");
            }
            other => panic!("expected UnknownOutputFormat, got {other:?}"),
        }
    }

    /// The producer's declared format propagates through `compile()` and
    /// is queryable via `plan.resource_format(id)`. This is the
    /// compile-time piece — the runtime piece (backend allocates at
    /// that format) is exercised by the backend's
    /// `texture2d_pools_separately_by_format` test.
    #[test]
    fn declared_output_format_lands_in_compiled_plan() {
        use crate::node_graph::primitives::WgslCompute0In1Tex;

        let mut g = Graph::new();
        let producer = g.add_node(Box::new(WgslCompute0In1Tex::new()));
        let out = g.add_node(Box::new(FinalOutput::new()));
        g.connect((producer, "out"), (out, "in")).unwrap();
        g.set_output_format(producer, "out", manifold_gpu::GpuTextureFormat::R32Float)
            .unwrap();

        let plan = crate::node_graph::compile(&g).unwrap();
        // Two outputs in the graph: WgslCompute0In1Tex.out (Texture2D,
        // declared r32float) and FinalOutput's outputs (none — it's a
        // sink). The first resource is the generator's `out`.
        assert_eq!(
            plan.resource_format(crate::node_graph::execution_plan::ResourceId(0)),
            Some(manifold_gpu::GpuTextureFormat::R32Float),
        );
    }

    #[test]
    fn unknown_type_id_is_a_clean_error() {
        let doc = GraphDocument {
            version: 1,
            name: None,
            description: None,
            preset_metadata: None,
            nodes: vec![NodeDocument {
                id: 0,
                type_id: "node.does_not_exist".to_string(),
                handle: None,
                params: BTreeMap::new(),
                exposed_params: Default::default(),
                editor_pos: None,
                wgsl_source: None,
                output_formats: BTreeMap::new(),
            }],
            wires: vec![],
        };
        let err = expect_err(doc.into_graph(&registry()));
        match err {
            LoadError::UnknownTypeId { node_id, type_id } => {
                assert_eq!(node_id, 0);
                assert_eq!(type_id, "node.does_not_exist");
            }
            other => panic!("expected UnknownTypeId, got {other:?}"),
        }
    }

    /// `Graph` doesn't impl `Debug`, so `Result::unwrap_err` won't
    /// compile against it. Replace with a small helper for these
    /// failing-load tests.
    fn expect_err(result: Result<Graph, LoadError>) -> LoadError {
        match result {
            Ok(_) => panic!("expected LoadError, got Ok(Graph)"),
            Err(e) => e,
        }
    }

    #[test]
    fn invalid_wire_port_is_a_clean_error() {
        let doc = GraphDocument {
            version: 1,
            name: None,
            description: None,
            preset_metadata: None,
            nodes: vec![
                NodeDocument {
                    id: 0,
                    type_id: SOURCE_TYPE_ID.to_string(),
                    handle: None,
                    params: BTreeMap::new(),
                    exposed_params: Default::default(),
                    editor_pos: None,
                    wgsl_source: None,
                    output_formats: BTreeMap::new(),
                },
                NodeDocument {
                    id: 1,
                    type_id: FINAL_OUTPUT_TYPE_ID.to_string(),
                    handle: None,
                    params: BTreeMap::new(),
                    exposed_params: Default::default(),
                    editor_pos: None,
                    wgsl_source: None,
                    output_formats: BTreeMap::new(),
                },
            ],
            wires: vec![WireDocument {
                from_node: 0,
                from_port: "nonexistent".to_string(),
                to_node: 1,
                to_port: "in".to_string(),
            }],
        };
        let err = expect_err(doc.into_graph(&registry()));
        assert!(matches!(err, LoadError::InvalidWire { .. }), "got {err:?}");
    }

    #[test]
    fn unknown_param_is_a_clean_error() {
        let mut params = BTreeMap::new();
        params.insert(
            "totally_made_up".to_string(),
            SerializedParamValue::Float { value: 0.5 },
        );
        let doc = GraphDocument {
            version: 1,
            name: None,
            description: None,
            preset_metadata: None,
            nodes: vec![NodeDocument {
                id: 0,
                type_id: primitives::THRESHOLD_TYPE_ID.to_string(),
                handle: None,
                params,
                exposed_params: Default::default(),
                editor_pos: None,
                wgsl_source: None,
                output_formats: BTreeMap::new(),
            }],
            wires: vec![],
        };
        let err = expect_err(doc.into_graph(&registry()));
        match err {
            LoadError::UnknownParam { param, .. } => assert_eq!(param, "totally_made_up"),
            other => panic!("expected UnknownParam, got {other:?}"),
        }
    }

    #[test]
    fn param_type_mismatch_is_a_clean_error() {
        let mut params = BTreeMap::new();
        // Threshold.level is a Float; we send an Enum.
        params.insert("level".to_string(), SerializedParamValue::Enum { value: 3 });
        let doc = GraphDocument {
            version: 1,
            name: None,
            description: None,
            preset_metadata: None,
            nodes: vec![NodeDocument {
                id: 0,
                type_id: primitives::THRESHOLD_TYPE_ID.to_string(),
                handle: None,
                params,
                exposed_params: Default::default(),
                editor_pos: None,
                wgsl_source: None,
                output_formats: BTreeMap::new(),
            }],
            wires: vec![],
        };
        let err = expect_err(doc.into_graph(&registry()));
        match err {
            LoadError::ParamTypeMismatch { expected, got, .. } => {
                assert_eq!(expected, "Float");
                assert_eq!(got, "Enum");
            }
            other => panic!("expected ParamTypeMismatch, got {other:?}"),
        }
    }

    #[test]
    fn future_version_is_rejected() {
        let doc = GraphDocument {
            version: EFFECT_GRAPH_VERSION_WITH_METADATA + 1,
            name: None,
            description: None,
            preset_metadata: None,
            nodes: vec![],
            wires: vec![],
        };
        let err = expect_err(doc.into_graph(&registry()));
        assert!(matches!(err, LoadError::UnsupportedVersion { .. }));
    }

    #[test]
    fn v1_document_is_accepted() {
        // V1 documents on disk (no presetMetadata) must keep loading
        // after the v2 schema bump. Every shipping bundled preset is
        // v2 post-§11, but user projects + test fixtures saved before
        // the migration must still round-trip.
        let doc = GraphDocument {
            version: 1,
            name: None,
            description: None,
            preset_metadata: None,
            nodes: vec![],
            wires: vec![],
        };
        let result = doc.into_graph(&registry());
        assert!(result.is_ok(), "v1 doc should load: {:?}", result.err());
    }

    #[test]
    fn v2_document_is_accepted() {
        let doc = GraphDocument {
            version: EFFECT_GRAPH_VERSION_WITH_METADATA,
            name: None,
            description: None,
            preset_metadata: None,
            nodes: vec![],
            wires: vec![],
        };
        let result = doc.into_graph(&registry());
        assert!(result.is_ok(), "v2 doc should load: {:?}", result.err());
    }

    #[test]
    fn five_node_bloom_shape_round_trips() {
        // The same topology as the integration test in
        // primitives/mod.rs: Source → MipChain → Blur → Blend.overlay
        // (with Source also fanning out to Blend.base), → FinalOutput.
        // Verifies fan-out + multi-input wires survive the round-trip.
        use primitives::{Blend, Threshold};
        let _ = (Blur::new(), MipChain::new(), Threshold::new(), Blend::new());

        let mut g = Graph::new();
        let src = g.add_node(Box::new(Source::new()));
        let mips = g.add_node(Box::new(MipChain::new()));
        let blur = g.add_node(Box::new(Blur::new()));
        let blend = g.add_node(Box::new(primitives::Blend::new()));
        let out = g.add_node(Box::new(FinalOutput::new()));

        g.connect((src, "out"), (mips, "source")).unwrap();
        g.connect((mips, "out"), (blur, "source")).unwrap();
        g.connect((src, "out"), (blend, "base")).unwrap();
        g.connect((blur, "out"), (blend, "overlay")).unwrap();
        g.connect((blend, "out"), (out, "in")).unwrap();

        let doc = GraphDocument::from_graph(&g);
        let json = serde_json::to_string(&doc).unwrap();
        let parsed: GraphDocument = serde_json::from_str(&json).unwrap();
        let g2 = parsed.into_graph(&registry()).unwrap();

        assert_eq!(g2.node_count(), 5);
        assert_eq!(g2.wires().len(), 5);
        validate(&g2).unwrap();
        let plan = compile(&g2).unwrap();
        assert_eq!(plan.steps().len(), 5);
    }

    #[test]
    fn name_and_description_round_trip() {
        let mut g = Graph::new();
        let _src = g.add_node(Box::new(Source::new()));
        let _out = g.add_node(Box::new(FinalOutput::new()));
        let doc = GraphDocument::from_graph(&g)
            .with_name("Bloom")
            .with_description("Bright-pass + multi-scale blur composite.");
        let json = serde_json::to_string(&doc).unwrap();
        let parsed: GraphDocument = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.name.as_deref(), Some("Bloom"));
        assert_eq!(
            parsed.description.as_deref(),
            Some("Bright-pass + multi-scale blur composite.")
        );
    }

    #[test]
    fn serialized_param_value_round_trips_every_variant() {
        let table = crate::node_graph::parameters::TableData::new(vec![
            vec![1.0, 2.0, 3.0],
            vec![4.0, 5.0, 6.0],
        ])
        .unwrap();
        let cases: Vec<ParamValue> = vec![
            ParamValue::Float(0.5),
            ParamValue::Float(7.0),
            ParamValue::Bool(true),
            ParamValue::Vec2([1.0, 2.0]),
            ParamValue::Vec3([1.0, 2.0, 3.0]),
            ParamValue::Vec4([1.0, 2.0, 3.0, 4.0]),
            ParamValue::Color([0.1, 0.2, 0.3, 1.0]),
            ParamValue::Enum(3),
            ParamValue::Table(std::sync::Arc::new(table)),
        ];
        for v in cases {
            let s: SerializedParamValue = v.clone().into();
            let json = serde_json::to_string(&s).unwrap();
            let back: SerializedParamValue = serde_json::from_str(&json).unwrap();
            let v2: ParamValue = back.into();
            assert_eq!(v, v2);
        }
    }
}
