//! `node.liquid_cells` — FLIP's emitter material grid on the whitewater
//! grid: air, liquid or solid per cell, the liquid shrunk off the air
//! (`docs/GPU_WHITEWATER_DESIGN.md` section 3.3). A per-element gather on the
//! codegen path.
//!
//! Ported from FLIP Fluids diffuseparticlesimulation.cpp (MIT, Copyright (C) 2026 Ryan L. Guy & Dennis Fassbaender); see THIRD_PARTY_NOTICES.md.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use super::sort_particles_into_cells::float_param;
use super::standalone_pipeline::standalone_pipeline;
use crate::node_graph::effect_node::{EffectNodeContext, ParamValues};
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;
use crate::node_graph::whitewater::{WHITEWATER_COMMON, cell_total, grid_cells, grid_nodes};

/// Codegen uniform layout: params in PARAMS order, then `dispatch_count`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct CellsUniforms {
    nodes_x: f32,
    nodes_y: f32,
    nodes_z: f32,
    dispatch_count: u32,
}

crate::primitive! {
    name: LiquidCells,
    type_id: "node.liquid_cells",
    purpose: "Mark each whitewater cell air (0), liquid (1) or solid (2) as FLIP's whitewater emitter does: solid where the mean of the cell's eight solid-lattice corners is below 0, else liquid where its distance is below 0, else air; then a liquid cell with an air face neighbour becomes air, so emitters sit in the surface layer's air.",
    inputs: {
        distance: Array(f32) required,
        solid: Array(f32) required,
        nodes_x: ScalarF32 optional, nodes_y: ScalarF32 optional, nodes_z: ScalarF32 optional,
    },
    outputs: {
        out: Array(u32),
    },
    params: [
        float_param!("nodes_x", "Solid Nodes X", 71.0, 3.0, 4096.0),
        float_param!("nodes_y", "Solid Nodes Y", 71.0, 3.0, 4096.0),
        float_param!("nodes_z", "Solid Nodes Z", 71.0, 3.0, 4096.0),
    ],
    depth_rule: Terminal,
    composition_notes: "distance from node.crossing_distance; solid and nodes_x/y/z from the particle frame's solid_b and grid_nodes_x/y/z. The whitewater's wavecrest and type rules read it.",
    examples: [],
    summary: "Sorts every grid cell into air, liquid or wall, the map whitewater uses to decide where spray and foam can form.",
    category: Particles3D,
    role: Filter,
    aliases: ["material grid", "cell types", "fluid mask"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/liquid_cells_body.wgsl"),
    input_access: [BufferGather, BufferGather],
    wgsl_includes: [WHITEWATER_COMMON],
}

impl Primitive for LiquidCells {
    fn array_output_capacity(&self, port: &str, _params: &ParamValues, inputs: &[(&str, u32)]) -> Option<u32> {
        (port == "out").then(|| inputs.iter().find(|(name, _)| *name == "distance").map(|&(_, n)| n)).flatten()
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let nodes = grid_nodes(ctx);
        let Some(cells) = grid_cells(nodes) else {
            ctx.error(format!("Liquid Cells: a {nodes:?} solid lattice has too few or too many nodes"));
            return;
        };
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let (Some(distance), Some(solid), Some(out)) = (ctx.inputs.array("distance"), ctx.inputs.array("solid"), ctx.outputs.array("out")) else {
            return;
        };
        let count = cell_total(cells);
        if count * 4 > distance.size.min(out.size) || cell_total(nodes) * 4 > solid.size {
            ctx.error(format!("Liquid Cells: a {nodes:?}-node grid is larger than its arrays"));
            return;
        }
        let uniforms = CellsUniforms { nodes_x: nodes[0] as f32, nodes_y: nodes[1] as f32, nodes_z: nodes[2] as f32, dispatch_count: count as u32 };
        let gpu = ctx.gpu_encoder();
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                GpuBinding::Buffer { binding: 1, buffer: distance, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: solid, offset: 0 },
                GpuBinding::Buffer { binding: 3, buffer: out, offset: 0 },
            ],
            [(count as u32).div_ceil(256), 1, 1],
            "node.liquid_cells",
        );
    }
}
