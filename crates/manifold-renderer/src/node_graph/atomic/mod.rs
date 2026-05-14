//! V1 atomic nodes — irreducibly one thing, opaque internals, sometimes
//! with rich port surfaces that expose internal data.
//!
//! Atomic nodes can't sensibly be decomposed into primitives — the kernel
//! IS the node. Three V1 examples cover the spectrum:
//!
//! - [`Plasma`]: zero-input pure generator. Simplest atomic shape.
//! - [`FluidSim2D`]: hero example. Multiple optional inputs (including a
//!   scalar input), four outputs (one default, three auxiliary "expose
//!   the internals" ports). Stateful (clear_state implemented).
//! - [`Glitch`]: single-shader multi-mode effect. The "tight kernel that
//!   doesn't decompose" case.

mod fluid_sim;
mod glitch;
mod plasma;

pub use fluid_sim::{FLUID_SIM_2D_TYPE_ID, FluidSim2D};
pub use glitch::{GLITCH_MODES, GLITCH_TYPE_ID, Glitch};
pub use plasma::{PLASMA_TYPE_ID, Plasma};

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    use manifold_core::{Beats, Seconds};

    use crate::node_graph::{
        EffectNode, Executor, FinalOutput, FrameTime, Graph, PortType, Source, compile, validate,
    };

    fn frame_time() -> FrameTime {
        FrameTime {
            beats: Beats(0.0),
            seconds: Seconds(0.0),
            delta: Seconds(1.0 / 60.0),
            frame_count: 0,
        }
    }

    fn all_atomics() -> Vec<Box<dyn EffectNode>> {
        vec![
            Box::new(Plasma::new()),
            Box::new(FluidSim2D::new()),
            Box::new(Glitch::new()),
        ]
    }

    #[test]
    fn all_v1_atomic_type_ids_are_unique_and_prefixed() {
        let atomics = all_atomics();
        let ids: HashSet<&str> = atomics.iter().map(|a| a.type_id().as_str()).collect();
        assert_eq!(ids.len(), 3);
        for a in atomics {
            assert!(
                a.type_id().as_str().starts_with("atomic."),
                "atomic type IDs must start with `atomic.` — got {}",
                a.type_id().as_str()
            );
        }
    }

    #[test]
    fn plasma_is_a_pure_generator() {
        let p = Plasma::new();
        assert_eq!(p.inputs().len(), 0);
        assert_eq!(p.outputs().len(), 1);
        assert_eq!(p.outputs()[0].name, "out");
    }

    #[test]
    fn plasma_alone_compiles_and_runs() {
        let mut g = Graph::new();
        g.add_node(Box::new(Plasma::new()));
        validate(&g).unwrap();
        let plan = compile(&g).unwrap();
        assert_eq!(plan.steps().len(), 1);
        let mut exec = Executor::with_mock();
        exec.execute_frame(&mut g, &plan, frame_time());
    }

    #[test]
    fn glitch_requires_source_input() {
        // Glitch with no source → required-input validation error.
        let mut g = Graph::new();
        g.add_node(Box::new(Glitch::new()));
        assert!(matches!(
            validate(&g),
            Err(crate::node_graph::GraphError::RequiredInputUnwired { .. })
        ));
    }

    #[test]
    fn fluid_sim_has_four_outputs_one_default_three_auxiliary() {
        let f = FluidSim2D::new();
        assert_eq!(f.outputs().len(), 4);
        let names: Vec<&str> = f.outputs().iter().map(|p| p.name).collect();
        assert_eq!(names, vec!["composited", "density", "velocity", "pressure"]);
        // First output is the conventional default.
        assert_eq!(f.outputs()[0].name, "composited");
    }

    #[test]
    fn fluid_sim_has_only_optional_inputs() {
        // FluidSim is a hybrid generator/effect — every input is optional
        // so the same node can run as a pure generator OR as an effect
        // depending on what the user wires.
        let f = FluidSim2D::new();
        for input in f.inputs() {
            assert!(
                !input.required,
                "FluidSim2D input `{}` should be optional",
                input.name
            );
        }
    }

    #[test]
    fn fluid_sim_has_a_scalar_input_port() {
        // Validates that Scalar(Vec3) input ports are supported by the
        // trait surface end-to-end. dye_color is the first scalar input
        // port we declare; all primitive inputs are Texture2D.
        let f = FluidSim2D::new();
        let dye = f
            .inputs()
            .iter()
            .find(|p| p.name == "dye_color")
            .expect("FluidSim2D must declare a dye_color input");
        assert!(matches!(
            dye.ty,
            PortType::Scalar(crate::node_graph::ScalarType::Vec3)
        ));
    }

    #[test]
    fn fluid_sim_alone_is_valid_and_runs() {
        // All inputs optional → FluidSim is a legal standalone graph.
        // Allocates 4 output slots (composited + 3 auxiliary).
        let mut g = Graph::new();
        g.add_node(Box::new(FluidSim2D::new()));
        validate(&g).unwrap();
        let plan = compile(&g).unwrap();
        assert_eq!(plan.steps().len(), 1);
        assert_eq!(plan.resource_count(), 4); // 4 output ports = 4 resources
        let mut exec = Executor::with_mock();
        exec.execute_frame(&mut g, &plan, frame_time());
        // Each unread output occupies its own slot — 4 slots high-water mark.
        assert_eq!(exec.backend().slot_count(), 4);
    }

    /// Hero test: FluidSim's auxiliary output (density) wired through
    /// downstream nodes (Threshold → Blend with the source) and out to
    /// FinalOutput. Validates the whole atomic-with-rich-ports flow:
    /// the host can use FluidSim's internals for compositing, not just
    /// its main composited output.
    #[test]
    fn fluid_sim_density_can_be_wired_downstream() {
        use crate::node_graph::primitives::{Blend, Threshold};

        let mut g = Graph::new();
        let src = g.add_node(Box::new(Source::new()));
        let fluid = g.add_node(Box::new(FluidSim2D::new()));
        let thresh = g.add_node(Box::new(Threshold::new()));
        let blend = g.add_node(Box::new(Blend::new()));
        let out = g.add_node(Box::new(FinalOutput::new()));

        // Source feeds FluidSim's source AND Blend's base (fan-out).
        g.connect((src, "out"), (fluid, "source")).unwrap();
        g.connect((src, "out"), (blend, "base")).unwrap();

        // FluidSim's auxiliary `density` output drives a Threshold whose
        // result becomes the Blend overlay.
        g.connect((fluid, "density"), (thresh, "source")).unwrap();
        g.connect((thresh, "out"), (blend, "overlay")).unwrap();

        g.connect((blend, "out"), (out, "in")).unwrap();

        validate(&g).unwrap();
        let plan = compile(&g).unwrap();
        let mut exec = Executor::with_mock();
        exec.execute_frame(&mut g, &plan, frame_time());
    }
}
