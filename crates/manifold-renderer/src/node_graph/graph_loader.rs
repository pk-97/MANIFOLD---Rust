//! Shared graph-build pipeline used by every JSON-to-runtime path.
//!
//! Two callers consume this module today:
//!
//! - **Generator path** ([`crate::node_graph::persistence::EffectGraphDefExt::into_graph`])
//!   instantiates a standalone preset into a fresh [`Graph`]. Every node in
//!   the def, including the `system.generator_input` and `system.final_output`
//!   boundaries, becomes a regular graph node.
//! - **Effect splice path** ([`crate::node_graph::chain_spec::splice_def_into_chain`])
//!   grafts an effect preset's worker subgraph into an existing chain
//!   [`Graph`]. The def's `system.source` boundary disappears (its fan-out
//!   re-anchors to the chain's previous endpoint), and `system.final_output`
//!   disappears (the wire feeding it identifies the spliced subgraph's
//!   output endpoint).
//!
//! Both paths share every per-node feature: WGSL source install, per-param
//! type-checked overrides, per-output format overrides, per-output canvas-
//! scale overrides, exposed-param seeding. The same single function applies
//! the same set of features so neither side can silently lack one — this
//! module's existence is the structural fix for the drift bug class that
//! produced the May 2026 Blob Track HUD outage (commits 3500e7a7, a69a71bf,
//! and the audit follow-up).

use std::borrow::Cow;

use ahash::AHashMap;

use manifold_core::effect_graph_def::{EFFECT_GRAPH_VERSION_WITH_METADATA, EffectGraphDef};

use crate::node_graph::boundary_nodes::{
    FINAL_OUTPUT_TYPE_ID, GENERATOR_INPUT_TYPE_ID, SOURCE_TYPE_ID,
};
use crate::node_graph::effect_node::NodeInstanceId;
use crate::node_graph::graph::Graph;
use crate::node_graph::parameters::{ParamType, ParamValue};
use crate::node_graph::persistence::{PrimitiveRegistry, format_from_str};

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// How handle names declared in the def map onto the live [`Graph`].
#[derive(Debug, Clone, Copy)]
pub enum HandleScope {
    /// Register every handle on the graph itself via `add_node_named`.
    /// Used by the generator path — one preset per graph, so handle
    /// names cannot collide.
    Global,
    /// Return handles in [`NodeInstantiation::effect_local_handles`] and
    /// do not register them on the graph. Used by the effect splice
    /// path — multiple presets share one chain graph and may declare
    /// colliding handle names ("mix", "feedback").
    PerSplice,
}

/// What to do with the def's boundary nodes during instantiation.
#[derive(Debug, Clone, Copy)]
pub enum BoundaryHandling {
    /// Instantiate every boundary node (`system.source`,
    /// `system.generator_input`, `system.final_output`) as a regular
    /// graph node. Wire translation is straight `id_map` remapping.
    /// Used by the generator path.
    Standalone,
    /// Fold `system.source` and `system.final_output` away. Wires fanning
    /// out from `system.source` re-anchor to `source_endpoint`; the wire
    /// feeding `system.final_output` identifies the spliced subgraph's
    /// output endpoint, returned in
    /// [`NodeInstantiation::output_endpoint`]. `system.generator_input`,
    /// if present, is instantiated (effect per-frame scalar boundary).
    Splice {
        source_endpoint: (NodeInstanceId, &'static str),
    },
}

/// Errors produced by [`instantiate_def`]. Both callers convert these
/// into their own error surfaces — [`crate::node_graph::LoadError`] on
/// the generator path, `Option::None` + structured log on the splice
/// path. Every variant carries enough context (`node_id`, `type_id`,
/// optional `handle`) for a future editor surface to highlight the
/// affected node.
#[derive(Debug, Clone, PartialEq)]
pub enum GraphBuildError {
    UnsupportedVersion {
        found: u32,
        max: u32,
    },
    DuplicateNodeId(u32),
    UnknownTypeId {
        node_id: u32,
        type_id: String,
    },
    UnknownNodeRef {
        wire_index: usize,
        node_id: u32,
        side: WireSide,
    },
    UnknownParam {
        node_id: u32,
        type_id: String,
        param: String,
    },
    ParamTypeMismatch {
        node_id: u32,
        type_id: String,
        param: String,
        expected: &'static str,
        got: &'static str,
    },
    InvalidWire {
        wire_index: usize,
        reason: String,
    },
    UnknownOutputFormat {
        node_id: u32,
        type_id: String,
        port: String,
        format: String,
    },
    OutputFormatNotSupported {
        node_id: u32,
        type_id: String,
        port: String,
        format: String,
    },
    MissingBoundarySource,
    MissingBoundaryFinalOutput,
}

/// Which side of a wire failed to resolve.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WireSide {
    From,
    To,
}

/// What [`instantiate_def`] produced. Every field is populated regardless
/// of which boundary mode was requested; fields that don't apply to the
/// requested mode are `None` / empty.
#[derive(Debug)]
pub struct NodeInstantiation {
    /// Doc-id → runtime-id remap. Exposed so callers can perform further
    /// wire surgery (today only the editor's snapshot path uses this; the
    /// chain build does not).
    pub id_map: AHashMap<u32, NodeInstanceId>,

    /// Effect-local handle map. Populated when `handle_scope = PerSplice`.
    /// Empty when `handle_scope = Global` — handles are on the graph
    /// itself in that case.
    pub effect_local_handles: Vec<(Cow<'static, str>, NodeInstanceId)>,

    /// Output endpoint of the spliced subgraph. `Some` for
    /// `BoundaryHandling::Splice`, `None` for `Standalone`.
    pub output_endpoint: Option<(NodeInstanceId, &'static str)>,

    /// The instantiated `system.generator_input` node id, if the def
    /// contained one. Both boundary modes return this when present.
    /// Generator path: `JsonGraphGenerator::set_frame_context` writes to
    /// this node. Effect path (planned phase 2): chain runner pushes
    /// per-frame scalars to this node so effects can react to project
    /// BPM / beat / aspect alongside their texture input.
    pub generator_input_id: Option<NodeInstanceId>,

    /// The instantiated `system.final_output` node id, present only for
    /// `BoundaryHandling::Standalone`. The host pre-binds the target
    /// texture to its `in` resource.
    pub final_output_id: Option<NodeInstanceId>,
}

// ---------------------------------------------------------------------------
// The shared per-node + per-wire pipeline
// ---------------------------------------------------------------------------

/// Instantiate every (non-folded) node from `def` into `graph`, apply
/// every per-node JSON feature, then translate wires according to
/// `boundary`.
///
/// Per-node features applied in order, before the node is moved into
/// the graph:
///
/// 1. **`wgsl_source`** — installed on the boxed node pre-`add_node` so
///    dynamic-shape primitives (`node.wgsl_compute`) reparse their port
///    list before parameter validation reads it.
/// 2. **`params`** — type-checked against the node's declared
///    [`ParamType`] list. Mismatches emit
///    [`GraphBuildError::ParamTypeMismatch`].
/// 3. **`output_formats`** — applied via [`Graph::set_output_format`]
///    with a post-set audit: primitives whose shader hard-codes its
///    output format silently no-op `set_output_format`, so writing
///    `outputFormats` against them silently dropped the override before
///    this audit existed. Now a no-op write is
///    [`GraphBuildError::OutputFormatNotSupported`].
/// 4. **`output_canvas_scales`** — applied via
///    [`Graph::set_output_canvas_scale`]. No audit (no shipping primitive
///    accepts canvas-scale yet besides `node.wgsl_compute`, which honours
///    every write).
/// 5. **Handle registration** — `add_node_named` for
///    [`HandleScope::Global`], owned `Cow::Owned` for
///    [`HandleScope::PerSplice`].
///
/// Wire translation then runs according to `boundary`:
///
/// - **`Standalone`** — every wire's `(from_node, to_node)` pair gets
///   remapped via `id_map`. Boundary nodes are regular graph nodes.
/// - **`Splice { source_endpoint }`** — wires from the def's
///   `system.source` re-anchor to `source_endpoint`; the wire feeding
///   the def's `system.final_output` identifies the splice's output
///   endpoint and is not connected. All other wires remap normally.
///
/// Returns the [`NodeInstantiation`] on success. On any error the
/// graph's state is the union of every successful step before the
/// failure — both callers handle this by either propagating
/// (generator, where the whole load aborts) or falling back to a
/// canonical def (splice, where the orphaned partial graph is the
/// price of "try divergent, then canonical").
pub fn instantiate_def(
    graph: &mut Graph,
    def: &EffectGraphDef,
    registry: &PrimitiveRegistry,
    handle_scope: HandleScope,
    boundary: BoundaryHandling,
) -> Result<NodeInstantiation, GraphBuildError> {
    if def.version > EFFECT_GRAPH_VERSION_WITH_METADATA {
        return Err(GraphBuildError::UnsupportedVersion {
            found: def.version,
            max: EFFECT_GRAPH_VERSION_WITH_METADATA,
        });
    }

    // For Splice, identify the def's Source and FinalOutput up front so
    // we know which nodes to skip during instantiation and which wires
    // to fold during translation.
    let (def_source_id, def_final_id) = match boundary {
        BoundaryHandling::Standalone => (None, None),
        BoundaryHandling::Splice { .. } => {
            let mut src: Option<u32> = None;
            let mut fin: Option<u32> = None;
            for n in &def.nodes {
                if n.type_id == SOURCE_TYPE_ID {
                    src = Some(n.id);
                } else if n.type_id == FINAL_OUTPUT_TYPE_ID {
                    fin = Some(n.id);
                }
            }
            (
                Some(src.ok_or(GraphBuildError::MissingBoundarySource)?),
                Some(fin.ok_or(GraphBuildError::MissingBoundaryFinalOutput)?),
            )
        }
    };

    let mut id_map: AHashMap<u32, NodeInstanceId> = AHashMap::default();
    let mut effect_local_handles: Vec<(Cow<'static, str>, NodeInstanceId)> = Vec::new();
    let mut generator_input_id: Option<NodeInstanceId> = None;
    let mut final_output_id: Option<NodeInstanceId> = None;

    // ── Per-node instantiation pass ──
    for node_doc in &def.nodes {
        // Splice folds these two boundary nodes — don't instantiate.
        if Some(node_doc.id) == def_source_id || Some(node_doc.id) == def_final_id {
            continue;
        }

        if id_map.contains_key(&node_doc.id) {
            return Err(GraphBuildError::DuplicateNodeId(node_doc.id));
        }

        let mut boxed = registry
            .construct(&node_doc.type_id)
            .ok_or_else(|| GraphBuildError::UnknownTypeId {
                node_id: node_doc.id,
                type_id: node_doc.type_id.clone(),
            })?;

        // (1) WGSL source — install on the box BEFORE `add_node` so the
        // node's reparse runs while we still own it. Static-shape
        // primitives' `set_wgsl_source` is a no-op, so this is free for
        // the common case.
        if let Some(source) = node_doc.wgsl_source.as_deref() {
            boxed.set_wgsl_source(source);
        }

        // Snapshot the declared param surface BEFORE moving `boxed` into
        // the graph — we need this for type-checked param overrides
        // below, plus for the exposed-params validation pass.
        let param_defs: Vec<(&'static str, ParamType)> = boxed
            .parameters()
            .iter()
            .map(|p| (p.name, p.ty))
            .collect();

        let runtime_id = match handle_scope {
            HandleScope::Global => {
                if let Some(handle) = node_doc.handle.as_deref() {
                    // `add_node_named` requires `&'static str`. We leak
                    // the handle string — bounded leak (one per inner
                    // node per preset load, ~30 per preset), amortized
                    // over the process lifetime. Same pattern persistence
                    // used pre-unification.
                    let static_handle: &'static str =
                        Box::leak(handle.to_string().into_boxed_str());
                    graph.add_node_named(static_handle, boxed)
                } else {
                    graph.add_node(boxed)
                }
            }
            HandleScope::PerSplice => graph.add_node(boxed),
        };
        id_map.insert(node_doc.id, runtime_id);

        // PerSplice: record the handle in the effect-local map. Owned
        // Cow because the handle string comes off disk and we don't
        // want to leak per-chain-build.
        if let HandleScope::PerSplice = handle_scope
            && let Some(handle_name) = node_doc.handle.as_deref()
        {
            effect_local_handles.push((Cow::Owned(handle_name.to_owned()), runtime_id));
        }

        // (2) Param overrides — type-checked.
        for (key, value) in &node_doc.params {
            let Some(&(name_static, expected_ty)) =
                param_defs.iter().find(|(n, _)| *n == key.as_str())
            else {
                return Err(GraphBuildError::UnknownParam {
                    node_id: node_doc.id,
                    type_id: node_doc.type_id.clone(),
                    param: key.clone(),
                });
            };
            let pv: ParamValue = value.clone().into();
            if !param_value_matches_type(&pv, expected_ty) {
                return Err(GraphBuildError::ParamTypeMismatch {
                    node_id: node_doc.id,
                    type_id: node_doc.type_id.clone(),
                    param: key.clone(),
                    expected: param_type_name(expected_ty),
                    got: param_type_label(&pv),
                });
            }
            // set_param can only fail with NodeNotFound (just added) or
            // ParamNotFound (just validated). Both impossible here.
            graph
                .set_param(runtime_id, name_static, pv)
                .expect("validated above");
        }

        // (3) Exposed params — Global scope only. Splice path effects
        // expose via `EffectInstance.user_param_bindings` at a different
        // layer.
        if let HandleScope::Global = handle_scope {
            for exposed_name in &node_doc.exposed_params {
                if let Some(&(name_static, _)) =
                    param_defs.iter().find(|(n, _)| *n == exposed_name.as_str())
                {
                    graph
                        .set_param_exposed(runtime_id, name_static, true)
                        .expect("just added");
                }
            }
        }

        // (4) Output format overrides + audit.
        for (port_name, fmt_str) in &node_doc.output_formats {
            let Some(fmt) = format_from_str(fmt_str) else {
                return Err(GraphBuildError::UnknownOutputFormat {
                    node_id: node_doc.id,
                    type_id: node_doc.type_id.clone(),
                    port: port_name.clone(),
                    format: fmt_str.clone(),
                });
            };
            graph
                .set_output_format(runtime_id, port_name, fmt)
                .expect("just added");
            // Audit: a primitive whose shader hardcodes its output format
            // has a no-op `set_output_format`. Writing `outputFormats`
            // against it silently dropped before this check existed;
            // catch it loudly at load time.
            let inst = graph.get_node(runtime_id).expect("just added");
            if inst.node.output_format(port_name) != Some(fmt) {
                return Err(GraphBuildError::OutputFormatNotSupported {
                    node_id: node_doc.id,
                    type_id: node_doc.type_id.clone(),
                    port: port_name.clone(),
                    format: fmt_str.clone(),
                });
            }
        }

        // (5) Output canvas-scale overrides. Honoured today only by
        // `node.wgsl_compute`; every other primitive has a no-op default.
        for (port_name, scale) in &node_doc.output_canvas_scales {
            let &[num, denom] = scale;
            graph
                .set_output_canvas_scale(runtime_id, port_name, (num, denom))
                .expect("just added");
        }

        // (6) Stash boundary node ids on the way through so the caller
        // can find them without a second scan.
        if node_doc.type_id == GENERATOR_INPUT_TYPE_ID {
            generator_input_id = Some(runtime_id);
        }
        if node_doc.type_id == FINAL_OUTPUT_TYPE_ID {
            // Only reachable on Standalone — Splice folded this above.
            final_output_id = Some(runtime_id);
        }
    }

    // ── Wire translation pass ──
    let mut output_endpoint: Option<(NodeInstanceId, &'static str)> = None;
    for (wire_index, w) in def.wires.iter().enumerate() {
        match boundary {
            BoundaryHandling::Standalone => {
                let from_chain = *id_map
                    .get(&w.from_node)
                    .ok_or(GraphBuildError::UnknownNodeRef {
                        wire_index,
                        node_id: w.from_node,
                        side: WireSide::From,
                    })?;
                let to_chain =
                    *id_map.get(&w.to_node).ok_or(GraphBuildError::UnknownNodeRef {
                        wire_index,
                        node_id: w.to_node,
                        side: WireSide::To,
                    })?;
                let from_port = resolve_output_port(graph, from_chain, &w.from_port).ok_or_else(
                    || GraphBuildError::InvalidWire {
                        wire_index,
                        reason: format!(
                            "from node {} has no output port '{}'",
                            w.from_node, w.from_port
                        ),
                    },
                )?;
                let to_port = resolve_input_port(graph, to_chain, &w.to_port).ok_or_else(|| {
                    GraphBuildError::InvalidWire {
                        wire_index,
                        reason: format!(
                            "to node {} has no input port '{}'",
                            w.to_node, w.to_port
                        ),
                    }
                })?;
                graph
                    .connect((from_chain, from_port), (to_chain, to_port))
                    .map_err(|e| GraphBuildError::InvalidWire {
                        wire_index,
                        reason: format!("{e:?}"),
                    })?;
            }
            BoundaryHandling::Splice { source_endpoint } => {
                // Source-fanout: re-anchor.
                if Some(w.from_node) == def_source_id {
                    let to_chain = *id_map.get(&w.to_node).ok_or(
                        GraphBuildError::UnknownNodeRef {
                            wire_index,
                            node_id: w.to_node,
                            side: WireSide::To,
                        },
                    )?;
                    let to_port = resolve_input_port(graph, to_chain, &w.to_port).ok_or_else(
                        || GraphBuildError::InvalidWire {
                            wire_index,
                            reason: format!(
                                "to node {} has no input port '{}'",
                                w.to_node, w.to_port
                            ),
                        },
                    )?;
                    graph
                        .connect(source_endpoint, (to_chain, to_port))
                        .map_err(|e| GraphBuildError::InvalidWire {
                            wire_index,
                            reason: format!("{e:?}"),
                        })?;
                    continue;
                }
                // FinalOutput-feed: identify output endpoint, do not connect.
                if Some(w.to_node) == def_final_id {
                    let from_chain = *id_map.get(&w.from_node).ok_or(
                        GraphBuildError::UnknownNodeRef {
                            wire_index,
                            node_id: w.from_node,
                            side: WireSide::From,
                        },
                    )?;
                    let from_port =
                        resolve_output_port(graph, from_chain, &w.from_port).ok_or_else(|| {
                            GraphBuildError::InvalidWire {
                                wire_index,
                                reason: format!(
                                    "from node {} has no output port '{}'",
                                    w.from_node, w.from_port
                                ),
                            }
                        })?;
                    output_endpoint = Some((from_chain, from_port));
                    continue;
                }
                // Normal wire.
                let from_chain = *id_map.get(&w.from_node).ok_or(
                    GraphBuildError::UnknownNodeRef {
                        wire_index,
                        node_id: w.from_node,
                        side: WireSide::From,
                    },
                )?;
                let to_chain =
                    *id_map.get(&w.to_node).ok_or(GraphBuildError::UnknownNodeRef {
                        wire_index,
                        node_id: w.to_node,
                        side: WireSide::To,
                    })?;
                let from_port = resolve_output_port(graph, from_chain, &w.from_port).ok_or_else(
                    || GraphBuildError::InvalidWire {
                        wire_index,
                        reason: format!(
                            "from node {} has no output port '{}'",
                            w.from_node, w.from_port
                        ),
                    },
                )?;
                let to_port = resolve_input_port(graph, to_chain, &w.to_port).ok_or_else(|| {
                    GraphBuildError::InvalidWire {
                        wire_index,
                        reason: format!(
                            "to node {} has no input port '{}'",
                            w.to_node, w.to_port
                        ),
                    }
                })?;
                graph
                    .connect((from_chain, from_port), (to_chain, to_port))
                    .map_err(|e| GraphBuildError::InvalidWire {
                        wire_index,
                        reason: format!("{e:?}"),
                    })?;
            }
        }
    }

    Ok(NodeInstantiation {
        id_map,
        effect_local_handles,
        output_endpoint,
        generator_input_id,
        final_output_id,
    })
}

// ---------------------------------------------------------------------------
// Param-type helpers
// ---------------------------------------------------------------------------

/// Whether a [`ParamValue`] satisfies a declared [`ParamType`]. The Int
/// collapse means `ParamType::Int` accepts `ParamValue::Float` (storage
/// is `Float` only since the legacy Int variant was removed).
pub(crate) fn param_value_matches_type(v: &ParamValue, ty: ParamType) -> bool {
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

/// Tag for the declared `ParamType` side of a mismatch error.
pub(crate) fn param_type_name(ty: ParamType) -> &'static str {
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

/// Tag for the `ParamValue` side of a mismatch error.
pub(crate) fn param_type_label(v: &ParamValue) -> &'static str {
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

/// Single-line structured log helper. The terminal-readable shape callers
/// agree on so logs grep cleanly and the future editor surface can
/// attach errors to the right node.
pub fn log_build_error(context: &str, err: &GraphBuildError) {
    use std::fmt::Write;

    let mut buf = String::with_capacity(120);
    let _ = write!(buf, "[graph-build] {context}: ");
    match err {
        GraphBuildError::UnsupportedVersion { found, max } => {
            let _ = write!(buf, "unsupported version {found} (max {max})");
        }
        GraphBuildError::DuplicateNodeId(id) => {
            let _ = write!(buf, "duplicate node id {id}");
        }
        GraphBuildError::UnknownTypeId { node_id, type_id } => {
            let _ = write!(buf, "node {node_id}: unknown type id '{type_id}'");
        }
        GraphBuildError::UnknownNodeRef {
            wire_index,
            node_id,
            side,
        } => {
            let _ = write!(
                buf,
                "wire #{wire_index}: {side:?} references unknown node id {node_id}"
            );
        }
        GraphBuildError::UnknownParam {
            node_id,
            type_id,
            param,
        } => {
            let _ = write!(buf, "node {node_id} ({type_id}): unknown param '{param}'");
        }
        GraphBuildError::ParamTypeMismatch {
            node_id,
            type_id,
            param,
            expected,
            got,
        } => {
            let _ = write!(
                buf,
                "node {node_id} ({type_id}): param '{param}' expected {expected}, got {got}"
            );
        }
        GraphBuildError::InvalidWire { wire_index, reason } => {
            let _ = write!(buf, "wire #{wire_index}: {reason}");
        }
        GraphBuildError::UnknownOutputFormat {
            node_id,
            type_id,
            port,
            format,
        } => {
            let _ = write!(
                buf,
                "node {node_id} ({type_id}): output '{port}' unknown format '{format}'"
            );
        }
        GraphBuildError::OutputFormatNotSupported {
            node_id,
            type_id,
            port,
            format,
        } => {
            let _ = write!(
                buf,
                "node {node_id} ({type_id}): outputFormats.{port}='{format}' silently \
                 ignored (primitive's shader hardcodes its format)"
            );
        }
        GraphBuildError::MissingBoundarySource => {
            let _ = write!(buf, "splice def has no system.source boundary");
        }
        GraphBuildError::MissingBoundaryFinalOutput => {
            let _ = write!(buf, "splice def has no system.final_output boundary");
        }
    }
    eprintln!("{buf}");
}

// ---------------------------------------------------------------------------
// Port-name resolution helpers
// ---------------------------------------------------------------------------

fn resolve_input_port(graph: &Graph, node: NodeInstanceId, name: &str) -> Option<&'static str> {
    graph
        .get_node(node)?
        .node
        .inputs()
        .iter()
        .find(|p| p.name == name)
        .map(|p| p.name)
}

fn resolve_output_port(graph: &Graph, node: NodeInstanceId, name: &str) -> Option<&'static str> {
    graph
        .get_node(node)?
        .node
        .outputs()
        .iter()
        .find(|p| p.name == name)
        .map(|p| p.name)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::boundary_nodes::{FinalOutput, Source};

    fn registry() -> PrimitiveRegistry {
        PrimitiveRegistry::with_builtin()
    }

    /// Standalone instantiation: every boundary node lives in the graph;
    /// `final_output_id` is populated and `output_endpoint` is None.
    #[test]
    fn standalone_instantiates_every_boundary() {
        let json = r#"{
            "version": 1,
            "name": "test",
            "nodes": [
                { "id": 0, "typeId": "system.generator_input", "handle": "input" },
                { "id": 1, "typeId": "node.uv_field", "handle": "uv" },
                { "id": 2, "typeId": "system.final_output", "handle": "final_output" }
            ],
            "wires": [
                { "fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "in" }
            ]
        }"#;
        let def: EffectGraphDef = serde_json::from_str(json).expect("parse");
        let mut graph = Graph::new();
        let inst = instantiate_def(
            &mut graph,
            &def,
            &registry(),
            HandleScope::Global,
            BoundaryHandling::Standalone,
        )
        .expect("standalone instantiates cleanly");
        assert!(inst.generator_input_id.is_some());
        assert!(inst.final_output_id.is_some());
        assert!(inst.output_endpoint.is_none());
        assert!(inst.effect_local_handles.is_empty());
        assert_eq!(graph.node_count(), 3);
        assert_eq!(graph.wires().len(), 1);
        assert!(graph.node_id_by_handle("uv").is_some());
    }

    /// Splice instantiation: system.source + system.final_output are
    /// folded, output_endpoint captures the wire that fed final_output,
    /// handles return in effect_local_handles (NOT on graph.handles()).
    #[test]
    fn splice_folds_boundaries_and_returns_output_endpoint() {
        // A trivial 1-effect splice: source → threshold → final_output.
        // The chain graph's prev_node is the host source we connect to.
        let mut graph = Graph::new();
        let host_source = graph.add_node(Box::new(Source::new()));
        let host_final = graph.add_node(Box::new(FinalOutput::new()));

        let json = r#"{
            "version": 1,
            "name": "test",
            "nodes": [
                { "id": 0, "typeId": "system.source" },
                { "id": 1, "typeId": "node.threshold", "handle": "thresh" },
                { "id": 2, "typeId": "system.final_output" }
            ],
            "wires": [
                { "fromNode": 0, "fromPort": "out", "toNode": 1, "toPort": "source" },
                { "fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "in" }
            ]
        }"#;
        let def: EffectGraphDef = serde_json::from_str(json).expect("parse");

        let inst = instantiate_def(
            &mut graph,
            &def,
            &registry(),
            HandleScope::PerSplice,
            BoundaryHandling::Splice {
                source_endpoint: (host_source, "out"),
            },
        )
        .expect("splice instantiates cleanly");

        // The two boundary nodes were folded.
        assert!(inst.final_output_id.is_none());
        assert!(inst.generator_input_id.is_none());

        // The threshold is the output endpoint.
        let (endpoint_node, endpoint_port) = inst.output_endpoint.expect("splice has endpoint");
        assert_eq!(endpoint_port, "out");
        let thresh_id = inst
            .id_map
            .get(&1)
            .copied()
            .expect("threshold id mapped");
        assert_eq!(endpoint_node, thresh_id);

        // Handle was returned locally, NOT registered on the graph.
        assert_eq!(inst.effect_local_handles.len(), 1);
        assert_eq!(inst.effect_local_handles[0].0.as_ref(), "thresh");
        assert!(graph.node_id_by_handle("thresh").is_none());

        // Wire host_source.out → thresh.source connected.
        let wires = graph.wires();
        assert!(
            wires.iter().any(|w| w.from.0 == host_source && w.to.0 == thresh_id),
            "source re-anchor wire missing; got wires: {wires:?}"
        );

        // host_source, host_final, threshold = 3 nodes. The def's
        // Source/FinalOutput were folded, never instantiated.
        assert_eq!(graph.node_count(), 3);
        let _ = host_final;
    }

    /// Drift bug regression #1: output_formats audit now fires on the
    /// splice path. Pre-unification, splice silently dropped
    /// outputFormats overrides on primitives whose shader hardcoded the
    /// format. Now it errors.
    #[test]
    fn splice_audits_output_format_overrides() {
        let mut graph = Graph::new();
        let host_source = graph.add_node(Box::new(Source::new()));

        // Threshold's output format is hard-coded in its shader. An
        // outputFormats override against it must be rejected.
        let json = r#"{
            "version": 1,
            "name": "test",
            "nodes": [
                { "id": 0, "typeId": "system.source" },
                {
                    "id": 1,
                    "typeId": "node.threshold",
                    "outputFormats": { "out": "rgba32float" }
                },
                { "id": 2, "typeId": "system.final_output" }
            ],
            "wires": [
                { "fromNode": 0, "fromPort": "out", "toNode": 1, "toPort": "source" },
                { "fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "in" }
            ]
        }"#;
        let def: EffectGraphDef = serde_json::from_str(json).expect("parse");

        let result = instantiate_def(
            &mut graph,
            &def,
            &registry(),
            HandleScope::PerSplice,
            BoundaryHandling::Splice {
                source_endpoint: (host_source, "out"),
            },
        );
        assert!(
            matches!(
                result,
                Err(GraphBuildError::OutputFormatNotSupported { .. })
            ),
            "splice should reject outputFormats against a hardcoded-format primitive; got {result:?}",
        );
    }

    /// Drift bug regression #2: param type validation now runs on the
    /// splice path. Pre-unification, splice silently coerced or
    /// dropped values; now it errors loudly.
    #[test]
    fn splice_rejects_param_type_mismatch() {
        let mut graph = Graph::new();
        let host_source = graph.add_node(Box::new(Source::new()));

        // Threshold.level is Float; this writes Bool.
        let json = r#"{
            "version": 1,
            "name": "test",
            "nodes": [
                { "id": 0, "typeId": "system.source" },
                {
                    "id": 1,
                    "typeId": "node.threshold",
                    "params": { "level": { "type": "Bool", "value": true } }
                },
                { "id": 2, "typeId": "system.final_output" }
            ],
            "wires": [
                { "fromNode": 0, "fromPort": "out", "toNode": 1, "toPort": "source" },
                { "fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "in" }
            ]
        }"#;
        let def: EffectGraphDef = serde_json::from_str(json).expect("parse");

        let result = instantiate_def(
            &mut graph,
            &def,
            &registry(),
            HandleScope::PerSplice,
            BoundaryHandling::Splice {
                source_endpoint: (host_source, "out"),
            },
        );
        assert!(
            matches!(result, Err(GraphBuildError::ParamTypeMismatch { .. })),
            "splice should reject param type mismatch; got {result:?}",
        );
    }

    /// The unknown-type-id error names the offending type for the
    /// future editor surface.
    #[test]
    fn unknown_type_id_includes_context() {
        let mut graph = Graph::new();
        let json = r#"{
            "version": 1,
            "name": "test",
            "nodes": [
                { "id": 0, "typeId": "system.generator_input" },
                { "id": 1, "typeId": "node.nonexistent" },
                { "id": 2, "typeId": "system.final_output" }
            ],
            "wires": []
        }"#;
        let def: EffectGraphDef = serde_json::from_str(json).expect("parse");
        let err = instantiate_def(
            &mut graph,
            &def,
            &registry(),
            HandleScope::Global,
            BoundaryHandling::Standalone,
        )
        .unwrap_err();
        match err {
            GraphBuildError::UnknownTypeId { node_id, type_id } => {
                assert_eq!(node_id, 1);
                assert_eq!(type_id, "node.nonexistent");
            }
            other => panic!("expected UnknownTypeId; got {other:?}"),
        }
    }

    /// Sanity: a fixture without `system.source` is rejected at the
    /// splice boundary check, not later during wire translation.
    #[test]
    fn splice_rejects_missing_source_boundary() {
        let mut graph = Graph::new();
        let host_source = graph.add_node(Box::new(Source::new()));
        let json = r#"{
            "version": 1,
            "name": "test",
            "nodes": [
                { "id": 0, "typeId": "node.threshold" },
                { "id": 1, "typeId": "system.final_output" }
            ],
            "wires": []
        }"#;
        let def: EffectGraphDef = serde_json::from_str(json).expect("parse");
        let err = instantiate_def(
            &mut graph,
            &def,
            &registry(),
            HandleScope::PerSplice,
            BoundaryHandling::Splice {
                source_endpoint: (host_source, "out"),
            },
        )
        .unwrap_err();
        assert!(matches!(err, GraphBuildError::MissingBoundarySource));
    }
}
