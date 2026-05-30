//! V1 composite presets — sub-graphs of primitives that ship as named
//! effects (Bloom, Halation, etc.).
//!
//! ## Approach for V1
//!
//! Each composite is a function that takes the outer [`Graph`] and an
//! input wire endpoint, splices its inner sub-graph in, and returns a
//! [`CompositeHandle`] exposing the output port and parameter routing.
//!
//! That's deliberately lightweight. The alternative — making each
//! composite a `Box<dyn EffectNode>` that the executor inlines at compile
//! time — would also work but adds a real chunk of compile-time graph
//! rewriting machinery. For V1 the function-based approach validates the
//! same thing (primitives compose into preset shapes, parameter routing
//! works, real V1 composites can be built end-to-end) without the
//! rewriting infrastructure.
//!
//! When the editor lands and needs a "click the cog to see Bloom's
//! internals" UX, [`CompositeHandle::inner_nodes`] gives us the node-id
//! group that came from one composite — enough to draw it as a
//! collapsible cluster in the editor and to round-trip composites through
//! save/load.
//!
//! ## V1 set
//!
//! - [`build_infrared`]: `Brightness → ColorRamp`.
//! - [`build_soft_focus`]: `Blur` + `Mix(source, blurred)`.
//!
//! ## Why no `build_color_compass` here
//!
//! New post-§11 effects ship as JSON-only — the `composite.color_compass`
//! preset lives at `assets/effect-presets/ColorCompass.json` and is
//! loaded into the registry through the standard `LoadedPresetSource`
//! path. The Rust builders above predate the JSON-authoritative
//! migration; they're kept because their parity tests (e.g.
//! [`build_strobe_opacity`] vs the legacy fused `node.strobe`) need
//! both graphs constructable in the same test. Effects with no legacy
//! to compare against don't need a Rust builder.

mod infrared;
mod soft_focus;
mod strobe_opacity;

pub use infrared::{INFRARED_TYPE_ID, build_infrared};
pub use soft_focus::{SOFT_FOCUS_TYPE_ID, build_soft_focus};
pub use strobe_opacity::{STROBE_OPACITY_TYPE_ID, build_strobe_opacity};

use ahash::AHashMap;

use crate::node_graph::effect_node::{EffectNodeType, NodeInstanceId};
use crate::node_graph::graph::Graph;
use crate::node_graph::parameters::ParamValue;
use crate::node_graph::validation::GraphError;

/// Returned by every composite-builder function. Tracks the wire endpoint
/// to use as the composite's output, the routing from outer parameter
/// names to the inner `(node, param)` they drive, and the full set of
/// inner node ids (so a future editor can draw the composite as a
/// collapsible group and so save/load can identify the cluster).
pub struct CompositeHandle {
    type_id: EffectNodeType,
    output: (NodeInstanceId, &'static str),
    param_routing: AHashMap<&'static str, (NodeInstanceId, &'static str)>,
    inner_nodes: Vec<NodeInstanceId>,
}

impl CompositeHandle {
    /// Construct a handle for a composite whose output port is `(node, port_name)`.
    pub fn new(type_id: &'static str, output: (NodeInstanceId, &'static str)) -> Self {
        Self {
            type_id: EffectNodeType::new(type_id),
            output,
            param_routing: AHashMap::default(),
            inner_nodes: Vec::new(),
        }
    }

    /// Record an inner node as belonging to this composite (for editor /
    /// save-load purposes; doesn't affect runtime).
    pub fn add_inner(&mut self, node: NodeInstanceId) -> &mut Self {
        self.inner_nodes.push(node);
        self
    }

    /// Expose an inner node's parameter under an outer name.
    /// Outer names are the slots that will appear on the effect card.
    pub fn expose_param(
        &mut self,
        outer_name: &'static str,
        inner_node: NodeInstanceId,
        inner_param: &'static str,
    ) -> &mut Self {
        self.param_routing
            .insert(outer_name, (inner_node, inner_param));
        self
    }

    pub fn type_id(&self) -> &EffectNodeType {
        &self.type_id
    }

    /// The wire endpoint downstream nodes connect to.
    pub fn output(&self) -> (NodeInstanceId, &'static str) {
        self.output
    }

    /// Inner node ids, in insertion order. The composite can be identified
    /// by this set; deleting them from the graph removes the composite.
    pub fn inner_nodes(&self) -> &[NodeInstanceId] {
        &self.inner_nodes
    }

    /// Outer parameter names this composite exposes.
    pub fn exposed_params(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.param_routing.keys().copied()
    }

    /// Resolve an outer parameter name to its inner (node, param)
    /// destination, or `None` if no such routing exists. Used by the
    /// editor inspector to flag inner params that an outer effect-
    /// card slider drives every frame.
    pub fn inner_routing_for(
        &self,
        outer_name: &str,
    ) -> Option<(NodeInstanceId, &'static str)> {
        self.param_routing.get(outer_name).copied()
    }

    /// Set an exposed parameter by its outer name. Routes through to the
    /// underlying inner node's parameter.
    pub fn set_param(
        &self,
        graph: &mut Graph,
        outer_name: &str,
        value: ParamValue,
    ) -> Result<(), GraphError> {
        let (node, inner_name) = self.param_routing.get(outer_name).copied().ok_or_else(|| {
            GraphError::ParamNotFound {
                // sentinel: this is a composite-level lookup, not a node-level one.
                node: NodeInstanceId(u32::MAX),
                param: outer_name.to_string(),
            }
        })?;
        graph.set_param(node, inner_name, value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    use manifold_core::{Beats, Seconds};

    use crate::node_graph::{Executor, FinalOutput, FrameTime, Graph, Source, compile, validate};

    fn frame_time() -> FrameTime {
        FrameTime {
            beats: Beats(0.0),
            seconds: Seconds(0.0),
            delta: Seconds(1.0 / 60.0),
            frame_count: 0,
        }
    }

    /// Helper: build `[Source → composite_builder → FinalOutput]` and run
    /// it once. Used by every composite test below for symmetry.
    fn run_composite_in_graph(
        builder: impl FnOnce(
            &mut Graph,
            (NodeInstanceId, &'static str),
        ) -> Result<CompositeHandle, GraphError>,
    ) -> (Graph, CompositeHandle) {
        let mut g = Graph::new();
        let src = g.add_node(Box::new(Source::new()));
        let handle = builder(&mut g, (src, "out")).unwrap();
        let out = g.add_node(Box::new(FinalOutput::new()));
        g.connect(handle.output(), (out, "in")).unwrap();

        validate(&g).unwrap();
        let plan = compile(&g).unwrap();
        let mut exec = Executor::with_mock();
        exec.execute_frame(&mut g, &plan, frame_time());
        (g, handle)
    }

    #[test]
    fn all_v1_composite_type_ids_are_unique_and_prefixed() {
        let mut g = Graph::new();
        let src = g.add_node(Box::new(Source::new()));
        let source_endpoint = (src, "out");

        // Build each composite into its own scratch graph (cheap) just to
        // collect their type IDs and assert the invariants.
        let ids: HashSet<&str> = [
            INFRARED_TYPE_ID,
            SOFT_FOCUS_TYPE_ID,
        ]
        .into_iter()
        .collect();
        assert_eq!(ids.len(), 2, "composite type IDs must be unique");

        for id in ids {
            assert!(
                id.starts_with("composite."),
                "composite type IDs must start with `composite.` — got {id}"
            );
        }

        // Sanity: each builder is callable.
        let _ = build_infrared(&mut g, source_endpoint);
    }

    #[test]
    fn infrared_compiles_executes_and_routes_color_params() {
        let (mut g, handle) = run_composite_in_graph(build_infrared);
        assert_eq!(handle.type_id().as_str(), INFRARED_TYPE_ID);
        assert_eq!(handle.inner_nodes().len(), 2);

        // Outer param routing: setting `color_a` on the handle routes to
        // ColorRamp's `color_a`.
        handle
            .set_param(&mut g, "color_a", ParamValue::Color([1.0, 0.0, 0.0, 1.0]))
            .unwrap();
        // Unknown outer param surfaces as a clean error.
        assert!(
            handle
                .set_param(&mut g, "nonexistent", ParamValue::Float(0.0))
                .is_err()
        );
    }

    #[test]
    fn soft_focus_uses_two_inner_nodes_and_exposes_radius_and_amount() {
        let (mut g, handle) = run_composite_in_graph(build_soft_focus);
        assert_eq!(handle.inner_nodes().len(), 2);
        let exposed: HashSet<&'static str> = handle.exposed_params().collect();
        assert!(exposed.contains("radius"));
        assert!(exposed.contains("amount"));
        handle
            .set_param(&mut g, "radius", ParamValue::Float(8.0))
            .unwrap();
        handle
            .set_param(&mut g, "amount", ParamValue::Float(0.7))
            .unwrap();
    }

    /// Hero test: chain two composites in series in the same graph.
    /// Validates that composites compose with each other, parameter
    /// routing remains independent per instance, and inner nodes from
    /// different composites share the same outer pool.
    #[test]
    fn two_composites_in_series_compose() {
        let mut g = Graph::new();
        let src = g.add_node(Box::new(Source::new()));
        let focused = build_soft_focus(&mut g, (src, "out")).unwrap();
        let infrared_after_focus = build_infrared(&mut g, focused.output()).unwrap();
        let out = g.add_node(Box::new(FinalOutput::new()));
        g.connect(infrared_after_focus.output(), (out, "in"))
            .unwrap();

        validate(&g).unwrap();
        let plan = compile(&g).unwrap();
        let mut exec = Executor::with_mock();
        exec.execute_frame(&mut g, &plan, frame_time());

        // SoftFocus (2 inner) + Infrared (2 inner) + Source + FinalOutput = 6 nodes.
        assert_eq!(g.node_count(), 6);
        // SoftFocus's and Infrared's inner-node sets are disjoint.
        let focus_inner: HashSet<NodeInstanceId> = focused.inner_nodes().iter().copied().collect();
        let infrared_inner: HashSet<NodeInstanceId> =
            infrared_after_focus.inner_nodes().iter().copied().collect();
        assert!(focus_inner.is_disjoint(&infrared_inner));
    }
}
