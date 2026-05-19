//! Phase A.8 of `BUFFER_PORT_PLAN`. End-to-end integration test
//! wiring the particle family into a coherent pipeline:
//!
//! ```text
//! SeedParticles ──→ ArrayFeedback ──→ IntegrateParticles
//!                          ↑                  │
//!                          │                  ↓
//!                          └────────────  (loop closes via state)
//!                                            │
//!                                            ↓
//!                                       ScatterParticles
//!                                            │
//!                                            ↓
//!                                       ResolveAccumulator → density texture
//! ```
//!
//! Validates the topology builds cleanly through `Graph::connect`'s
//! port-type matching. Each wire crosses an Array(Particle) or
//! Array(u32) or Texture2D boundary that the new
//! [`crate::node_graph::ports::PortType::Array`] variant has to
//! authorise — if anything in the macro / port-type / validation
//! chain regresses, this test catches it.
//!
//! Full GPU dispatch and pixel-exact FluidSim parity are deferred
//! follow-up work — the legacy FluidSim pipeline has additional
//! blur + gradient + display stages and seven legacy seed patterns
//! that need primitive-level companions. This test validates the
//! shape of the foundation; the further build-out happens on top.

use manifold_renderer::node_graph::primitives::{
    ArrayFeedback, IntegrateParticles, ResolveAccumulator, ScatterParticles, SeedParticles,
};
use manifold_renderer::node_graph::{Graph, Source};

// `manifold-renderer` doesn't directly re-export the boundary
// nodes, but the test only needs to wire the four particle
// primitives among themselves — boundary plumbing is exercised
// by the chain-level tests that ship inside the crate.

#[test]
fn particle_pipeline_topology_builds_and_connects_with_matching_port_types() {
    let mut g = Graph::new();

    // Velocity producer — in the full FluidSim pipeline this is
    // the output of (Scatter → Resolve → Blur → Gradient → Blur).
    // For the topology test we substitute a host-supplied Source.
    let velocity_source = g.add_node(Box::new(Source::new()));

    let seed = g.add_node(Box::new(SeedParticles::new()));
    let feedback = g.add_node(Box::new(ArrayFeedback::new()));
    let integrate = g.add_node(Box::new(IntegrateParticles::new()));
    let scatter = g.add_node(Box::new(ScatterParticles::new()));
    let resolve = g.add_node(Box::new(ResolveAccumulator::new()));

    // Particle stream: Seed → Feedback → Integrate (loop closes
    // via Feedback's state, not via wires).
    g.connect((seed, "particles"), (feedback, "in"))
        .expect("Seed.particles → Feedback.in: matching Array(Particle) layout");
    g.connect((feedback, "out"), (integrate, "in"))
        .expect("Feedback.out → Integrate.in: matching Array(Particle) layout");
    g.connect((velocity_source, "out"), (integrate, "velocity"))
        .expect("Source.out → Integrate.velocity: matching Texture2D");

    // After Integrate writes back, downstream Scatter reads the
    // same buffer (chain build aliases Integrate.out and
    // Integrate.in slots; the wire here is the graph-level
    // declaration of dataflow).
    g.connect((integrate, "out"), (scatter, "particles"))
        .expect("Integrate.out → Scatter.particles: matching Array(Particle) layout");

    // Accumulator → density texture. This crosses port-type
    // families (Array(u32) → Texture2D via the ResolveAccumulator
    // primitive). The wire validation only checks each connection;
    // ResolveAccumulator's input is Array(u32) so the scatter
    // output must match.
    g.connect((scatter, "accum"), (resolve, "accum"))
        .expect("Scatter.accum → Resolve.accum: matching Array(u32) layout");

    // Validate the complete graph — no cycles (Feedback breaks the
    // logical loop via state), every node reachable, every required
    // input wired.
    let validation = manifold_renderer::node_graph::validate(&g);
    assert!(
        validation.is_ok(),
        "complete particle pipeline should validate cleanly: {validation:?}",
    );
}

#[test]
fn array_particle_port_rejects_array_u32_connection() {
    // Regression guard: even though both ends carry Array
    // PortType variants, the (item_size, item_align) descriptors
    // differ. The graph must reject the connection rather than
    // silently produce undefined GPU behavior.
    let mut g = Graph::new();
    let seed = g.add_node(Box::new(SeedParticles::new()));
    let resolve = g.add_node(Box::new(ResolveAccumulator::new()));

    let r = g.connect((seed, "particles"), (resolve, "accum"));
    assert!(
        r.is_err(),
        "Array(Particle) wire must NOT connect to an Array(u32) port",
    );
}

#[test]
fn integrate_demands_a_velocity_texture_in_addition_to_particles() {
    // Integrate's `velocity` is a required Texture2D input. The
    // graph must reject validation if the particle stream is wired
    // but the velocity input is missing.
    let mut g = Graph::new();
    let seed = g.add_node(Box::new(SeedParticles::new()));
    let integrate = g.add_node(Box::new(IntegrateParticles::new()));

    g.connect((seed, "particles"), (integrate, "in")).unwrap();
    // No `velocity` wire.

    let validation = manifold_renderer::node_graph::validate(&g);
    assert!(
        validation.is_err(),
        "graph with unwired required Integrate.velocity should fail validation",
    );
}
