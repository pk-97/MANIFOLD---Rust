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
    use crate::validation::validate;
    use super::*;

    use manifold_core::{Beats, Seconds};

    use crate::{exec::execution::Executor, exec::effect_node::FrameTime, graph::Graph, exec::execution_plan::compile};

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
            Err(crate::validation::GraphError::RequiredInputUnwired { .. })
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


}
