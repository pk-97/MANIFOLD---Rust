//! End-to-end integration test wiring the particle family into a
//! coherent pipeline using the atom-decomposed integrator:
//!
//! ```text
//! SeedParticles ─→ ArrayFeedback ─→ SampleTextureAtParticles ─→
//!                       ↑                                       ↓
//!                       │                                  EulerStepParticles
//!                       │                                       ↓
//!                       │                                  WrapParticlesTorus
//!                       │                                       │
//!                       └────────── (loop closes via state) ─────┘
//!                                                            │
//!                                                            ↓
//!                                                      ScatterParticles
//!                                                            │
//!                                                            ↓
//!                                                      ResolveAccumulator → density
//! ```
//!
//! Validates the topology builds cleanly through `Graph::connect`'s
//! port-type matching. Each wire crosses an Array(Particle) or
//! Array(vec2<f32>) or Array(u32) or Texture2D boundary that the
//! [`manifold_renderer::node_graph::ports::PortType::Array`] variant has to
//! authorise — if anything in the macro / port-type / validation
//! chain regresses, this test catches it.

use manifold_renderer::node_graph::primitives::{
    ArrayFeedback, EulerStepParticles, ResolveAccumulator, SampleTextureAtParticles,
    ScatterParticles, SeedParticles, WrapParticlesTorus,
};
use manifold_renderer::node_graph::{Graph, Source};

#[test]
fn particle_pipeline_topology_builds_and_connects_with_matching_port_types() {
    let mut g = Graph::new();

    // Velocity producer — in the full FluidSim pipeline this is
    // the output of (Scatter → Resolve → Blur → Gradient → Blur).
    // For the topology test we substitute a host-supplied Source.
    let velocity_source = g.add_node(Box::new(Source::new()));

    let seed = g.add_node(Box::new(SeedParticles::new()));
    let feedback = g.add_node(Box::new(ArrayFeedback::new()));
    let sample = g.add_node(Box::new(SampleTextureAtParticles::new()));
    let euler = g.add_node(Box::new(EulerStepParticles::new()));
    let wrap = g.add_node(Box::new(WrapParticlesTorus::new()));
    let scatter = g.add_node(Box::new(ScatterParticles::new()));
    let resolve = g.add_node(Box::new(ResolveAccumulator::new()));

    // Particle stream: Seed → Feedback → Sample → Euler → Wrap.
    // The loop closes via Feedback's state, not via wires.
    g.connect((seed, "particles"), (feedback, "in"))
        .expect("Seed.particles → Feedback.in: matching Array(Particle) layout");
    g.connect((feedback, "out"), (sample, "particles"))
        .expect("Feedback.out → Sample.particles: matching Array(Particle) layout");
    g.connect((velocity_source, "out"), (sample, "in"))
        .expect("Source.out → Sample.in: matching Texture2D");

    // Sample → Euler: Array<vec2<f32>> forces flow alongside the
    // particle stream so Euler can update positions in place.
    g.connect((feedback, "out"), (euler, "in"))
        .expect("Feedback.out → Euler.in: matching Array(Particle) layout");
    g.connect((sample, "out"), (euler, "forces"))
        .expect("Sample.out → Euler.forces: matching Array(vec2<f32>) layout");

    // Euler → Wrap: aliased Array(Particle) carrying the updated
    // positions; Wrap applies the toroidal boundary in place.
    g.connect((euler, "out"), (wrap, "in"))
        .expect("Euler.out → Wrap.in: matching Array(Particle) layout");

    // Wrap → Scatter: same aliased buffer reaches the splat stage.
    g.connect((wrap, "out"), (scatter, "particles"))
        .expect("Wrap.out → Scatter.particles: matching Array(Particle) layout");

    // Accumulator → density texture. Crosses Array(u32) → Texture2D
    // boundary via ResolveAccumulator.
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
fn sample_demands_a_field_texture_in_addition_to_particles() {
    // Sample's `in` (the field texture) is a required Texture2D
    // input. The graph must reject validation if the particle
    // stream is wired but the field input is missing.
    let mut g = Graph::new();
    let seed = g.add_node(Box::new(SeedParticles::new()));
    let sample = g.add_node(Box::new(SampleTextureAtParticles::new()));

    g.connect((seed, "particles"), (sample, "particles")).unwrap();
    // No `in` (field texture) wire.

    let validation = manifold_renderer::node_graph::validate(&g);
    assert!(
        validation.is_err(),
        "graph with unwired required Sample.in should fail validation",
    );
}
