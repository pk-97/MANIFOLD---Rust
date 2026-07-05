//! [`BoundGraph`] — the shared per-frame binding lifecycle for a graph that an
//! outer card drives.
//!
//! Effects and generators both run a node graph whose inner-node params are
//! driven by outer-card sliders through [`ResolvedBinding`]s. The *apply
//! primitives* ([`apply_bindings`], [`LastAppliedCache`], [`ResolvedBinding`])
//! have been shared for a while, but the **orchestration around them** — resolve,
//! cache, push def overrides, re-assert bindings — was written twice (the effect
//! chain in `effect_chain_graph.rs`, the generator in `json_graph_generator.rs`),
//! and the two copies drifted. That drift is a bug factory:
//!
//! - The generator's "push def inner-param overrides" path forgot to clear the
//!   binding cache afterward, so a bound slider (OilyFluid Speed) stuck at the
//!   baked def value whenever a node-position edit landed — the effect chain
//!   cleared it, the generator didn't (`903deaa8`).
//!
//! `BoundGraph` is the single home for that orchestration so it can't diverge
//! again. It owns the resolved binding list + the skip-on-unchanged cache, and
//! exposes the two operations both runtimes need every frame:
//!
//! - [`BoundGraph::apply`] — push the outer-card values through the bindings
//!   (skipping unchanged slots).
//! - [`BoundGraph::apply_inner_overrides`] — push a def's inner-node param values
//!   into the live graph **and clear the cache** so the live bindings re-assert
//!   over them on the next [`apply`](BoundGraph::apply). Pushing the def value
//!   into *every* inner node — including the ones a slider drives — is why the
//!   cache must be cleared: without it, the next `apply` sees the outer value
//!   unchanged and skips the write, leaving the bound inner param stuck at the
//!   def default. The clear lives *inside* this method, so neither runtime can
//!   forget it.
//!
//! Binding *construction* still differs per side (effects build a static prefix +
//! a user tail off `PresetInstance`; generators resolve everything from the def's
//! `preset_metadata.bindings`), and the note-reshape / user-tail rebuild paths
//! still live in each runtime for now — those fold in as the storage unification
//! (`docs/PRESET_UNIFICATION_PLAN.md` Phases 1b/4-struct) progresses. This is the
//! first shared seam: the per-frame apply + override-and-clear.

use manifold_core::NodeId;
use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_core::params::ParamManifest;

use crate::node_graph::effect_node::NodeInstanceId;
use crate::node_graph::graph::Graph;
use crate::node_graph::param_binding::{
    LastAppliedCache, ResolvedBinding, apply_binding_defaults, apply_bindings,
};

/// The resolved binding list for one card-driven graph plus its skip-on-unchanged
/// cache. Owned by an effect's `EffectSlot` and a `JsonGraphGenerator` alike.
pub struct BoundGraph {
    /// Outer-card → inner-node bindings, resolved against the live graph. For
    /// effects this is the static prefix (`bindings[..n_static]`) followed by the
    /// user tail; for generators it is the preset's full binding list. Either way
    /// [`apply_bindings`] walks it against `cache` in lockstep with the host's
    /// `param_values`.
    pub bindings: Vec<ResolvedBinding>,
    /// Per-binding skip-on-unchanged cache. Seeded with each binding's declared
    /// default at construction so a freshly-built graph only writes the slots that
    /// already diverge from their default.
    pub cache: LastAppliedCache,
}

impl BoundGraph {
    /// Build from an already-resolved binding list, seeding the cache and planting
    /// each binding's declared default into its inner-node target so the cache's
    /// `Applied(default)` claim is true on the first frame (see
    /// [`apply_binding_defaults`]).
    pub fn new(bindings: Vec<ResolvedBinding>, graph: &mut Graph) -> Self {
        let mut cache = LastAppliedCache::new();
        cache.seed_from_bindings(&bindings);
        apply_binding_defaults(&bindings, graph, None);
        Self { bindings, cache }
    }

    /// Push the host's outer-card values through the bindings, skipping slots whose
    /// outer value hasn't changed since last frame. The per-frame hot call.
    pub fn apply(&mut self, graph: &mut Graph, values: &ParamManifest) {
        apply_bindings(&self.bindings, graph, None, values, &mut self.cache);
    }

    /// Push `def`'s inner-node param values into the live `graph` for every node
    /// present in `node_map`, then clear the binding cache so the live bindings
    /// re-assert over what was just written on the next [`apply`](Self::apply).
    ///
    /// This is the single home for an editing-time graph-version edit (a value or
    /// position change that doesn't rebuild the graph). The cache clear is the
    /// load-bearing part: the override pushes the def value into *every* inner
    /// node, including ones a slider drives, so without re-asserting the bindings
    /// a bound param would stick at the def value (the OilyFluid Speed snap-back).
    /// Both runtimes route through here, so the clear can't be forgotten on one
    /// side and not the other.
    pub fn apply_inner_overrides(
        &mut self,
        graph: &mut Graph,
        node_map: &[(NodeId, NodeInstanceId)],
        def: Option<&EffectGraphDef>,
    ) {
        apply_inner_param_overrides(def, node_map, graph);
        self.cache.clear();
    }
}

/// Push every inner-node param value declared in `def` into the live `graph`,
/// resolving each def node to its runtime instance through `node_map`. Flattens
/// groups first so a group-interface override routes onto its concrete inner node
/// (group nodes carry no runtime step). Edit-time only — never per frame.
///
/// Standalone (not a `BoundGraph` method) because the effect chain builds the
/// `node_map` from its multi-effect splice while a generator builds one over its
/// whole graph; both feed the same routine. Callers that also drive bindings
/// should use [`BoundGraph::apply_inner_overrides`] so the cache clear comes for
/// free.
pub fn apply_inner_param_overrides(
    def: Option<&EffectGraphDef>,
    node_map: &[(NodeId, NodeInstanceId)],
    graph: &mut Graph,
) {
    let Some(def) = def else { return };
    let Ok(flat) = manifold_core::flatten::flatten_groups(def) else {
        return;
    };
    for node in &flat.nodes {
        let Some((_, inst)) = node_map.iter().find(|(nid, _)| *nid == node.node_id) else {
            continue;
        };
        for (name, value) in &node.params {
            // `set_param` (not unchecked) so a dynamic-port param re-runs the
            // node's `reconfigure` hook, matching a live slider write.
            let _ = graph.set_param(*inst, name, value.clone().into());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::borrow::Cow;
    use std::collections::BTreeMap;

    use manifold_core::NodeId;
    use manifold_core::effect_graph_def::{
        EFFECT_GRAPH_VERSION, EffectGraphDef, EffectGraphNode, ParamSpecDef, SerializedParamValue,
    };
    use manifold_core::params::{Param, ParamManifest};

    use crate::node_graph::param_binding::{BindingSource, ResolvedBinding, ResolvedTarget};
    use crate::node_graph::parameters::ParamValue;
    use crate::node_graph::primitives::AffineTransform;
    use crate::node_graph::{Graph, ParamConvert};

    fn slot(id: &str, value: f32, exposed: bool) -> Param {
        let mut p = Param::bundled(ParamSpecDef {
            id: id.into(),
            name: id.into(),
            min: 0.0,
            max: 1.0,
            default_value: value,
            whole_numbers: false,
            is_toggle: false,
            is_trigger: false,
            value_labels: vec![],
            format_string: None,
            osc_suffix: String::new(),
            curve: Default::default(),
            invert: false,
        });
        p.value = value;
        p.base = value;
        p.exposed = exposed;
        p
    }

    fn scale_of(graph: &Graph, inst: NodeInstanceId) -> ParamValue {
        graph
            .get_node(inst)
            .and_then(|n| n.params.get("scale").cloned())
            .expect("affine exposes `scale`")
    }

    /// A def carrying a single node `"feedback"` whose `scale` param is set to
    /// `value` — the "baked def value" an editor value/position edit pushes.
    fn def_with_scale(value: f32) -> EffectGraphDef {
        let mut params = BTreeMap::new();
        params.insert("scale".to_string(), SerializedParamValue::Float { value });
        EffectGraphDef {
            version: EFFECT_GRAPH_VERSION,
            name: None,
            description: None,
            preset_metadata: None,
            nodes: vec![EffectGraphNode {
                id: 0,
                node_id: NodeId::new("feedback"),
                type_id: "node.transform".to_string(),
                handle: Some("feedback".to_string()),
                params,
                exposed_params: Default::default(),
                editor_pos: None,
                wgsl_source: None,
                title: None,
                output_formats: BTreeMap::new(),
                output_canvas_scales: BTreeMap::new(),
                group: None,
            }],
            wires: vec![],
        }
    }

    /// The anti-divergence property both runtimes now inherit: pushing a def's
    /// inner-param overrides MUST clear the binding cache, so a bound slider
    /// re-asserts its card value over the baked def value on the next apply.
    /// This is the structural form of the OilyFluid Speed snap-back fix — proved
    /// once on the shared unit instead of twice on each runtime.
    #[test]
    fn apply_inner_overrides_clears_cache_so_bound_value_re_asserts() {
        let mut graph = Graph::new();
        let feedback = graph.add_node_named("feedback", Box::new(AffineTransform::new()));
        graph.set_node_id(feedback, NodeId::new("feedback"));
        let node_map = vec![(NodeId::new("feedback"), feedback)];

        // One binding: outer slot 0 → feedback.scale.
        let binding = ResolvedBinding {
            id: Cow::Borrowed("amount"),
            label: Cow::Borrowed("Amount"),
            default_value: 0.5,
            target: ResolvedTarget::Node {
                node: feedback,
                param: "scale",
            },
            convert: ParamConvert::Float,
            source: BindingSource::Static,
            source_id: Cow::Borrowed("amount"),
            reshape: None,
            wraps_angle: false,
        };
        let mut bound = BoundGraph::new(vec![binding], &mut graph);

        // Card drives scale to 0.3.
        bound.apply(
            &mut graph,
            &ParamManifest::from_params(vec![slot("amount", 0.3, true)]),
        );
        assert_eq!(scale_of(&graph, feedback), ParamValue::Float(0.3));

        // An editor value/position edit pushes the baked def value (0.9) into
        // every inner node, clobbering the bound scale.
        bound.apply_inner_overrides(&mut graph, &node_map, Some(&def_with_scale(0.9)));
        assert_eq!(
            scale_of(&graph, feedback),
            ParamValue::Float(0.9),
            "override must land the def value first",
        );

        // Re-apply the SAME card value. The cache was cleared by
        // `apply_inner_overrides`, so the binding re-writes 0.3 even though the
        // outer slot didn't move. Without the clear this would skip and the
        // slider would be stuck at 0.9 — the snap-back.
        bound.apply(
            &mut graph,
            &ParamManifest::from_params(vec![slot("amount", 0.3, true)]),
        );
        assert_eq!(
            scale_of(&graph, feedback),
            ParamValue::Float(0.3),
            "bound card value must re-assert over the def override after the \
             cache clear (the structural OilyFluid Speed fix)",
        );
    }
}
