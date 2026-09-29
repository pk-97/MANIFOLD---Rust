//! Executor repeat on the GPU (`docs/GPU_MPM_SOLVER_DESIGN.md` D7, phase
//! P0b): per-iteration uniforms reach every dispatch, and a frozen (fused)
//! region body runs exactly like the unfused one.
//!
//! The graph: `seed → boundary`; body `boundary.out → mover_a (speed ←
//! step_dt) → mover_b (speed ← step_index) → boundary.in`; `boundary.out →
//! sink → final_output` keeps the region live. Both movers are
//! `node.move_particles_3d`, so the body fuses into one buffer kernel.

use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_core::{Beats, Seconds};
use manifold_gpu::GpuTextureFormat;
use manifold_renderer::generators::compute_common::Particle;
use manifold_renderer::gpu_encoder::GpuEncoder;
use manifold_renderer::node_graph::freeze::install::fuse_generator_view;
use manifold_renderer::node_graph::substeps::test_nodes::{
    particle_step_dt, register_substep_test_nodes,
};
use manifold_renderer::node_graph::{
    Backend, EffectGraphDefExt, ExecutionPlan, Executor, FrameTime, Graph, MetalBackend,
    NodeInstanceId, PrimitiveRegistry, ResourceId, StateStore, compile, pre_allocate_resources,
};

use crate::harness;

const N: usize = 1000;
const ITERATIONS: u32 = 4;
const FRAMES: u32 = 3;

fn def() -> EffectGraphDef {
    serde_json::from_value(serde_json::json!({
        "version": 3,
        "nodes": [
            {"id": 0, "nodeId": "seed", "typeId": "test.particle_source",
             "params": {"max_capacity": {"type": "Int", "value": N}}},
            {"id": 1, "nodeId": "forces", "typeId": "test.force_source",
             "params": {"max_capacity": {"type": "Int", "value": N}}},
            {"id": 2, "nodeId": "boundary", "typeId": "test.particle_boundary",
             "params": {"iterations": {"type": "Int", "value": ITERATIONS}}},
            {"id": 3, "nodeId": "mover_a", "typeId": "node.move_particles_3d"},
            {"id": 4, "nodeId": "mover_b", "typeId": "node.move_particles_3d"},
            {"id": 6, "nodeId": "sink", "typeId": "test.particle_sink"},
            {"id": 7, "nodeId": "output", "typeId": "system.final_output"}
        ],
        "wires": [
            {"fromNode": 0, "fromPort": "out", "toNode": 2, "toPort": "seed"},
            {"fromNode": 2, "fromPort": "out", "toNode": 3, "toPort": "in"},
            {"fromNode": 1, "fromPort": "out", "toNode": 3, "toPort": "forces"},
            {"fromNode": 2, "fromPort": "step_dt", "toNode": 3, "toPort": "speed"},
            {"fromNode": 3, "fromPort": "out", "toNode": 4, "toPort": "in"},
            {"fromNode": 1, "fromPort": "out", "toNode": 4, "toPort": "forces"},
            {"fromNode": 2, "fromPort": "step_index", "toNode": 4, "toPort": "speed"},
            {"fromNode": 4, "fromPort": "out", "toNode": 2, "toPort": "in"},
            {"fromNode": 2, "fromPort": "out", "toNode": 6, "toPort": "particles"},
            {"fromNode": 6, "fromPort": "out", "toNode": 7, "toPort": "in"}
        ]
    }))
    .expect("substep proof def")
}

fn registry() -> PrimitiveRegistry {
    let mut registry = PrimitiveRegistry::with_builtin();
    register_substep_test_nodes(&mut registry);
    registry
}

fn seed_particles() -> Vec<Particle> {
    (0..N)
        .map(|i| Particle {
            position: [i as f32 * 0.001, 0.5, -(i as f32) * 0.0005],
            _pad0: 0.0,
            velocity: [0.0; 3],
            life: if i % 7 == 3 { 0.0 } else { 1.0 },
            age: 0.0,
            _pad1: [0.0; 3],
            color: [1.0; 4],
        })
        .collect()
}

fn forces() -> Vec<[f32; 3]> {
    (0..N)
        .map(|i| [0.01 + i as f32 * 1.0e-5, -0.02, 0.003 * (i % 5) as f32])
        .collect()
}

fn node_of(graph: &Graph, type_id: &str) -> NodeInstanceId {
    graph
        .nodes()
        .find(|n| n.node.type_id().as_str() == type_id)
        .map(|n| n.id)
        .unwrap_or_else(|| panic!("graph has no `{type_id}`"))
}

fn resource(plan: &ExecutionPlan, node: NodeInstanceId, port: &str, output: bool) -> ResourceId {
    let step = plan.steps().iter().find(|s| s.node == node).expect("node compiled");
    let list = if output { &step.outputs } else { &step.inputs };
    list.iter()
        .find(|(name, _)| *name == port)
        .map(|&(_, r)| r)
        .unwrap_or_else(|| panic!("no port `{port}`"))
}

struct Run {
    particles: Vec<Particle>,
    /// The boundary's capture and state resolve to one buffer.
    in_place: bool,
}

fn run(mut graph: Graph) -> Run {
    let harness = harness::shared();
    let device = &harness.device;
    let plan = compile(&graph).expect("proof def compiles");
    assert_eq!(plan.substep_regions().len(), 1, "one substep region");
    let mut backend = MetalBackend::new(device.clone(), 64, 64, GpuTextureFormat::Rgba16Float);
    pre_allocate_resources(&graph, &plan, device, &mut backend).expect("pre-allocate");

    let boundary = node_of(&graph, "test.particle_boundary");
    let seed_res = resource(&plan, node_of(&graph, "test.particle_source"), "out", true);
    let force_res = resource(&plan, node_of(&graph, "test.force_source"), "out", true);
    let out_res = resource(&plan, boundary, "out", true);
    let in_res = resource(&plan, boundary, "in", false);
    for (res, bytes) in [
        (seed_res, bytemuck::cast_slice::<Particle, u8>(&seed_particles()).to_vec()),
        (force_res, bytemuck::cast_slice::<[f32; 3], u8>(&forces()).to_vec()),
    ] {
        let slot = backend.slot_for(res).expect("array bound");
        let buffer = Backend::array_buffer(&backend, slot).expect("array buffer");
        assert!(buffer.size as usize >= bytes.len());
        // SAFETY: shared-storage buffer, no GPU work in flight yet.
        unsafe { buffer.write(0, &bytes) };
    }

    let mut exec = Executor::new(Box::new(backend));
    let mut state = StateStore::new();
    for frame in 0..FRAMES {
        let time = FrameTime {
            beats: Beats(0.0),
            seconds: Seconds(f64::from(frame) / 60.0),
            delta: Seconds(1.0 / 60.0),
            frame_count: i64::from(frame),
        };
        let mut enc = device.create_encoder("substeps-proof");
        {
            let mut gpu = GpuEncoder::new(&mut enc, device);
            exec.execute_frame_with_state(&mut graph, &plan, time, &mut gpu, &mut state, 0);
        }
        enc.commit_and_wait_completed();
    }

    let backend = exec.backend();
    let out = backend
        .array_buffer(backend.slot_for(out_res).expect("state bound"))
        .expect("state buffer");
    let captured = backend
        .array_buffer(backend.slot_for(in_res).expect("capture bound"))
        .expect("capture buffer");
    let ptr = out.mapped_ptr().expect("shared state buffer");
    // SAFETY: the encoder completed; the buffer holds at least N particles.
    let particles =
        unsafe { std::slice::from_raw_parts(ptr.cast::<Particle>().cast_const(), N).to_vec() };
    Run {
        particles,
        in_place: out.ptr_eq(captured),
    }
}

/// CPU reference: per frame, per iteration, `mover_a` then `mover_b`, each
/// `position += force * speed * dt_scaled` in f32.
fn expected() -> Vec<Particle> {
    let dt_scaled = (1.0f64 / 60.0) as f32 * 60.0;
    let forces = forces();
    let mut particles = seed_particles();
    for _ in 0..FRAMES {
        for i in 0..ITERATIONS {
            for speed in [particle_step_dt(i), i as f32] {
                for (p, f) in particles.iter_mut().zip(&forces) {
                    if p.life <= 0.0 {
                        continue;
                    }
                    for (position, force) in p.position.iter_mut().zip(f) {
                        *position += force * speed * dt_scaled;
                    }
                }
            }
        }
    }
    particles
}

#[test]
fn substeps_uniforms_distinct_per_iteration() {
    let registry = registry();
    let graph = def().into_graph(&registry, &Default::default()).expect("proof def builds");
    let got = run(graph);
    assert!(got.in_place, "the unfused body mutates the state buffer in place");
    let want = expected();
    let seed = seed_particles();
    let forces = forces();
    // What one shared uniform would produce: every dispatch sees the last
    // iteration's values.
    let last = ITERATIONS - 1;
    let shared_total =
        FRAMES as f32 * ITERATIONS as f32 * (particle_step_dt(last) + last as f32);
    let mut max_err = 0.0f32;
    for (i, (g, w)) in got.particles.iter().zip(&want).enumerate() {
        for k in 0..3 {
            max_err = max_err.max((g.position[k] - w.position[k]).abs());
        }
        if w.life > 0.0 {
            let shared = seed[i].position[0] + forces[i][0] * shared_total;
            assert!(
                (g.position[0] - shared).abs() > 1.0e-3,
                "particle {i} moved as if every dispatch shared one uniform"
            );
        }
    }
    assert!(max_err <= 1.0e-5, "GPU vs CPU max |Δposition| = {max_err}");
}

#[test]
fn substeps_frozen_unfrozen_match() {
    let registry = registry();
    let canonical = def();
    let view = fuse_generator_view(&canonical, &registry)
        .expect("the body pair fuses and the fused def builds");
    let fused_types: Vec<&str> = view.def.nodes.iter().map(|n| n.type_id.as_str()).collect();
    assert!(
        !fused_types.contains(&"node.move_particles_3d")
            && fused_types.contains(&"node.wgsl_compute"),
        "the body should be one fused kernel: {fused_types:?}"
    );
    let unfused = run(canonical.into_graph(&registry, &Default::default()).expect("builds"));
    let frozen = run(
        (*view.def)
            .clone()
            .into_graph(&registry, &view.mesh_rules)
            .expect("fused def builds"),
    );
    assert!(unfused.in_place && frozen.in_place, "both bodies write the state in place");
    let unfused_bytes: &[u8] = bytemuck::cast_slice(&unfused.particles);
    let frozen_bytes: &[u8] = bytemuck::cast_slice(&frozen.particles);
    assert!(
        unfused_bytes == frozen_bytes,
        "fused body diverged from unfused (buffer regions are bit-exact)"
    );
}
