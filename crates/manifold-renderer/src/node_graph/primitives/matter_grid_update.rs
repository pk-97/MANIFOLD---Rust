//! `node.matter_grid_update` — resolve the matter grid: momentum to velocity,
//! gravity, closed walls and the CFL clamp (`docs/GPU_MPM_SOLVER_DESIGN.md`
//! section 4.1 step 4). Colliders join in P2a, forces and impulses in P3c.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::matter::MatterGridNode;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;
use super::matter_common::read_lattice;
use super::standalone_pipeline::standalone_pipeline;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GridUpdateUniforms {
    nodes_x: i32,
    nodes_y: i32,
    nodes_z: i32,
    cell_size: f32,
    step_dt: f32,
    gravity_x: f32,
    gravity: f32,
    gravity_z: f32,
    closed_faces: i32,
    dispatch_count: u32,
    _pad0: u32,
    _pad1: u32,
}

crate::primitive! {
    name: MatterGridUpdate,
    type_id: "node.matter_grid_update",
    purpose: "Resolve the matter grid: turn each node's accumulated momentum into velocity, add gravity, stop velocity into closed domain walls on the outer three nodes, and clamp each component to 0.9 cells per substep. Keeps the pre-force velocity for the Liveliness blend.",
    inputs: {
        accum: Array(i32) required,
        grid: Array(MatterGridNode) required,
        nodes_x: ScalarF32 optional, nodes_y: ScalarF32 optional, nodes_z: ScalarF32 optional,
        cell_size: ScalarF32 optional,
        step_dt: ScalarF32 optional,
        gravity_x: ScalarF32 optional, gravity: ScalarF32 optional, gravity_z: ScalarF32 optional,
        closed_faces: ScalarF32 optional,
    },
    outputs: {
        grid_out: Array(MatterGridNode),
    },
    params: [
        ParamDef { name: Cow::Borrowed("nodes_x"), label: "Nodes X", ty: ParamType::Int, default: ParamValue::Float(71.0), range: Some((1.0, 4096.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("nodes_y"), label: "Nodes Y", ty: ParamType::Int, default: ParamValue::Float(71.0), range: Some((1.0, 4096.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("nodes_z"), label: "Nodes Z", ty: ParamType::Int, default: ParamValue::Float(71.0), range: Some((1.0, 4096.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("cell_size"), label: "Cell Size", ty: ParamType::Float, default: ParamValue::Float(0.0625), range: Some((1.0e-4, 100.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("step_dt"), label: "Substep (s)", ty: ParamType::Float, default: ParamValue::Float(4.9e-4), range: Some((0.0, 1.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("gravity_x"), label: "Gravity X", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((-20.0, 20.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("gravity"), label: "Gravity Y", ty: ParamType::Float, default: ParamValue::Float(-9.81), range: Some((-20.0, 20.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("gravity_z"), label: "Gravity Z", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((-20.0, 20.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("closed_faces"), label: "Closed Faces (bits −X +X −Y +Y −Z +Z)", ty: ParamType::Int, default: ParamValue::Float(63.0), range: Some((0.0, 63.0)), enum_values: &[] },
    ],
    depth_rule: Terminal,
    composition_notes: "Region body of the Live Matter group, between node.matter_to_grid and node.grid_to_matter. grid/grid_out alias the node.matter_state grid array (one MatterGridNode per lattice node, x fastest); accum is read as a gather (4 words per node). Gravity and closed faces come from node.matter_domain, step_dt from the substep boundary.",
    examples: ["WaterDamBreakMatter", "WaterStillPoolMatter"],
    picker: { label: "Matter Grid Update", category: Atom },
    summary: "Turns the grid's gathered liquid momentum into velocities, adds gravity and stops the liquid at the walls.",
    category: Particles3D,
    role: Filter,
    aliases: ["grid update", "mpm grid", "resolve grid"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/matter_grid_update_body.wgsl"),
    input_access: [BufferGather, Coincident],
}

impl Primitive for MatterGridUpdate {
    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &crate::node_graph::effect_node::ParamValues,
        input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        (port_name == "grid_out")
            .then(|| input_capacities.iter().find(|(p, _)| *p == "grid").map(|&(_, n)| n))
            .flatten()
    }

    fn aliased_array_io(&self) -> &'static [(&'static str, &'static str)] {
        &[("grid", "grid_out")]
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let lattice = read_lattice(ctx);
        let step_dt = ctx.scalar_or_param("step_dt", 4.9e-4);
        let gravity = [
            ctx.scalar_or_param("gravity_x", 0.0),
            ctx.scalar_or_param("gravity", -9.81),
            ctx.scalar_or_param("gravity_z", 0.0),
        ];
        let closed_faces = ctx.scalar_or_param("closed_faces", 63.0).round().clamp(0.0, 63.0) as i32;
        // In place on the grid input; the GPU is touched on every path.
        let accum = ctx.inputs.array("accum");
        let grid = ctx.inputs.array("grid");
        let gpu = ctx.gpu_encoder();
        let (Some(accum), Some(grid)) = (accum, grid) else {
            return;
        };
        let nodes = lattice
            .node_count()
            .min((grid.size / std::mem::size_of::<MatterGridNode>() as u64) as u32)
            .min((accum.size / 16) as u32);
        if nodes == 0 || step_dt <= 0.0 {
            return;
        }
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let uniforms = GridUpdateUniforms {
            nodes_x: lattice.nodes[0] as i32,
            nodes_y: lattice.nodes[1] as i32,
            nodes_z: lattice.nodes[2] as i32,
            cell_size: lattice.cell_size,
            step_dt,
            gravity_x: gravity[0],
            gravity: gravity[1],
            gravity_z: gravity[2],
            closed_faces,
            dispatch_count: nodes,
            _pad0: 0,
            _pad1: 0,
        };
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                GpuBinding::Buffer { binding: 1, buffer: accum, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: grid, offset: 0 },
                GpuBinding::Buffer { binding: 3, buffer: grid, offset: 0 },
            ],
            [nodes.div_ceil(256), 1, 1],
            "node.matter_grid_update",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matter_grid_update_generates_a_gathering_node_kernel() {
        let wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<MatterGridUpdate>()
            .expect("matter_grid_update codegen");
        assert!(wgsl.contains("var<storage, read> buf_accum: array<i32>"), "{wgsl}");
        assert!(wgsl.contains("buf_grid_out[idx] = body(idx, params.dispatch_count, e_grid,"), "{wgsl}");
        assert_eq!(std::mem::size_of::<GridUpdateUniforms>(), 48);
        let body = include_str!("shaders/matter_grid_update_body.wgsl");
        assert!(body.contains("1048576.0") && body.contains("0.9 * vel_unit"));
        assert_eq!(crate::node_graph::matter::VELOCITY_CLAMP_CFL, 0.9);
    }
}
