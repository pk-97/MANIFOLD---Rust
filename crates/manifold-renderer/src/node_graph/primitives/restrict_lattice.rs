//! `node.restrict_lattice` — a fine lattice's values averaged down to a
//! lattice half as long on every axis, the multigrid pressure solve's
//! restriction (docs/GPU_FLIP_PRESSURE_SOLVE.md). A per-element gather on
//! the codegen path.

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
struct RestrictUniforms {
    nodes_x: f32,
    nodes_y: f32,
    nodes_z: f32,
    dispatch_count: u32,
}

crate::primitive! {
    name: RestrictLattice,
    type_id: "node.restrict_lattice",
    purpose: "Average a fine lattice down to a coarse one half as long on every axis (nodes_x/y/z are the coarse cells; the fine lattice has twice as many per axis, cell (i, j, k) at i + nx·(j + ny·k)): each coarse water cell (water > 0.5) takes Σ w · fine over the 4×4×4 fine cells around it, with w the product per axis of the trilinear share that fine cell takes from it (3/4 from its parent, 1/4 from the parent's neighbour, clamped at the walls), over 8. 0 in air. The transpose of node.prolong_lattice, over 8.",
    inputs: {
        fine: Array(f32) required,
        water: Array(f32) required,
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
    composition_notes: "The multigrid V-cycle's way down: node.pressure_residual on the fine level → restrict_lattice with water = node.coarsen_water's coarse water → the coarse level's smoothing, or on the coarsest level node.combine_rows with node.coarse_inverse.",
    examples: [],
    picker: { label: "Restrict Lattice", category: Atom },
    summary: "Shrinks a grid of values to half size, averaging each neighbourhood.",
    category: Particles3D,
    role: Filter,
    aliases: ["restrict", "downsample volume", "coarsen", "multigrid restrict"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/restrict_lattice_body.wgsl"),
    input_access: [BufferGather, Coincident],
    output_capacity: FusedOutputCapacity::FromInput { input: "water" },
}

impl Primitive for RestrictLattice {
    fn array_output_capacity(&self, port: &str, _params: &ParamValues, inputs: &[(&str, u32)]) -> Option<u32> {
        (port == "out").then(|| inputs.iter().find(|(name, _)| *name == "water").map(|&(_, n)| n)).flatten()
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(nodes) = cell_lattice(ctx.params).filter(|nodes| nodes.iter().all(|&n| n <= 512)) else {
            ctx.error("Restrict Lattice: every coarse lattice length must be 1 to 512".to_string());
            return;
        };
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let (Some(fine), Some(water), Some(out)) = (ctx.inputs.array("fine"), ctx.inputs.array("water"), ctx.outputs.array("out"))
        else {
            return;
        };
        let cells = cell_count(nodes);
        if cells * 32 > fine.size || cells * 4 > water.size.min(out.size) {
            ctx.error(format!("Restrict Lattice: a {nodes:?} coarse lattice is larger than its arrays"));
            return;
        }
        let uniforms = RestrictUniforms {
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
                GpuBinding::Buffer { binding: 2, buffer: water, offset: 0 },
                GpuBinding::Buffer { binding: 3, buffer: out, offset: 0 },
            ],
            [(cells as u32).div_ceil(256), 1, 1],
            "node.restrict_lattice",
        );
    }
}
