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
//! New post-section 11 effects ship as JSON-only — the `composite.color_compass`
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

#[cfg(test)]
use manifold_node_engine::param_binding::composite_handle::CompositeHandle;

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_node_engine::{exec::effect_node::NodeInstanceId, validation::GraphError, parameters::ParamValue};
    use std::collections::HashSet;

    use manifold_core::{Beats, Seconds};

    use manifold_node_engine::{exec::execution::Executor, scene::boundary_nodes::FinalOutput, exec::effect_node::FrameTime, graph::Graph, scene::boundary_nodes::Source, exec::execution_plan::compile, validate};

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
