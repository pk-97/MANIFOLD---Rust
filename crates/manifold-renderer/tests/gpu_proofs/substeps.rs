//! Executor repeat on the GPU (`docs/GPU_MPM_SOLVER_DESIGN.md` D7, phase
//! P0b): per-iteration uniforms reach every dispatch, and a frozen (fused)
//! region body runs exactly like the unfused one.
//!
//! The graph: `seed → boundary`; body `boundary.out → mover_a (speed ←
//! step_dt) → mover_b (speed ← step_index) → boundary.in`; `boundary.out →
//! sink → final_output` keeps the region live. Both movers are
//! `node.move_particles_3d`, so the body fuses into one buffer kernel.
//!
//! Nested regions (`docs/LIQUID_SOLVER_SEAM_DESIGN.md` P6): a 3 × 4 nest
//! equals the same movers unrolled, and fuses per level without changing a
//! bit.

use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_core::{Beats, Seconds};
use manifold_gpu::GpuTextureFormat;
use manifold_renderer::generators::compute_common::Particle;
use manifold_renderer::gpu_encoder::GpuEncoder;
use manifold_renderer::node_graph::freeze::install::fuse_generator_view;
use manifold_renderer::node_graph::ports::PortType;
use manifold_renderer::node_graph::resource_allocation::plan_array_allocations;
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

// ─── Nested regions ───
//
// ```text
// seed ─▶ outer (OUTER iterations)
// outer.out ─▶ outer_a (speed ← outer.step_dt) ─▶ outer_b (speed ← outer.step_index)
//   ─▶ inner.seed (INNER iterations, restarts from its seed every outer iteration)
// inner.out ─▶ inner_a (speed ← inner.step_dt) ─▶ inner_b (speed ← inner.step_index) ─▶ inner.in
// inner.out ─▶ post (speed ← outer.step_dt) ─▶ outer.in
// outer.out ─▶ sink ─▶ final_output
// ```
// `outer_a`/`outer_b` fuse into one outer-body kernel and `inner_a`/`inner_b`
// into one inner-body kernel; `post` stays alone.

const OUTER: u32 = 3;
const INNER: u32 = 4;

fn nested_def() -> EffectGraphDef {
    serde_json::from_value(serde_json::json!({
        "version": 3,
        "nodes": [
            {"id": 0, "nodeId": "seed", "typeId": "test.particle_source",
             "params": {"max_capacity": {"type": "Int", "value": N}}},
            {"id": 1, "nodeId": "forces", "typeId": "test.force_source",
             "params": {"max_capacity": {"type": "Int", "value": N}}},
            {"id": 2, "nodeId": "outer", "typeId": "test.particle_boundary",
             "params": {"iterations": {"type": "Int", "value": OUTER}}},
            {"id": 3, "nodeId": "outer_a", "typeId": "node.move_particles_3d"},
            {"id": 4, "nodeId": "outer_b", "typeId": "node.move_particles_3d"},
            {"id": 5, "nodeId": "inner", "typeId": "test.particle_inner_boundary",
             "params": {"iterations": {"type": "Int", "value": INNER}}},
            {"id": 6, "nodeId": "inner_a", "typeId": "node.move_particles_3d"},
            {"id": 7, "nodeId": "inner_b", "typeId": "node.move_particles_3d"},
            {"id": 8, "nodeId": "post", "typeId": "node.move_particles_3d"},
            {"id": 9, "nodeId": "sink", "typeId": "test.particle_sink"},
            {"id": 10, "nodeId": "output", "typeId": "system.final_output"}
        ],
        "wires": [
            {"fromNode": 0, "fromPort": "out", "toNode": 2, "toPort": "seed"},
            {"fromNode": 2, "fromPort": "out", "toNode": 3, "toPort": "in"},
            {"fromNode": 1, "fromPort": "out", "toNode": 3, "toPort": "forces"},
            {"fromNode": 2, "fromPort": "step_dt", "toNode": 3, "toPort": "speed"},
            {"fromNode": 3, "fromPort": "out", "toNode": 4, "toPort": "in"},
            {"fromNode": 1, "fromPort": "out", "toNode": 4, "toPort": "forces"},
            {"fromNode": 2, "fromPort": "step_index", "toNode": 4, "toPort": "speed"},
            {"fromNode": 4, "fromPort": "out", "toNode": 5, "toPort": "seed"},
            {"fromNode": 5, "fromPort": "out", "toNode": 6, "toPort": "in"},
            {"fromNode": 1, "fromPort": "out", "toNode": 6, "toPort": "forces"},
            {"fromNode": 5, "fromPort": "step_dt", "toNode": 6, "toPort": "speed"},
            {"fromNode": 6, "fromPort": "out", "toNode": 7, "toPort": "in"},
            {"fromNode": 1, "fromPort": "out", "toNode": 7, "toPort": "forces"},
            {"fromNode": 5, "fromPort": "step_index", "toNode": 7, "toPort": "speed"},
            {"fromNode": 7, "fromPort": "out", "toNode": 5, "toPort": "in"},
            {"fromNode": 5, "fromPort": "out", "toNode": 8, "toPort": "in"},
            {"fromNode": 1, "fromPort": "out", "toNode": 8, "toPort": "forces"},
            {"fromNode": 2, "fromPort": "step_dt", "toNode": 8, "toPort": "speed"},
            {"fromNode": 8, "fromPort": "out", "toNode": 2, "toPort": "in"},
            {"fromNode": 2, "fromPort": "out", "toNode": 9, "toPort": "particles"},
            {"fromNode": 9, "fromPort": "out", "toNode": 10, "toPort": "in"}
        ]
    }))
    .expect("nested proof def")
}

/// Mover speeds in dispatch order for one frame of the nest.
fn nested_speeds() -> Vec<f32> {
    let mut speeds = Vec::new();
    for o in 0..OUTER {
        speeds.extend([particle_step_dt(o), o as f32]);
        for i in 0..INNER {
            speeds.extend([particle_step_dt(i), i as f32]);
        }
        speeds.push(particle_step_dt(o));
    }
    speeds
}

/// The nest's movers unrolled into one chain with constant speeds, run as
/// the body of a one-iteration region so the result lands in the
/// boundary's persistent state like the nest's does.
fn unrolled_def() -> EffectGraphDef {
    let speeds = nested_speeds();
    let mut nodes = vec![
        serde_json::json!({"id": 0, "nodeId": "seed", "typeId": "test.particle_source",
             "params": {"max_capacity": {"type": "Int", "value": N}}}),
        serde_json::json!({"id": 1, "nodeId": "forces", "typeId": "test.force_source",
             "params": {"max_capacity": {"type": "Int", "value": N}}}),
        serde_json::json!({"id": 2, "nodeId": "once", "typeId": "test.particle_boundary",
             "params": {"iterations": {"type": "Int", "value": 1}}}),
        serde_json::json!({"id": 3, "nodeId": "sink", "typeId": "test.particle_sink"}),
        serde_json::json!({"id": 4, "nodeId": "output", "typeId": "system.final_output"}),
    ];
    let mut wires = vec![
        serde_json::json!({"fromNode": 0, "fromPort": "out", "toNode": 2, "toPort": "seed"}),
        serde_json::json!({"fromNode": 2, "fromPort": "out", "toNode": 3, "toPort": "particles"}),
        serde_json::json!({"fromNode": 3, "fromPort": "out", "toNode": 4, "toPort": "in"}),
    ];
    let mut previous = 2u32;
    for (k, speed) in speeds.iter().enumerate() {
        let id = 10 + k as u32;
        nodes.push(serde_json::json!({"id": id, "nodeId": format!("mover_{k}"),
            "typeId": "node.move_particles_3d",
            "params": {"speed": {"type": "Float", "value": speed}}}));
        wires.push(serde_json::json!({"fromNode": previous, "fromPort": "out", "toNode": id, "toPort": "in"}));
        wires.push(serde_json::json!({"fromNode": 1, "fromPort": "out", "toNode": id, "toPort": "forces"}));
        previous = id;
    }
    wires.push(serde_json::json!({"fromNode": previous, "fromPort": "out", "toNode": 2, "toPort": "in"}));
    serde_json::from_value(serde_json::json!({"version": 3, "nodes": nodes, "wires": wires}))
        .expect("unrolled proof def")
}

/// Run `frames` frames and read back the persistent state of the one node of
/// type `boundary_type`.
fn run_state(graph: Graph, frames: u32, boundary_type: &str) -> Vec<Particle> {
    run_nest(graph, frames, boundary_type, Storage::Planned).state
}

/// Where arrays live: where the planner puts them, or each in its own
/// buffer, pre-bound before planning so the planner reuses nothing.
#[derive(Clone, Copy, PartialEq)]
enum Storage {
    Planned,
    Dedicated,
}

struct NestRun {
    /// The boundary's persistent state.
    state: Vec<Particle>,
    /// What the sink read.
    sink: Vec<Particle>,
    /// Distinct buffers behind every array.
    buffers: usize,
}

fn run_nest(mut graph: Graph, frames: u32, boundary_type: &str, storage: Storage) -> NestRun {
    let harness = harness::shared();
    let device = &harness.device;
    let plan = compile(&graph).expect("proof def compiles");
    let mut backend = MetalBackend::new(device.clone(), 64, 64, GpuTextureFormat::Rgba16Float);
    if storage == Storage::Dedicated {
        let planned = plan_array_allocations(&graph, &plan, (64, 64), &Default::default()).expect("plan allocates");
        for (&resource, array) in &planned.storage {
            backend.pre_bind_array(resource, device.create_buffer_shared(array.bytes));
        }
    }
    pre_allocate_resources(&graph, &plan, device, &mut backend).expect("pre-allocate");
    let buffers = (0..plan.resource_count() as u32)
        .filter_map(|r| backend.slot_for(ResourceId(r)).filter(|_| matches!(plan.resource_type(ResourceId(r)), Some(PortType::Array(_)))))
        .collect::<std::collections::HashSet<_>>()
        .len();
    let sink_res = resource(&plan, node_of(&graph, "test.particle_sink"), "particles", false);
    let seed_res = resource(&plan, node_of(&graph, "test.particle_source"), "out", true);
    let force_res = resource(&plan, node_of(&graph, "test.force_source"), "out", true);
    let state_res = resource(&plan, node_of(&graph, boundary_type), "out", true);
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
    for frame in 0..frames {
        let time = FrameTime {
            beats: Beats(0.0),
            seconds: Seconds(f64::from(frame) / 60.0),
            delta: Seconds(1.0 / 60.0),
            frame_count: i64::from(frame),
        };
        let mut enc = device.create_encoder("nested-substeps-proof");
        {
            let mut gpu = GpuEncoder::new(&mut enc, device);
            exec.execute_frame_with_state(&mut graph, &plan, time, &mut gpu, &mut state, 0);
        }
        enc.commit_and_wait_completed();
    }
    let backend = exec.backend();
    let read = |res: ResourceId| {
        let buffer = backend.array_buffer(backend.slot_for(res).expect("array bound")).expect("array buffer");
        let ptr = buffer.mapped_ptr().expect("shared array buffer");
        // SAFETY: the encoder completed; the buffer holds at least N particles.
        unsafe { std::slice::from_raw_parts(ptr.cast::<Particle>().cast_const(), N).to_vec() }
    };
    NestRun { state: read(state_res), sink: read(sink_res), buffers }
}

/// CPU reference for `frames` frames of the nest.
fn nested_expected(frames: u32) -> Vec<Particle> {
    let dt_scaled = (1.0f64 / 60.0) as f32 * 60.0;
    let forces = forces();
    let mut particles = seed_particles();
    for _ in 0..frames {
        for speed in nested_speeds() {
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
    particles
}

fn max_position_error(got: &[Particle], want: &[Particle]) -> f32 {
    got.iter()
        .zip(want)
        .flat_map(|(g, w)| (0..3).map(move |k| (g.position[k] - w.position[k]).abs()))
        .fold(0.0, f32::max)
}

#[test]
fn nested_region_matches_unrolled() {
    let registry = registry();
    let nested_graph = nested_def().into_graph(&registry, &Default::default()).expect("nest builds");
    let plan = compile(&nested_graph).expect("nest compiles");
    let regions = plan.substep_regions();
    assert_eq!(regions.len(), 1, "one top-level region");
    assert_eq!(regions[0].inner.len(), 1, "one inner region");
    let nested = run_state(nested_graph, 1, "test.particle_boundary");
    let unrolled = run_state(
        unrolled_def().into_graph(&registry, &Default::default()).expect("unrolled builds"),
        1,
        "test.particle_boundary",
    );
    let nested_bytes: &[u8] = bytemuck::cast_slice(&nested);
    let unrolled_bytes: &[u8] = bytemuck::cast_slice(&unrolled);
    assert!(nested_bytes == unrolled_bytes, "the nest diverged from the same dispatches unrolled");
    let err = max_position_error(&nested, &nested_expected(1));
    assert!(err <= 1.0e-5, "GPU vs CPU max |Δposition| = {err}");
    // The nest moved the particles: a stale inner restart or a skipped level
    // would leave far less motion than the reference.
    let seed = seed_particles();
    let moved = nested.iter().zip(&seed).filter(|(g, s)| s.life > 0.0 && (g.position[0] - s.position[0]).abs() > 1.0e-3).count();
    assert!(moved > N / 2, "only {moved} particles moved");
}

#[test]
fn nested_region_frozen_unfrozen_match() {
    let registry = registry();
    let canonical = nested_def();
    let view = fuse_generator_view(&canonical, &registry)
        .expect("each level's movers fuse and the fused def builds");
    let count = |type_id: &str| view.def.nodes.iter().filter(|n| n.type_id == type_id).count();
    assert_eq!(count("node.wgsl_compute"), 2, "one kernel per level");
    assert_eq!(count("node.move_particles_3d"), 1, "`post` stays alone");
    let frozen_graph = (*view.def).clone().into_graph(&registry, &view.mesh_rules).expect("fused def builds");
    let plan = compile(&frozen_graph).expect("fused nest compiles");
    assert_eq!(plan.substep_regions()[0].inner.len(), 1, "the fused def keeps the nest");
    let unfused = run_state(
        canonical.into_graph(&registry, &Default::default()).expect("builds"),
        FRAMES,
        "test.particle_boundary",
    );
    let frozen = run_state(frozen_graph, FRAMES, "test.particle_boundary");
    let unfused_bytes: &[u8] = bytemuck::cast_slice(&unfused);
    let frozen_bytes: &[u8] = bytemuck::cast_slice(&frozen);
    assert!(unfused_bytes == frozen_bytes, "fused nest diverged from unfused (buffer regions are bit-exact)");
    let err = max_position_error(&frozen, &nested_expected(FRAMES));
    assert!(err <= 1.0e-5, "GPU vs CPU max |Δposition| = {err}");
}

/// [`nested_def`] with three-copy chains of same-size temporaries before the
/// nest (`seed` into `outer.seed`), in the outer body (`outer_b` into
/// `inner.seed`) and after it (`outer.out` into the sink). A copy changes no
/// value, so [`nested_expected`] still holds.
fn nested_copy_chains_def() -> EffectGraphDef {
    let mut def = serde_json::to_value(nested_def()).expect("nest serialises");
    let mut chain = |ids: [u64; 3], from: u64, to: (u64, &str)| {
        let mut previous = from;
        for id in ids {
            def["nodes"].as_array_mut().expect("nodes").push(
                serde_json::json!({"id": id, "nodeId": format!("copy_{id}"), "typeId": "test.particle_copy"}),
            );
            def["wires"].as_array_mut().expect("wires").push(
                serde_json::json!({"fromNode": previous, "fromPort": "out", "toNode": id, "toPort": "in"}),
            );
            previous = id;
        }
        let wires = def["wires"].as_array_mut().expect("wires");
        wires.retain(|w| !(w["toNode"] == to.0 && w["toPort"] == to.1));
        wires.push(serde_json::json!({"fromNode": previous, "fromPort": "out", "toNode": to.0, "toPort": to.1}));
    };
    chain([20, 21, 22], 0, (2, "seed"));
    chain([25, 26, 27], 4, (5, "seed"));
    chain([30, 31, 32], 2, (9, "particles"));
    serde_json::from_value(def).expect("nest with copy chains")
}

/// Temporary reuse before, inside and after a nest changes no bit: the
/// planner's shared storage against every array in its own buffer.
#[test]
fn nested_region_shared_storage_matches_dedicated() {
    let registry = registry();
    let build = || nested_copy_chains_def().into_graph(&registry, &Default::default()).expect("nest with chains builds");
    let shared = run_nest(build(), FRAMES, "test.particle_boundary", Storage::Planned);
    let dedicated = run_nest(build(), FRAMES, "test.particle_boundary", Storage::Dedicated);
    eprintln!("nested chains: {} buffers shared, {} dedicated", shared.buffers, dedicated.buffers);
    assert!(shared.buffers + 3 <= dedicated.buffers, "the chains reuse storage");
    for (what, a, b) in [("state", &shared.state, &dedicated.state), ("sink", &shared.sink, &dedicated.sink)] {
        let (a, b): (&[u8], &[u8]) = (bytemuck::cast_slice(a), bytemuck::cast_slice(b));
        assert!(a == b, "shared storage changed the {what}");
    }
    let err = max_position_error(&shared.state, &nested_expected(FRAMES));
    assert!(err <= 1.0e-5, "GPU vs CPU max |Δposition| = {err}");
}
