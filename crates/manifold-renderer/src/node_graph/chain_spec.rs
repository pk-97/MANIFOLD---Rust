//! Chain-graph splicing primitives.
//!
//! After the §11 unified-registry migration, every shipping effect is
//! a JSON `EffectGraphDef` consumed via [`crate::node_graph::LoadedPresetView`].
//! The chain build loop calls [`splice_def_into_chain`] with each
//! active effect's canonical (or per-instance overridden) def to graft
//! its worker subgraph into the shared chain graph; [`is_skipped_for`]
//! decides whether to skip an effect entirely based on its declared
//! [`SkipMode`].
//!
//! This module holds the small set of types both paths share —
//! [`SpliceResult`], [`SkipMode`], and the splice/skip fns themselves.
//! The legacy `ChainSpec` inventory channel that previously lived here
//! is gone (block 8); the file name persists for the moment so the
//! re-export surface in `node_graph/mod.rs` can stay stable through
//! the rest of §11.

use std::borrow::Cow;

use ahash::AHashMap;
use manifold_core::EffectTypeId;
use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_core::effects::EffectInstance;

use crate::node_graph::boundary_nodes::{FINAL_OUTPUT_TYPE_ID, SOURCE_TYPE_ID};
use crate::node_graph::effect_node::NodeInstanceId;
use crate::node_graph::graph::Graph;
use crate::node_graph::persistence::{PrimitiveRegistry, SerializedParamValue};

/// Outcome of a single splice into the chain graph.
pub struct SpliceResult {
    /// `(node, port)` where the spliced effect's output lives. Port
    /// names come from primitive port declarations and are always
    /// `&'static str`.
    pub output: (NodeInstanceId, &'static str),

    /// Effect-local handle map. Names are scoped to the spliced
    /// effect; bindings + user-bindings look up nodes here, never on
    /// the chain graph globally.
    ///
    /// `Cow<'static, str>` so canonical splices can carry compile-time
    /// literals as `Cow::Borrowed("mix")` while JSON-loaded defs use
    /// `Cow::Owned` for names that came off disk.
    pub handles: Vec<(Cow<'static, str>, NodeInstanceId)>,
}

/// When the chain should drop an effect entirely (no workers added,
/// no cost). Previous output flows directly to the next effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipMode {
    /// Effect always contributes its workers.
    Never,
    /// Skip when the param identified by `param_id` is ≤ 0.
    OnZero { param_id: &'static str },
}

/// Standalone skip check used by the JSON-loaded preset path
/// (`LoadedPresetView`). Lookup goes through
/// `effect_definition_registry::param_id_to_index` which is
/// dual-source aware — works for both inventory-submitted
/// `EffectMetadata` and JSON-loaded `PresetMetadata`.
pub fn is_skipped_for(skip: SkipMode, type_id: &EffectTypeId, fx: &EffectInstance) -> bool {
    match skip {
        SkipMode::Never => false,
        SkipMode::OnZero { param_id } => {
            let Some(idx) =
                manifold_core::effect_definition_registry::param_id_to_index(type_id, param_id)
            else {
                return false;
            };
            fx.param_values
                .get(idx)
                .map(|s| s.value <= 0.0)
                .unwrap_or(false)
        }
    }
}

/// Splice an [`EffectGraphDef`] into the chain graph. Used by both the
/// canonical path (with `view.canonical_def` from
/// [`crate::node_graph::LoadedPresetView`]) and the per-card override
/// path (with `EffectInstance.graph`). Returns the output endpoint +
/// effect-local handle map.
///
/// The def's `Source` boundary disappears — every wire fanning out
/// from it is re-anchored to `source` (the chain's previous endpoint).
/// The def's `FinalOutput` boundary also disappears — the wire feeding
/// into it identifies the def's output endpoint, which becomes the
/// chain's next source.
///
/// Per-node params encoded in the def (the user's slider edits) are
/// applied via `graph.set_param` before returning. Effect-local
/// handles (named nodes in the def) flow into [`SpliceResult::handles`]
/// so routings + user-bindings resolve uniformly.
///
/// Returns `None` on malformed input (no Source / no FinalOutput /
/// unknown type id / orphan wire).
pub fn splice_def_into_chain(
    graph: &mut Graph,
    source: (NodeInstanceId, &'static str),
    def: &EffectGraphDef,
    registry: &PrimitiveRegistry,
) -> Option<SpliceResult> {
    // First pass: identify the def's Source and FinalOutput ids so we
    // know which wires to re-anchor / treat as the output.
    let mut def_source_id: Option<u32> = None;
    let mut def_final_id: Option<u32> = None;
    for n in &def.nodes {
        if n.type_id == SOURCE_TYPE_ID {
            def_source_id = Some(n.id);
        } else if n.type_id == FINAL_OUTPUT_TYPE_ID {
            def_final_id = Some(n.id);
        }
    }
    let def_source_id = def_source_id?;
    let def_final_id = def_final_id?;

    // Second pass: instantiate every non-boundary node. Track
    // (def_id → chain_node_id) so wires can be translated.
    let mut def_to_chain: AHashMap<u32, NodeInstanceId> = AHashMap::default();
    let mut handles: Vec<(Cow<'static, str>, NodeInstanceId)> = Vec::new();
    for n in &def.nodes {
        if n.id == def_source_id || n.id == def_final_id {
            continue;
        }
        let node = registry.construct(&n.type_id)?;
        let chain_id = graph.add_node(node);
        def_to_chain.insert(n.id, chain_id);
        if let Some(handle_name) = n.handle.as_deref() {
            handles.push((Cow::Owned(handle_name.to_owned()), chain_id));
        }
    }

    // Apply per-node params from the def.
    for n in &def.nodes {
        if let Some(&chain_id) = def_to_chain.get(&n.id) {
            for (param_name, value) in &n.params {
                let pv = match value {
                    SerializedParamValue::Float { value } => {
                        Some(crate::node_graph::ParamValue::Float(*value))
                    }
                    SerializedParamValue::Int { value } => {
                        // Back-compat: old saves wrote `Int` for whole-number
                        // params. In-memory storage is `Float` only — coerce.
                        Some(crate::node_graph::ParamValue::Float(*value as f32))
                    }
                    SerializedParamValue::Bool { value } => {
                        Some(crate::node_graph::ParamValue::Bool(*value))
                    }
                    SerializedParamValue::Enum { value } => {
                        Some(crate::node_graph::ParamValue::Enum(*value))
                    }
                    // Vec2/Vec3/Vec4/Color are not yet plumbed through
                    // the runtime `ParamValue` enum. Skip for now —
                    // the primitive keeps its declared default.
                    SerializedParamValue::Vec2 { .. }
                    | SerializedParamValue::Vec3 { .. }
                    | SerializedParamValue::Vec4 { .. }
                    | SerializedParamValue::Color { .. } => None,
                };
                if let Some(pv) = pv
                    && let Some(static_name) = resolve_param_name(graph, chain_id, param_name)
                {
                    let _ = graph.set_param(chain_id, static_name, pv);
                }
            }
        }
    }

    // Third pass: translate wires. Port names need to be resolved into
    // the primitive's declared `&'static str` references — those are
    // what `graph.connect` accepts, and looking them up on the just-
    // instantiated nodes is cleaner than leaking heap strings.
    let mut output_endpoint: Option<(NodeInstanceId, &'static str)> = None;
    for w in &def.wires {
        if w.from_node == def_source_id {
            let to_chain = *def_to_chain.get(&w.to_node)?;
            let to_port = resolve_input_port(graph, to_chain, &w.to_port)?;
            graph.connect(source, (to_chain, to_port)).ok()?;
            continue;
        }
        if w.to_node == def_final_id {
            let from_chain = *def_to_chain.get(&w.from_node)?;
            let from_port = resolve_output_port(graph, from_chain, &w.from_port)?;
            output_endpoint = Some((from_chain, from_port));
            continue;
        }
        let from_chain = *def_to_chain.get(&w.from_node)?;
        let to_chain = *def_to_chain.get(&w.to_node)?;
        let from_port = resolve_output_port(graph, from_chain, &w.from_port)?;
        let to_port = resolve_input_port(graph, to_chain, &w.to_port)?;
        graph.connect((from_chain, from_port), (to_chain, to_port)).ok()?;
    }

    Some(SpliceResult {
        output: output_endpoint?,
        handles,
    })
}

fn resolve_param_name(graph: &Graph, node: NodeInstanceId, name: &str) -> Option<&'static str> {
    graph
        .get_node(node)?
        .node
        .parameters()
        .iter()
        .map(|p| p.name)
        .find(|n| *n == name)
}

fn resolve_input_port(graph: &Graph, node: NodeInstanceId, name: &str) -> Option<&'static str> {
    graph
        .get_node(node)?
        .node
        .inputs()
        .iter()
        .map(|p| p.name)
        .find(|n| *n == name)
}

fn resolve_output_port(graph: &Graph, node: NodeInstanceId, name: &str) -> Option<&'static str> {
    graph
        .get_node(node)?
        .node
        .outputs()
        .iter()
        .map(|p| p.name)
        .find(|n| *n == name)
}
