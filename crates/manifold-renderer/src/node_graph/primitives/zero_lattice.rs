//! `node.zero_lattice` — a lattice of f32 zeros, the starting value of the
//! multigrid pressure solve's smoothing on every level
//! (docs/GPU_FLIP_PRESSURE_SOLVE.md). A source on the codegen path.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use super::cells_with_particles::{LATTICE_PARAMS, cell_capacity, cell_count, cell_lattice};
use super::sort_particles_into_cells::float_param;
use super::standalone_pipeline::standalone_pipeline;
use crate::node_graph::effect_node::{EffectNodeContext, ParamValues};
use crate::node_graph::freeze::classify::FusedOutputCapacity;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Codegen uniform layout: params in PARAMS order, then `dispatch_count`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ZeroUniforms {
    nodes_x: f32,
    nodes_y: f32,
    nodes_z: f32,
    dispatch_count: u32,
}

crate::primitive! {
    name: ZeroLattice,
    type_id: "node.zero_lattice",
    purpose: "Emit nodes_x · nodes_y · nodes_z f32 zeros: the zero start of a lattice solve.",
    inputs: {},
    outputs: {
        out: Array(f32),
    },
    params: [
        float_param!("nodes_x", "Cells X", 32.0, 1.0, 1024.0),
        float_param!("nodes_y", "Cells Y", 32.0, 1.0, 1024.0),
        float_param!("nodes_z", "Cells Z", 32.0, 1.0, 1024.0),
    ],
    depth_rule: Terminal,
    composition_notes: "One per multigrid level: the value node.pressure_smooth's first sweep starts from, and the zero rhs node.pressure_residual takes to give −L p.",
    examples: [],
    picker: { label: "Zero Lattice", category: Atom },
    summary: "A grid of zeros, the starting guess for the pressure solver.",
    category: MathAndConvert,
    role: Source,
    aliases: ["zeros", "zero field", "empty lattice"],
    pure: true,
    fusion_kind: Source,
    wgsl_body: include_str!("shaders/zero_lattice_body.wgsl"),
    output_capacity: FusedOutputCapacity::ParamProduct { params: &LATTICE_PARAMS, plus: 0 },
}

impl Primitive for ZeroLattice {
    fn array_output_capacity(&self, port: &str, params: &ParamValues, _inputs: &[(&str, u32)]) -> Option<u32> {
        (port == "out").then(|| cell_capacity(params)).flatten()
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(nodes) = cell_lattice(ctx.params) else {
            ctx.error("Zero Lattice: every lattice length must be 1 to 1024".to_string());
            return;
        };
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let Some(out) = ctx.outputs.array("out") else {
            return;
        };
        let cells = cell_count(nodes);
        if cells * 4 > out.size {
            ctx.error(format!("Zero Lattice: a {nodes:?} lattice is larger than its array"));
            return;
        }
        let uniforms = ZeroUniforms {
            nodes_x: nodes[0] as f32,
            nodes_y: nodes[1] as f32,
            nodes_z: nodes[2] as f32,
            dispatch_count: cells as u32,
        };
        let gpu = ctx.gpu_encoder();
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                GpuBinding::Buffer { binding: 1, buffer: out, offset: 0 },
            ],
            [(cells as u32).div_ceil(256), 1, 1],
            "node.zero_lattice",
        );
    }
}
