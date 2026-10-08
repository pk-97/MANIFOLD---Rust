//! Native GPU regression for growing FluidSurface geometry through a deformer.

use bytemuck::pod_read_unaligned;
use manifold_core::{Beats, Seconds};
use manifold_gpu::GpuTextureFormat;
use manifold_node_engine::runtime::frame_status::FrameRenderStatus;
use manifold_node_engine::mesh::MeshVertex;
use manifold_node_engine::gpu::gpu_encoder::GpuEncoder;
use manifold_node_engine::water::physics::PhysicsStepScope;
use manifold_renderer::node_graph::primitives::WaveShearMesh;
use manifold_node_engine::{exec::execution_plan::ExecutionPlan, exec::execution::Executor, exec::effect_node::FrameTime, graph::Graph, exec::metal_backend::MetalBackend, exec::effect_node::NodeInstanceId, parameters::ParamValue, persistence::PrimitiveRegistry, exec::execution_plan::ResourceId, exec::execution_plan::compile, load::graph_loader::pre_allocate_resources};

use crate::harness;

struct Runtime {
    graph: Graph,
    plan: ExecutionPlan,
    executor: Executor,
    output: ResourceId,
    vertex_count: ResourceId,
}

struct Snapshot {
    active_vertices: u32,
    capacity: usize,
    bytes: Vec<u8>,
}

fn resource_for(plan: &ExecutionPlan, node: NodeInstanceId, port: &str) -> ResourceId {
    plan.steps()
        .iter()
        .find(|step| step.node == node)
        .and_then(|step| {
            step.outputs
                .iter()
                .find(|(name, _)| *name == port)
                .map(|(_, resource)| *resource)
        })
        .unwrap_or_else(|| panic!("missing output {node:?}.{port}"))
}

fn make_runtime(harness: &harness::ParityHarness, max_capacity: f32) -> Runtime {
    let registry = PrimitiveRegistry::with_cpu_flip_reference();
    let mut graph = Graph::new();
    let fluid = graph.add_node(
        registry
            .construct(manifold_core::liquid_domain::FLIP_DOMAIN_TYPE_ID)
            .expect("FluidSurface primitive"),
    );
    let wave = graph.add_node(Box::new(WaveShearMesh::new()));
    graph.connect((fluid, "vertices"), (wave, "in")).unwrap();
    graph.add_external_output(wave, "out").unwrap();
    graph.add_external_output(fluid, "vertex_count").unwrap();
    for (name, value) in [
        ("seed", ParamValue::Float(4242.0)),
        ("resolution", ParamValue::Float(12.0)),
        ("fill_height", ParamValue::Float(0.8)),
        ("max_capacity", ParamValue::Float(max_capacity)),
    ] {
        graph.set_param(fluid, name, value).unwrap();
    }
    graph
        .set_param(wave, "amplitude", ParamValue::Float(0.25))
        .unwrap();
    graph
        .set_param(wave, "frequency", ParamValue::Float(1.5))
        .unwrap();

    let plan = compile(&graph).unwrap();
    let output = resource_for(&plan, wave, "out");
    let vertex_count = resource_for(&plan, fluid, "vertex_count");
    let mut backend = MetalBackend::new(
        std::sync::Arc::clone(&harness.device),
        harness.width,
        harness.height,
        GpuTextureFormat::Rgba16Float,
    );
    pre_allocate_resources(&mut graph, &plan, &harness.device, &mut backend).unwrap();

    Runtime {
        graph,
        plan,
        executor: Executor::new(Box::new(backend)),
        output,
        vertex_count,
    }
}

fn execute_frame(
    runtime: &mut Runtime,
    device: &manifold_gpu::GpuDevice,
    seconds: f64,
    frame_count: i64,
) -> Snapshot {
    let mut native_encoder = device.create_encoder("fluid-array-growth-proof");
    let status;
    {
        let mut gpu = GpuEncoder::new(&mut native_encoder, device);
        runtime.executor.execute_frame_with_gpu(
            &mut runtime.graph,
            &runtime.plan,
            FrameTime {
                beats: Beats(seconds * 2.0),
                seconds: Seconds(seconds),
                delta: Seconds(1.0 / 60.0),
                frame_count,
            },
            &mut gpu,
        );
        status = gpu.frame_status();
    }
    assert_eq!(status, FrameRenderStatus::Complete);
    native_encoder.commit_and_wait_completed();

    let buffer = runtime
        .executor
        .host_array_buffer(&runtime.graph, &runtime.plan, runtime.output)
        .expect("downstream output holds its own contents");
    let backend = runtime.executor.backend();
    let bytes = unsafe {
        std::slice::from_raw_parts(
            buffer.mapped_ptr().expect("shared downstream output") as *const u8,
            buffer.size() as usize,
        )
    };
    let count_slot = backend
        .slot_for(runtime.vertex_count)
        .expect("vertex count slot");
    let active_vertices = match backend.scalar(count_slot) {
        Some(ParamValue::Float(value)) if value.is_finite() && value >= 0.0 => value.round() as u32,
        other => panic!("invalid published vertex count: {other:?}"),
    };
    let active_bytes = active_vertices as usize * std::mem::size_of::<MeshVertex>();
    assert!(
        active_bytes <= bytes.len(),
        "active geometry exceeds output buffer"
    );
    Snapshot {
        active_vertices,
        capacity: bytes.len() / std::mem::size_of::<MeshVertex>(),
        bytes: bytes[..active_bytes].to_vec(),
    }
}

fn assert_finite_nonzero_geometry(snapshot: &Snapshot) {
    assert!(
        snapshot.active_vertices > 3,
        "native mesh did not outgrow capacity 3"
    );
    assert!(
        snapshot.capacity > 3,
        "downstream buffer did not grow beyond 3 vertices"
    );
    let vertices: Vec<MeshVertex> = snapshot
        .bytes
        .chunks_exact(std::mem::size_of::<MeshVertex>())
        .map(pod_read_unaligned)
        .collect();
    assert!(vertices.iter().all(|vertex| {
        vertex
            .position
            .iter()
            .chain(vertex.normal.iter())
            .all(|value| value.is_finite())
    }));
    assert!(
        vertices
            .iter()
            .any(|vertex| vertex.position.iter().any(|value| value.abs() > 1.0e-6))
    );
}

#[test]
fn fluid_mesh_growth_reaches_downstream_deformer_without_truncation() {
    let harness = harness::shared();
    let _render = PhysicsStepScope::for_render(true);
    let mut compact = make_runtime(harness, 3.0);
    let mut roomy = make_runtime(harness, 100_000.0);
    let mut compact_frames = Vec::new();

    for (frame_count, seconds) in [(0, 0.0), (1, 1.0 / 60.0), (2, 2.0 / 60.0)] {
        let compact_snapshot = execute_frame(&mut compact, &harness.device, seconds, frame_count);
        let roomy_snapshot = execute_frame(&mut roomy, &harness.device, seconds, frame_count);
        if frame_count > 0 {
            assert_finite_nonzero_geometry(&compact_snapshot);
            assert_finite_nonzero_geometry(&roomy_snapshot);
        } else {
            // Tick zero prepares the native world; meshing follows its first step.
            assert_eq!(compact_snapshot.active_vertices, 0);
        }
        assert_eq!(
            compact_snapshot.active_vertices,
            roomy_snapshot.active_vertices
        );
        assert_eq!(compact_snapshot.bytes, roomy_snapshot.bytes);
        compact_frames.push(compact_snapshot);
    }

    let compact_repeat = execute_frame(&mut compact, &harness.device, 2.0 / 60.0, 2);
    let roomy_repeat = execute_frame(&mut roomy, &harness.device, 2.0 / 60.0, 2);
    assert_eq!(
        compact_repeat.active_vertices,
        compact_frames[2].active_vertices
    );
    assert_eq!(compact_repeat.bytes, compact_frames[2].bytes);
    assert_eq!(roomy_repeat.active_vertices, compact_repeat.active_vertices);
    assert_eq!(roomy_repeat.bytes, compact_repeat.bytes);
}
