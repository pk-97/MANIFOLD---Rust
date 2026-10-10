//! Shared CPU execution and mesh helpers for fluid-role contract tests.

use crate::primitives::fluid_role_source::FluidRoleSource;
use manifold_node_engine::exec::backend::{Backend, MockBackend};
use manifold_node_engine::exec::cpu_values::CpuWireWrites;
use manifold_node_engine::bindings::{NodeInputs, NodeOutputs, Slot};
use manifold_node_engine::exec::effect_node::{EffectNodeContext, FrameTime, ParamValues};
use manifold_node_engine::exec::execution_plan::ResourceId;
use manifold_node_engine::ports::PortType;
use manifold_node_engine::primitive::Primitive;
use crate::fluid_role::FluidRole;
use manifold_node_engine::scene::transform::Transform;
use manifold_node_engine::mesh::MeshVertex;
use manifold_core::{Beats, Seconds};

fn frame_time() -> FrameTime {
    FrameTime {
        beats: Beats(0.0),
        seconds: Seconds(0.0),
        delta: Seconds(1.0 / 60.0),
        frame_count: 0,
    }
}

pub(crate) fn run_inputs(
    primitive: &mut FluidRoleSource,
    backend: &mut MockBackend,
    input_bindings: &[(&'static str, Slot)],
    output_slot: Slot,
    params: &ParamValues,
) -> bool {
    run_inputs_under(primitive, backend, input_bindings, output_slot, params, Default::default())
}

pub(crate) fn run_inputs_under(
    primitive: &mut FluidRoleSource,
    backend: &mut MockBackend,
    input_bindings: &[(&'static str, Slot)],
    output_slot: Slot,
    params: &ParamValues,
    step: manifold_node_engine::exec::effect_node::SimStep,
) -> bool {
    let output_bindings: &[(&'static str, Slot)] = &[("role", output_slot)];
    let mut scalar_scratch = Vec::new();
    let mut camera_scratch = Vec::new();
    let mut light_scratch = Vec::new();
    let mut material_scratch = Vec::new();
    let mut transform_scratch = Vec::new();
    let mut atmosphere_scratch = Vec::new();
    let mut render_mode_scratch = Vec::new();
    let mut object_scratch = Vec::new();
    let mut role_scratch = CpuWireWrites::default();
    let inputs = NodeInputs::new(input_bindings, backend, &[]);
    let outputs = NodeOutputs::new(
        output_bindings,
        backend,
        &mut scalar_scratch,
        &mut camera_scratch,
        &mut light_scratch,
        &mut material_scratch,
        &mut transform_scratch,
        &mut atmosphere_scratch,
        &mut render_mode_scratch,
        &mut object_scratch,
    )
    .with_cpu_value_writes(&mut role_scratch);
    let pending = {
        let mut ctx = EffectNodeContext::new(frame_time(), params, inputs, outputs, None).with_sim_step(step);
        primitive.run(&mut ctx);
        ctx.outputs_pending
    };
    role_scratch.commit(backend.cpu_values_mut());
    pending
}

pub fn test_slots(backend: &mut MockBackend) -> (Slot, Slot) {
    let transform_slot = backend.acquire(ResourceId(0), PortType::Transform, None, (0, 0));
    let output_slot = backend.acquire(ResourceId(1), PortType::FluidRole, None, (0, 0));
    backend.set_transform(transform_slot, Transform::default());
    (transform_slot, output_slot)
}

pub fn settle_inputs(
    primitive: &mut FluidRoleSource,
    backend: &mut MockBackend,
    inputs: &[(&'static str, Slot)],
    output_slot: Slot,
    params: &ParamValues,
) -> FluidRole {
    for _ in 0..200 {
        let pending = run_inputs(primitive, backend, inputs, output_slot, params);
        if !pending && let Some(role) = backend.cpu_values().get::<FluidRole>(output_slot) {
            return role;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    panic!("fluid role source preparation did not settle");
}

pub fn cube_triangle_list() -> Vec<MeshVertex> {
    let points = [
        [-0.5, -0.5, -0.5],
        [0.5, -0.5, -0.5],
        [0.5, 0.5, -0.5],
        [-0.5, 0.5, -0.5],
        [-0.5, -0.5, 0.5],
        [0.5, -0.5, 0.5],
        [0.5, 0.5, 0.5],
        [-0.5, 0.5, 0.5],
    ];
    let faces = [
        [3, 2, 1, 0],
        [4, 0, 1, 5],
        [4, 7, 3, 0],
        [5, 1, 2, 6],
        [6, 2, 3, 7],
        [7, 4, 5, 6],
    ];
    faces
        .into_iter()
        .flat_map(|[a, b, c, d]| [a, b, c, a, c, d])
        .map(|index| MeshVertex {
            position: points[index],
            ..bytemuck::Zeroable::zeroed()
        })
        .collect()
}
