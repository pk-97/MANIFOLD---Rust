//! `node.surface_crossings` — where the liquid surface crosses the refined
//! level set's edges inside each whitewater cell
//! (`docs/GPU_WHITEWATER_DESIGN.md` D4, section 3.3). A per-element gather on
//! the codegen path.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use super::sort_particles_into_cells::float_param;
use super::standalone_pipeline::standalone_pipeline;
use crate::node_graph::effect_node::{EffectNodeContext, ParamValues};
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;
use crate::node_graph::whitewater::{SURFACE_CROSSING_BYTES, SurfaceCrossing, WHITEWATER_COMMON, cell_total, grid_cells, grid_nodes, refinement};

/// Codegen uniform layout: params in PARAMS order, then `dispatch_count`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct CrossingsUniforms {
    nodes_x: f32,
    nodes_y: f32,
    nodes_z: f32,
    level_nodes_x: f32,
    level_nodes_y: f32,
    level_nodes_z: f32,
    dispatch_count: u32,
    _pad0: u32,
}

fn param_nodes(params: &ParamValues, names: [&str; 3], default: f32) -> [u32; 3] {
    names.map(|name| match params.get(name) {
        Some(ParamValue::Float(v)) => v.round().max(0.0) as u32,
        _ => default as u32,
    })
}

crate::primitive! {
    name: SurfaceCrossings,
    type_id: "node.surface_crossings",
    purpose: "For each cell of the whitewater grid (the solid lattice read as nodes − 1 cells a side), the zero crossing of the refined level set nearest the cell centre: of the edges between refined nodes inside the cell (144 at 3 nodes per cell), those whose ends straddle zero with neither end in a solid cross at the linear root. Out holds the crossing in grid cells from the lattice's first node (1e6 when none), the unit surface normal there (the level set's gradient at the edge's liquid end, differenced against liquid neighbours where it has them, since a node outside may sit at the level set's cap; zero when none), and the level set at the cell centre in metres.",
    inputs: {
        level_set: Array(f32) required,
        solid: Array(f32) required,
        nodes_x: ScalarF32 optional, nodes_y: ScalarF32 optional, nodes_z: ScalarF32 optional,
        level_nodes_x: ScalarF32 optional, level_nodes_y: ScalarF32 optional, level_nodes_z: ScalarF32 optional,
    },
    outputs: {
        out: Array(SurfaceCrossing),
    },
    params: [
        float_param!("nodes_x", "Solid Nodes X", 71.0, 3.0, 4096.0),
        float_param!("nodes_y", "Solid Nodes Y", 71.0, 3.0, 4096.0),
        float_param!("nodes_z", "Solid Nodes Z", 71.0, 3.0, 4096.0),
        float_param!("level_nodes_x", "Level Nodes X", 211.0, 2.0, 16385.0),
        float_param!("level_nodes_y", "Level Nodes Y", 211.0, 2.0, 16385.0),
        float_param!("level_nodes_z", "Level Nodes Z", 211.0, 2.0, 16385.0),
    ],
    depth_rule: Terminal,
    composition_notes: "Wire level_set and level_nodes_x/y/z from the Liquid Surface group's level_set outputs, solid and nodes_x/y/z from the particle frame's solid_b and grid_nodes_x/y/z. The level set must refine the grid by the same whole number (1 to 4) on every axis, or nothing runs, a named error. Then node.nearest_crossing three times and node.crossing_distance.",
    examples: [],
    summary: "Finds where the liquid's surface passes through each grid cell, the first step to measuring distance to the surface.",
    category: Particles3D,
    role: Filter,
    aliases: ["zero crossing", "surface points", "redistance"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/surface_crossings_body.wgsl"),
    input_access: [BufferGather, BufferGather],
    wgsl_includes: [WHITEWATER_COMMON],
}

impl Primitive for SurfaceCrossings {
    fn array_output_capacity(&self, port: &str, params: &ParamValues, inputs: &[(&str, u32)]) -> Option<u32> {
        if port != "out" {
            return None;
        }
        // The lattice may arrive on wires, which the planner cannot read; a
        // cell per solid node covers any lattice the solid array holds.
        if let Some(&(_, nodes)) = inputs.iter().find(|(name, _)| *name == "solid") {
            return Some(nodes);
        }
        let cells = grid_cells(param_nodes(params, ["nodes_x", "nodes_y", "nodes_z"], 71.0))?;
        u32::try_from(cell_total(cells)).ok()
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let nodes = grid_nodes(ctx);
        let levels = ["level_nodes_x", "level_nodes_y", "level_nodes_z"].map(|name| ctx.scalar_or_param(name, 211.0).round().max(0.0) as u32);
        let s = match refinement(nodes, levels) {
            Ok(s) => s,
            Err(message) => {
                ctx.error(format!("Surface Crossings: {message}"));
                return;
            }
        };
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let (Some(level_set), Some(solid), Some(out)) = (ctx.inputs.array("level_set"), ctx.inputs.array("solid"), ctx.outputs.array("out")) else {
            return;
        };
        let cells = nodes.map(|n| n - 1);
        let count = cell_total(cells);
        let level_count = cell_total(levels);
        if count * SURFACE_CROSSING_BYTES > out.size || cell_total(nodes) * 4 > solid.size || level_count * 4 > level_set.size {
            ctx.error(format!("Surface Crossings: a {nodes:?}-node grid at refinement {s} is larger than its arrays"));
            return;
        }
        let uniforms = CrossingsUniforms {
            nodes_x: nodes[0] as f32,
            nodes_y: nodes[1] as f32,
            nodes_z: nodes[2] as f32,
            level_nodes_x: levels[0] as f32,
            level_nodes_y: levels[1] as f32,
            level_nodes_z: levels[2] as f32,
            dispatch_count: count as u32,
            _pad0: 0,
        };
        let gpu = ctx.gpu_encoder();
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                GpuBinding::Buffer { binding: 1, buffer: level_set, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: solid, offset: 0 },
                GpuBinding::Buffer { binding: 3, buffer: out, offset: 0 },
            ],
            [(count as u32).div_ceil(256), 1, 1],
            "node.surface_crossings",
        );
    }
}

#[cfg(any(test, feature = "gpu-proofs"))]
mod extent;
