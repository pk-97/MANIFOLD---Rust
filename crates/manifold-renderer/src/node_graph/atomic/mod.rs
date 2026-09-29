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

    use manifold_core::{Beats, Seconds};

    use crate::node_graph::{
        Executor, FinalOutput, FrameTime, Graph, Source, compile, validate,
    };

    fn frame_time() -> FrameTime {
        FrameTime {
            beats: Beats(0.0),
            seconds: Seconds(0.0),
            delta: Seconds(1.0 / 60.0),
            frame_count: 0,
        }
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
    fn fluid_sim_alone_is_valid_and_runs() {
        // All inputs optional → FluidSim is a legal standalone graph.
        // Outputs with no downstream consumer are skipped by the
        // planner (since A — "don't pay for what nobody reads"), so
        // a lone FluidSim allocates zero resources and zero slots.
        let mut g = Graph::new();
        g.add_node(Box::new(FluidSim2D::new()));
        validate(&g).unwrap();
        let plan = compile(&g).unwrap();
        assert_eq!(plan.steps().len(), 1);
        assert_eq!(plan.resource_count(), 0);
        let mut exec = Executor::with_mock();
        exec.execute_frame(&mut g, &plan, frame_time());
        assert_eq!(exec.backend().slot_count(), 0);
    }

    /// Hero test: FluidSim's auxiliary output (density) wired through
    /// downstream nodes (Threshold → Mix with the source) and out to
    /// FinalOutput. Validates the whole atomic-with-rich-ports flow:
    /// the host can use FluidSim's internals for compositing, not just
    /// its main composited output.
    #[test]
    fn fluid_sim_density_can_be_wired_downstream() {
        use crate::node_graph::primitives::{Mix, Threshold};

        let mut g = Graph::new();
        let src = g.add_node(Box::new(Source::new()));
        let fluid = g.add_node(Box::new(FluidSim2D::new()));
        let thresh = g.add_node(Box::new(Threshold::new()));
        let mix = g.add_node(Box::new(Mix::new()));
        let out = g.add_node(Box::new(FinalOutput::new()));

        // Source feeds FluidSim's source AND Mix's `a` input (fan-out).
        g.connect((src, "out"), (fluid, "source")).unwrap();
        g.connect((src, "out"), (mix, "a")).unwrap();

        // FluidSim's auxiliary `density` output drives a Threshold whose
        // result becomes the Mix `b` input.
        g.connect((fluid, "density"), (thresh, "source")).unwrap();
        g.connect((thresh, "out"), (mix, "b")).unwrap();

        g.connect((mix, "out"), (out, "in")).unwrap();

        validate(&g).unwrap();
        let plan = compile(&g).unwrap();
        let mut exec = Executor::with_mock();
        exec.execute_frame(&mut g, &plan, frame_time());
    }
}
