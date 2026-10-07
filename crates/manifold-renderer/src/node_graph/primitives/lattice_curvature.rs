//! `node.lattice_curvature` — FLIP's curvature grid on the whitewater grid's
//! signed distance (`docs/GPU_WHITEWATER_DESIGN.md` section 3.3, O1). A
//! per-element gather on the codegen path.
//!
//! Ported from FLIP Fluids particlelevelset.cpp (MIT, Copyright (C) 2026 Ryan L. Guy & Dennis Fassbaender); see THIRD_PARTY_NOTICES.md.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use super::sort_particles_into_cells::float_param;
use super::standalone_pipeline::standalone_pipeline;
use crate::node_graph::effect_node::{EffectNodeContext, ParamValues};
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;
use crate::node_graph::whitewater::{KnownValue, WHITEWATER_COMMON, cell_total, grid_cells, grid_nodes};

/// Codegen uniform layout: params in PARAMS order, then `dispatch_count`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct CurvatureUniforms {
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
    name: LatticeCurvature,
    type_id: "node.lattice_curvature",
    purpose: "Mean curvature of the whitewater grid's signed distance, as FLIP's whitewater measures it: at a cell off the grid border whose distance and six face neighbours' distances all lie within 2 cells of the surface, central differences give the curvature in 1/m, clamped to ±1 per cell, and the cell is known; elsewhere the value is 0 and unknown, for node.extend_lattice to fill.",
    inputs: {
        distance: Array(f32) required,
        nodes_x: ScalarF32 optional, nodes_y: ScalarF32 optional, nodes_z: ScalarF32 optional,
        cell_size: ScalarF32 optional,
    },
    outputs: {
        out: Array(KnownValue),
    },
    params: [
        float_param!("nodes_x", "Solid Nodes X", 71.0, 3.0, 4096.0),
        float_param!("nodes_y", "Solid Nodes Y", 71.0, 3.0, 4096.0),
        float_param!("nodes_z", "Solid Nodes Z", 71.0, 3.0, 4096.0),
        float_param!("cell_size", "Cell Size", 0.0625, 0.0001, 100.0),
    ],
    depth_rule: Terminal,
    composition_notes: "distance from node.crossing_distance, nodes_x/y/z the particle frame's grid_nodes_x/y/z, cell_size the domain's. Chain node.extend_lattice three times after it, as FLIP extends its curvature three layers.",
    examples: [],
    summary: "Measures how sharply the liquid's surface bends at each grid cell, which is where wave crests throw foam.",
    category: Particles3D,
    role: Filter,
    aliases: ["curvature", "mean curvature", "surface bend"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/lattice_curvature_body.wgsl"),
    input_access: [BufferGather],
    wgsl_includes: [WHITEWATER_COMMON],
}

impl Primitive for LatticeCurvature {
    fn array_output_capacity(&self, port: &str, _params: &ParamValues, inputs: &[(&str, u32)]) -> Option<u32> {
        (port == "out").then(|| inputs.iter().find(|(name, _)| *name == "distance").map(|&(_, n)| n)).flatten()
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let nodes = grid_nodes(ctx);
        let Some(cells) = grid_cells(nodes) else {
            ctx.error(format!("Lattice Curvature: a {nodes:?} solid lattice has too few or too many nodes"));
            return;
        };
        let cell_size = ctx.scalar_or_param("cell_size", 0.0625);
        if !(cell_size > 0.0 && cell_size.is_finite()) {
            ctx.error(format!("Lattice Curvature: cell size {cell_size} is not a length"));
            return;
        }
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let (Some(distance), Some(out)) = (ctx.inputs.array("distance"), ctx.outputs.array("out")) else {
            return;
        };
        let count = cell_total(cells);
        if count * 4 > distance.size || count * 8 > out.size {
            ctx.error(format!("Lattice Curvature: a {nodes:?}-node grid is larger than its arrays"));
            return;
        }
        let uniforms = CurvatureUniforms {
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
                GpuBinding::Buffer { binding: 1, buffer: distance, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: out, offset: 0 },
            ],
            [(count as u32).div_ceil(256), 1, 1],
            "node.lattice_curvature",
        );
    }
}

#[cfg(any(test, feature = "gpu-proofs"))]
mod extent;
