//! `node.crossing_distance` — the whitewater grid's signed distance to the
//! liquid surface from each cell's nearest crossing
//! (`docs/GPU_WHITEWATER_DESIGN.md` D4, section 3.3). A per-element atom on
//! the codegen path.
//!
//! Ported from FLIP Fluids particlelevelset.cpp (MIT, Copyright (C) 2026 Ryan L. Guy & Dennis Fassbaender); see THIRD_PARTY_NOTICES.md.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use super::sort_particles_into_cells::float_param;
use super::standalone_pipeline::standalone_pipeline;
use crate::node_graph::effect_node::{EffectNodeContext, ParamValues};
use crate::node_graph::freeze::classify::FusedOutputCapacity;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;
use crate::node_graph::whitewater::{SURFACE_CROSSING_BYTES, SurfaceCrossing, WHITEWATER_COMMON, cell_total, grid_cells, grid_nodes};

/// Codegen uniform layout: params in PARAMS order, then `dispatch_count`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct DistanceUniforms {
    nodes_x: f32,
    nodes_y: f32,
    nodes_z: f32,
    cell_size: f32,
    dispatch_count: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

crate::primitive! {
    name: CrossingDistance,
    type_id: "node.crossing_distance",
    purpose: "The whitewater grid's signed distance to the liquid surface, one f32 per cell in metres: the distance from the cell centre to the tangent plane at its nearest crossing (to the crossing itself when it carries no normal), negative where the level at the centre is, held to 4 cells. FLIP's post-process follows: a cell whose centre is inside a solid and whose distance is under half a cell reads minus half a cell, so the liquid meets the walls, and a distance within 0.005 cell of 0 moves out to it on its own side.",
    inputs: {
        crossings: Array(SurfaceCrossing) required,
        solid: Array(f32) required,
        nodes_x: ScalarF32 optional, nodes_y: ScalarF32 optional, nodes_z: ScalarF32 optional,
        cell_size: ScalarF32 optional,
    },
    outputs: {
        out: Array(f32),
    },
    params: [
        float_param!("nodes_x", "Solid Nodes X", 71.0, 3.0, 4096.0),
        float_param!("nodes_y", "Solid Nodes Y", 71.0, 3.0, 4096.0),
        float_param!("nodes_z", "Solid Nodes Z", 71.0, 3.0, 4096.0),
        float_param!("cell_size", "Cell Size", 0.0625, 0.0001, 100.0),
    ],
    depth_rule: Terminal,
    composition_notes: "After three node.nearest_crossing passes; solid and nodes_x/y/z from the particle frame's solid_b and grid_nodes_x/y/z, cell_size the domain's. Feeds node.liquid_cells and node.lattice_curvature.",
    examples: [],
    picker: { label: "Crossing Distance", category: Atom },
    summary: "Measures how far each grid cell is from the liquid's surface, negative inside the liquid.",
    category: Particles3D,
    role: Filter,
    aliases: ["signed distance", "level set", "distance to surface"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/crossing_distance_body.wgsl"),
    input_access: [Coincident, BufferGather],
    output_capacity: FusedOutputCapacity::FromInput { input: "crossings" },
    wgsl_includes: [WHITEWATER_COMMON],
}

impl Primitive for CrossingDistance {
    fn array_output_capacity(&self, port: &str, _params: &ParamValues, inputs: &[(&str, u32)]) -> Option<u32> {
        (port == "out").then(|| inputs.iter().find(|(name, _)| *name == "crossings").map(|&(_, n)| n)).flatten()
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let nodes = grid_nodes(ctx);
        let Some(cells) = grid_cells(nodes) else {
            ctx.error(format!("Crossing Distance: a {nodes:?} solid lattice has too few or too many nodes"));
            return;
        };
        let cell_size = ctx.scalar_or_param("cell_size", 0.0625);
        if !(cell_size > 0.0 && cell_size.is_finite()) {
            ctx.error(format!("Crossing Distance: cell size {cell_size} is not a length"));
            return;
        }
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let (Some(crossings), Some(solid), Some(out)) = (ctx.inputs.array("crossings"), ctx.inputs.array("solid"), ctx.outputs.array("out")) else {
            return;
        };
        let count = cell_total(cells);
        if count * SURFACE_CROSSING_BYTES > crossings.size || count * 4 > out.size || cell_total(nodes) * 4 > solid.size {
            ctx.error(format!("Crossing Distance: a {nodes:?}-node grid is larger than its arrays"));
            return;
        }
        let uniforms = DistanceUniforms {
            nodes_x: nodes[0] as f32,
            nodes_y: nodes[1] as f32,
            nodes_z: nodes[2] as f32,
            cell_size,
            dispatch_count: count as u32,
            _pad0: 0,
            _pad1: 0,
            _pad2: 0,
        };
        let gpu = ctx.gpu_encoder();
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                GpuBinding::Buffer { binding: 1, buffer: crossings, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: solid, offset: 0 },
                GpuBinding::Buffer { binding: 3, buffer: out, offset: 0 },
            ],
            [(count as u32).div_ceil(256), 1, 1],
            "node.crossing_distance",
        );
    }
}
