//! `node.prolong_lattice` — a coarse lattice interpolated up to one twice as
//! long on every axis and added on, the multigrid pressure solve's coarse
//! correction (docs/GPU_FLIP_PRESSURE_SOLVE.md). A per-element gather on the
//! codegen path.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use super::cells_with_particles::{cell_count, cell_lattice};
use super::sort_particles_into_cells::float_param;
use super::standalone_pipeline::standalone_pipeline;
use crate::node_graph::effect_node::{EffectNodeContext, ParamValues};
use crate::node_graph::freeze::classify::FusedOutputCapacity;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Codegen uniform layout: params in PARAMS order, then `dispatch_count`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ProlongUniforms {
    nodes_x: f32,
    nodes_y: f32,
    nodes_z: f32,
    dispatch_count: u32,
}

/// A fine lattice that halves: every length even, 2 to 1024.
pub(crate) fn even_lattice(params: &ParamValues) -> Option<[u32; 3]> {
    cell_lattice(params).filter(|nodes| nodes.iter().all(|&n| n >= 2 && n % 2 == 0))
}

crate::primitive! {
    name: ProlongLattice,
    type_id: "node.prolong_lattice",
    purpose: "Add a coarse lattice, interpolated up, onto a fine one (nodes_x/y/z are the fine cells, each even; the coarse lattice has half as many per axis, cell (i, j, k) at i + nx·(j + ny·k)): out = value + Σ w · coarse in water cells (water > 0.5), value in air, where per axis a fine cell takes 3/4 from its parent coarse cell and 1/4 from the parent's neighbour on its side, clamped at the walls.",
    inputs: {
        value: Array(f32) required,
        coarse: Array(f32) required,
        water: Array(f32) required,
    },
    outputs: {
        out: Array(f32),
    },
    params: [
        float_param!("nodes_x", "Cells X", 64.0, 2.0, 1024.0),
        float_param!("nodes_y", "Cells Y", 64.0, 2.0, 1024.0),
        float_param!("nodes_z", "Cells Z", 64.0, 2.0, 1024.0),
    ],
    depth_rule: Terminal,
    composition_notes: "The multigrid V-cycle's way up: value is the fine level after its pre-smoothing, coarse the correction the coarse level solved for, water the fine level's water; the post-smoothing node.pressure_smooth sweeps follow.",
    examples: [],
    picker: { label: "Prolong Lattice", category: Atom },
    summary: "Grows a grid of values to double size, blending neighbours, and adds it on.",
    category: Particles3D,
    role: Filter,
    aliases: ["prolong", "interpolate", "upsample volume", "coarse correction", "multigrid prolong"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/prolong_lattice_body.wgsl"),
    input_access: [Coincident, BufferGather, Coincident],
    output_capacity: FusedOutputCapacity::FromInput { input: "value" },
}

impl Primitive for ProlongLattice {
    fn array_output_capacity(&self, port: &str, _params: &ParamValues, inputs: &[(&str, u32)]) -> Option<u32> {
        (port == "out").then(|| inputs.iter().find(|(name, _)| *name == "value").map(|&(_, n)| n)).flatten()
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(nodes) = even_lattice(ctx.params) else {
            ctx.error("Prolong Lattice: every lattice length must be even, 2 to 1024".to_string());
            return;
        };
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let (Some(value), Some(coarse), Some(water), Some(out)) =
            (ctx.inputs.array("value"), ctx.inputs.array("coarse"), ctx.inputs.array("water"), ctx.outputs.array("out"))
        else {
            return;
        };
        let cells = cell_count(nodes);
        if cells * 4 > value.size.min(water.size).min(out.size) || cells / 2 > coarse.size {
            ctx.error(format!("Prolong Lattice: a {nodes:?} lattice is larger than its arrays"));
            return;
        }
        let uniforms = ProlongUniforms {
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
                GpuBinding::Buffer { binding: 1, buffer: value, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: coarse, offset: 0 },
                GpuBinding::Buffer { binding: 3, buffer: water, offset: 0 },
                GpuBinding::Buffer { binding: 4, buffer: out, offset: 0 },
            ],
            [(cells as u32).div_ceil(256), 1, 1],
            "node.prolong_lattice",
        );
    }
}
