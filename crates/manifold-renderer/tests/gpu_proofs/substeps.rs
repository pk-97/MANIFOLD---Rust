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
use manifold_renderer::particles::Particle;
use manifold_renderer::gpu_encoder::GpuEncoder;
use manifold_renderer::node_graph::freeze::install::fuse_generator_view;
use manifold_renderer::node_graph::ports::PortType;
use manifold_renderer::node_graph::resource_allocation::plan_array_allocations;
use manifold_renderer::testkit::substep_nodes::{
    particle_step_dt, register_substep_test_nodes,
};
use manifold_renderer::node_graph::{
    Backend, EffectGraphDefExt, ExecutionPlan, Executor, FrameTime, Graph, MetalBackend,
    NodeInstanceId, PrimitiveRegistry, ResourceId, StateStore, compile, pre_allocate_resources,
};

use crate::harness;

pub(crate) const N: usize = 1000;
const ITERATIONS: u32 = 4;
const FRAMES: u32 = 3;

pub(crate) fn def() -> EffectGraphDef {
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

pub(crate) fn registry() -> PrimitiveRegistry {
    let mut registry = PrimitiveRegistry::with_builtin();
    register_substep_test_nodes(&mut registry);
    registry
}

pub(crate) fn seed_particles() -> Vec<Particle> {
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

pub(crate) fn forces() -> Vec<[f32; 3]> {
    (0..N)
        .map(|i| [0.01 + i as f32 * 1.0e-5, -0.02, 0.003 * (i % 5) as f32])
        .collect()
}

pub(crate) fn node_of(graph: &Graph, type_id: &str) -> NodeInstanceId {
    graph
        .nodes()
        .find(|n| n.node.type_id().as_str() == type_id)
        .map(|n| n.id)
        .unwrap_or_else(|| panic!("graph has no `{type_id}`"))
}

pub(crate) fn resource(plan: &ExecutionPlan, node: NodeInstanceId, port: &str, output: bool) -> ResourceId {
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
    pre_allocate_resources(&mut graph, &plan, device, &mut backend).expect("pre-allocate");

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

    let out = exec.host_array_buffer(&graph, &plan, out_res).expect("state buffer");
    let captured = exec.host_array_buffer(&graph, &plan, in_res).expect("capture buffer");
    let ptr = out.mapped_ptr().expect("shared state buffer");
    // SAFETY: the encoder completed; the buffer holds at least N particles.
    let particles =
        unsafe { std::slice::from_raw_parts(ptr.cast::<Particle>().cast_const(), N).to_vec() };
    Run {
        particles,
        in_place: out.ptr_eq(captured),
    }
}

/// CPU reference for `frames` frames: per frame, per iteration, `mover_a`
/// then `mover_b`, each `position += force * speed * dt_scaled` in f32.
fn expected(frames: u32) -> Vec<Particle> {
    let dt_scaled = (1.0f64 / 60.0) as f32 * 60.0;
    let forces = forces();
    let mut particles = seed_particles();
    for _ in 0..frames {
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
    let want = expected(FRAMES);
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

/// Where arrays live: where the planner puts them, or each in its own
/// buffer, pre-bound before planning so the planner reuses nothing.
#[derive(Clone, Copy, PartialEq)]
enum Storage {
    Planned,
    Dedicated,
}

struct StorageRun {
    /// The boundary's persistent state.
    state: Vec<Particle>,
    /// What the sink read.
    sink: Vec<Particle>,
    /// Distinct buffers behind every array.
    buffers: usize,
}

fn run_with_storage(mut graph: Graph, frames: u32, storage: Storage) -> StorageRun {
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
    pre_allocate_resources(&mut graph, &plan, device, &mut backend).expect("pre-allocate");
    let buffers = (0..plan.resource_count() as u32)
        .filter_map(|r| backend.slot_for(ResourceId(r)).filter(|_| matches!(plan.resource_type(ResourceId(r)), Some(PortType::Array(_)))))
        .collect::<std::collections::HashSet<_>>()
        .len();
    let sink_res = resource(&plan, node_of(&graph, "test.particle_sink"), "particles", false);
    let seed_res = resource(&plan, node_of(&graph, "test.particle_source"), "out", true);
    let force_res = resource(&plan, node_of(&graph, "test.force_source"), "out", true);
    let state_res = resource(&plan, node_of(&graph, "test.particle_boundary"), "out", true);
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
        let mut enc = device.create_encoder("substeps-storage-proof");
        {
            let mut gpu = GpuEncoder::new(&mut enc, device);
            exec.execute_frame_with_state(&mut graph, &plan, time, &mut gpu, &mut state, 0);
        }
        enc.commit_and_wait_completed();
    }
    let read = |res: ResourceId| {
        let buffer = exec.host_array_buffer(&graph, &plan, res).expect("array holds its own contents");
        let ptr = buffer.mapped_ptr().expect("shared array buffer");
        // SAFETY: the encoder completed; the buffer holds at least N particles.
        unsafe { std::slice::from_raw_parts(ptr.cast::<Particle>().cast_const(), N).to_vec() }
    };
    StorageRun { state: read(state_res), sink: read(sink_res), buffers }
}

fn max_position_error(got: &[Particle], want: &[Particle]) -> f32 {
    got.iter()
        .zip(want)
        .flat_map(|(g, w)| (0..3).map(move |k| (g.position[k] - w.position[k]).abs()))
        .fold(0.0, f32::max)
}

/// [`def`] with three-copy chains of same-size temporaries before the region
/// (`seed` into `boundary.seed`), in its body (`mover_a` into `mover_b`) and
/// after it (`boundary.out` into the sink). A copy changes no value, so
/// [`expected`] still holds.
pub(crate) fn copy_chains_def() -> EffectGraphDef {
    let mut def = serde_json::to_value(def()).expect("def serialises");
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
    chain([25, 26, 27], 3, (4, "in"));
    chain([30, 31, 32], 2, (6, "particles"));
    serde_json::from_value(def).expect("def with copy chains")
}

/// Temporary reuse before, inside and after a region changes no bit: the
/// planner's shared storage against every array in its own buffer.
#[test]
fn region_shared_storage_matches_dedicated() {
    let registry = registry();
    let build = || copy_chains_def().into_graph(&registry, &Default::default()).expect("def with chains builds");
    let shared = run_with_storage(build(), FRAMES, Storage::Planned);
    let dedicated = run_with_storage(build(), FRAMES, Storage::Dedicated);
    eprintln!("region chains: {} buffers shared, {} dedicated", shared.buffers, dedicated.buffers);
    assert!(shared.buffers + 3 <= dedicated.buffers, "the chains reuse storage");
    for (what, a, b) in [("state", &shared.state, &dedicated.state), ("sink", &shared.sink, &dedicated.sink)] {
        let (a, b): (&[u8], &[u8]) = (bytemuck::cast_slice(a), bytemuck::cast_slice(b));
        assert!(a == b, "shared storage changed the {what}");
    }
    let err = max_position_error(&shared.state, &expected(FRAMES));
    assert!(err <= 1.0e-5, "GPU vs CPU max |Δposition| = {err}");
}

/// `seed → first → second → mover (in place) → third → sink`: `third` takes
/// the storage `first` released, so after the frame that storage holds the
/// moved particles.
fn reused_storage_def() -> EffectGraphDef {
    serde_json::from_value(serde_json::json!({
        "version": 3,
        "nodes": [
            {"id": 0, "nodeId": "seed", "typeId": "test.particle_source",
             "params": {"max_capacity": {"type": "Int", "value": N}}},
            {"id": 1, "nodeId": "forces", "typeId": "test.force_source",
             "params": {"max_capacity": {"type": "Int", "value": N}}},
            {"id": 2, "nodeId": "first", "typeId": "test.particle_copy"},
            {"id": 3, "nodeId": "second", "typeId": "test.particle_copy"},
            {"id": 4, "nodeId": "mover", "typeId": "node.move_particles_3d"},
            {"id": 5, "nodeId": "third", "typeId": "test.particle_copy"},
            {"id": 6, "nodeId": "sink", "typeId": "test.particle_sink"},
            {"id": 7, "nodeId": "output", "typeId": "system.final_output"}
        ],
        "wires": [
            {"fromNode": 0, "fromPort": "out", "toNode": 2, "toPort": "in"},
            {"fromNode": 2, "fromPort": "out", "toNode": 3, "toPort": "in"},
            {"fromNode": 3, "fromPort": "out", "toNode": 4, "toPort": "in"},
            {"fromNode": 1, "fromPort": "out", "toNode": 4, "toPort": "forces"},
            {"fromNode": 4, "fromPort": "out", "toNode": 5, "toPort": "in"},
            {"fromNode": 5, "fromPort": "out", "toNode": 6, "toPort": "particles"},
            {"fromNode": 6, "fromPort": "out", "toNode": 7, "toPort": "in"}
        ]
    }))
    .expect("reused storage def")
}

/// Arrays read after the frame read their own contents, not those of the
/// array that later took their storage: the whole-graph dump shows `first`
/// as the seed it copied, and the host reader refuses it.
#[test]
fn post_frame_readers_never_see_reused_storage() {
    let registry = registry();
    let mut graph = reused_storage_def().into_graph(&registry, &Default::default()).expect("builds");
    // The test fills both sources before the frame, so both keep their storage.
    graph.add_external_output(node_of(&graph, "test.particle_source"), "out").expect("seed output");
    graph.add_external_output(node_of(&graph, "test.force_source"), "out").expect("force output");
    let plan = compile(&graph).expect("compiles");
    let harness = harness::shared();
    let device = &harness.device;
    let mut backend = MetalBackend::new(device.clone(), 64, 64, GpuTextureFormat::Rgba16Float);
    pre_allocate_resources(&mut graph, &plan, device, &mut backend).expect("pre-allocate");
    let copies: Vec<ResourceId> = plan
        .steps()
        .iter()
        .filter(|s| graph.get_node(s.node).is_some_and(|n| n.node.type_id().as_str() == "test.particle_copy"))
        .map(|s| s.outputs[0].1)
        .collect();
    let [first, _, third] = copies[..] else { panic!("three copies, got {copies:?}") };
    assert_eq!(backend.slot_for(first), backend.slot_for(third), "premise: `third` takes `first`'s storage");
    let seed_res = resource(&plan, node_of(&graph, "test.particle_source"), "out", true);
    let force_res = resource(&plan, node_of(&graph, "test.force_source"), "out", true);
    for (res, bytes) in [
        (seed_res, bytemuck::cast_slice::<Particle, u8>(&seed_particles()).to_vec()),
        (force_res, bytemuck::cast_slice::<[f32; 3], u8>(&forces()).to_vec()),
    ] {
        let buffer = Backend::array_buffer(&backend, backend.slot_for(res).expect("bound")).expect("array");
        // SAFETY: shared-storage buffer, no GPU work in flight yet.
        unsafe { buffer.write(0, &bytes) };
    }

    let mut exec = Executor::new(Box::new(backend));
    exec.set_dump_all(true);
    let time = FrameTime { beats: Beats(0.0), seconds: Seconds(0.0), delta: Seconds(1.0 / 60.0), frame_count: 0 };
    let mut enc = device.create_encoder("reused-storage-proof");
    {
        let mut gpu = GpuEncoder::new(&mut enc, device);
        exec.execute_frame_with_state(&mut graph, &plan, time, &mut gpu, &mut StateStore::new(), 0);
    }
    enc.commit_and_wait_completed();

    let particles = |buffer: &manifold_gpu::GpuBuffer| {
        let ptr = buffer.mapped_ptr().expect("shared array buffer");
        // SAFETY: the encoder completed; the buffer holds at least N particles.
        unsafe { std::slice::from_raw_parts(ptr.cast::<Particle>().cast_const(), N).to_vec() }
    };
    let bits = |p: &[Particle]| bytemuck::cast_slice::<Particle, u8>(p).to_vec();
    let seed = seed_particles();
    let backend = exec.backend();
    let live_first = particles(backend.array_buffer(backend.slot_for(first).expect("bound")).expect("array"));
    let moved = particles(exec.host_array_buffer(&graph, &plan, third).expect("`third` is last in its storage"));
    let dt_scaled = (1.0f64 / 60.0) as f32 * 60.0;
    let want: Vec<Particle> = seed
        .iter()
        .zip(forces())
        .map(|(p, f)| {
            let mut p = *p;
            if p.life > 0.0 {
                for (position, force) in p.position.iter_mut().zip(f) {
                    *position += force * dt_scaled;
                }
            }
            p
        })
        .collect();
    assert!(max_position_error(&moved, &want) <= 1.0e-6, "the mover moved every live particle by its force");
    assert!(bits(&live_first) == bits(&moved), "premise: `first`'s storage ends holding `third`");
    assert!(exec.host_array_buffer(&graph, &plan, first).is_none(), "the host reader refuses `first`");

    let dumped = |res: ResourceId| {
        assert!(exec.dump_array_resources().iter().any(|&(_, _, r)| r == res), "{res:?} dumped");
        particles(exec.dump_array_buffer(res).expect("dump buffer"))
    };
    assert!(bits(&dumped(first)) == bits(&seed), "the dump shows `first` as the seed it copied");
    assert!(bits(&dumped(third)) == bits(&moved), "the dump shows `third` as written");
}
