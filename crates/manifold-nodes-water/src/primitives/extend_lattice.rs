//! `node.extend_lattice` — one layer of FLIP's grid extrapolation into a
//! whitewater grid's unknown cells (`docs/GPU_WHITEWATER_DESIGN.md` section
//! 3.3). A per-element gather on the codegen path.
//!
//! Ported from FLIP Fluids gridutils.h (MIT, Copyright (C) 2026 Ryan L. Guy & Dennis Fassbaender); see THIRD_PARTY_NOTICES.md.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use crate::float_param;
use manifold_node_engine::primitives::standalone_pipeline::standalone_pipeline;
use manifold_node_engine::exec::effect_node::{EffectNodeContext, ParamValues};
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::primitive::Primitive;
use crate::whitewater::{KnownValue, WHITEWATER_COMMON, cell_total, grid_cells, grid_nodes};

/// Codegen uniform layout: params in PARAMS order, then `dispatch_count`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ExtendUniforms {
    nodes_x: f32,
    nodes_y: f32,
    nodes_z: f32,
    dispatch_count: u32,
}

manifold_node_engine::primitive! {
    name: ExtendLattice,
    type_id: "node.extend_lattice",
    purpose: "One layer of FLIP's grid extrapolation on the whitewater grid: a known cell keeps its value; an unknown cell off the grid border with a known face neighbour off the border becomes known with the mean of its face neighbours that are known or on the border (FLIP counts the border as settled and never fills it). Others stay as they are.",
    inputs: {
        values: Array(KnownValue) required,
        nodes_x: ScalarF32 optional, nodes_y: ScalarF32 optional, nodes_z: ScalarF32 optional,
    },
    outputs: {
        out: Array(KnownValue),
    },
    params: [
        float_param!("nodes_x", "Solid Nodes X", 71.0, 3.0, 4096.0),
        float_param!("nodes_y", "Solid Nodes Y", 71.0, 3.0, 4096.0),
        float_param!("nodes_z", "Solid Nodes Z", 71.0, 3.0, 4096.0),
    ],
    depth_rule: Terminal,
    composition_notes: "Chain one per layer, each reading the one before: three after node.lattice_curvature, as FLIP extends its curvature grid. nodes_x/y/z are the particle frame's grid_nodes_x/y/z.",
    examples: [],
    summary: "Fills the empty cells next to known ones with their neighbours' average, one cell further each time.",
    category: Particles3D,
    role: Filter,
    aliases: ["extrapolate", "extend field", "fill unknown cells"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/extend_lattice_body.wgsl"),
    input_access: [BufferGather],
    wgsl_includes: [WHITEWATER_COMMON],
}

impl Primitive for ExtendLattice {
    fn array_output_capacity(&self, port: &str, _params: &ParamValues, inputs: &[(&str, u32)]) -> Option<u32> {
        (port == "out").then(|| inputs.iter().find(|(name, _)| *name == "values").map(|&(_, n)| n)).flatten()
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let nodes = grid_nodes(ctx);
        let Some(cells) = grid_cells(nodes) else {
            ctx.error(format!("Extend Lattice: a {nodes:?} solid lattice has too few or too many nodes"));
            return;
        };
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let (Some(values), Some(out)) = (ctx.inputs.array("values"), ctx.outputs.array("out")) else {
            return;
        };
        let count = cell_total(cells);
        if count * 8 > values.size.min(out.size) {
            ctx.error(format!("Extend Lattice: a {nodes:?}-node grid is larger than its arrays"));
            return;
        }
        let uniforms = ExtendUniforms { nodes_x: nodes[0] as f32, nodes_y: nodes[1] as f32, nodes_z: nodes[2] as f32, dispatch_count: count as u32 };
        let gpu = ctx.gpu_encoder();
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                GpuBinding::Buffer { binding: 1, buffer: values, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: out, offset: 0 },
            ],
            [(count as u32).div_ceil(256), 1, 1],
            "node.extend_lattice",
        );
    }
}

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
mod extent;
