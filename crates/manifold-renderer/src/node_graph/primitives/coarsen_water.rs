//! `node.coarsen_water` — a water lattice half as long on every axis, for
//! the multigrid pressure solve's coarse levels (docs/GPU_FLIP_PRESSURE_SOLVE.md):
//! a coarse cell is water only when all eight cells it covers are, so the
//! free surface never moves into the water on a coarse level. A per-element
//! gather on the codegen path.

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
struct CoarsenUniforms {
    nodes_x: f32,
    nodes_y: f32,
    nodes_z: f32,
    dispatch_count: u32,
}

crate::primitive! {
    name: CoarsenWater,
    type_id: "node.coarsen_water",
    purpose: "Halve a water lattice on every axis (nodes_x/y/z are the coarse cells; the fine lattice has twice as many per axis, cell (i, j, k) at i + nx·(j + ny·k)): a coarse cell is 1 when all eight fine cells it covers are water (> 0.5), else 0.",
    inputs: {
        fine: Array(f32) required,
    },
    outputs: {
        out: Array(f32),
    },
    params: [
        float_param!("nodes_x", "Coarse Cells X", 32.0, 1.0, 512.0),
        float_param!("nodes_y", "Coarse Cells Y", 32.0, 1.0, 512.0),
        float_param!("nodes_z", "Coarse Cells Z", 32.0, 1.0, 512.0),
    ],
    depth_rule: Terminal,
    composition_notes: "Chained from node.cells_with_particles once per water step, one per multigrid level: every solve on that step's water (the pressure and the density correction) reads the same coarse levels.",
    examples: [],
    picker: { label: "Coarsen Water", category: Atom },
    summary: "Makes a half-size copy of which cells hold water, for the pressure solver's coarse levels.",
    category: Particles3D,
    role: Filter,
    aliases: ["coarse water", "water mask", "multigrid levels"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/coarsen_water_body.wgsl"),
    input_access: [BufferGather],
    output_capacity: FusedOutputCapacity::ParamProduct { params: &LATTICE_PARAMS, plus: 0 },
}

impl Primitive for CoarsenWater {
    fn array_output_capacity(&self, port: &str, params: &ParamValues, _inputs: &[(&str, u32)]) -> Option<u32> {
        (port == "out").then(|| cell_capacity(params)).flatten()
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(nodes) = cell_lattice(ctx.params).filter(|nodes| nodes.iter().all(|&n| n <= 512)) else {
            ctx.error("Coarsen Water: every coarse lattice length must be 1 to 512".to_string());
            return;
        };
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let (Some(fine), Some(out)) = (ctx.inputs.array("fine"), ctx.outputs.array("out")) else {
            return;
        };
        let cells = cell_count(nodes);
        if cells * 32 > fine.size || cells * 4 > out.size {
            ctx.error(format!("Coarsen Water: a {nodes:?} coarse lattice is larger than its arrays"));
            return;
        }
        let uniforms = CoarsenUniforms {
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
                GpuBinding::Buffer { binding: 1, buffer: fine, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: out, offset: 0 },
            ],
            [(cells as u32).div_ceil(256), 1, 1],
            "node.coarsen_water",
        );
    }
}
