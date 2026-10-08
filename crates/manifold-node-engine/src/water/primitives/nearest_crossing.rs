//! `node.nearest_crossing` — spread each whitewater cell's nearest surface
//! crossing one cell further (`docs/GPU_WHITEWATER_DESIGN.md` D4, section
//! 3.3). A per-element gather on the codegen path.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use crate::float_param;
use crate::primitives::standalone_pipeline::standalone_pipeline;
use crate::exec::effect_node::{EffectNodeContext, ParamValues};
use crate::parameters::{ParamDef, ParamType, ParamValue};
use crate::primitive::Primitive;
use crate::water::whitewater::{SURFACE_CROSSING_BYTES, SurfaceCrossing, WHITEWATER_COMMON, cell_total, grid_cells, grid_nodes};

/// Codegen uniform layout: params in PARAMS order, then `dispatch_count`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct NearestUniforms {
    nodes_x: f32,
    nodes_y: f32,
    nodes_z: f32,
    step: f32,
    dispatch_count: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

crate::primitive! {
    name: NearestCrossing,
    type_id: "node.nearest_crossing",
    purpose: "One pass of spreading surface crossings over the whitewater grid: each cell takes whichever of its own crossing and the crossings of the 26 cells Step cells away lies nearest its centre, with its normal, keeping its own level. Passes at steps 2, 1, 1 give every cell the nearest crossing within four cells; a first pass at step 2 finds the nearest crossing where three unit passes settle on a neighbour's.",
    inputs: {
        crossings: Array(SurfaceCrossing) required,
        nodes_x: ScalarF32 optional, nodes_y: ScalarF32 optional, nodes_z: ScalarF32 optional,
    },
    outputs: {
        out: Array(SurfaceCrossing),
    },
    params: [
        float_param!("nodes_x", "Solid Nodes X", 71.0, 3.0, 4096.0),
        float_param!("nodes_y", "Solid Nodes Y", 71.0, 3.0, 4096.0),
        float_param!("nodes_z", "Solid Nodes Z", 71.0, 3.0, 4096.0),
        float_param!("step", "Step (cells)", 1.0, 1.0, 8.0),
    ],
    depth_rule: Terminal,
    composition_notes: "Chain three after node.surface_crossings, each reading the one before, at Step 2, 1, 1, then node.crossing_distance. nodes_x/y/z are the solid lattice's, as node.surface_crossings takes them.",
    examples: [],
    summary: "Passes each grid cell the closest known point on the liquid's surface from its neighbours.",
    category: Particles3D,
    role: Filter,
    aliases: ["closest point", "surface spread", "redistance pass"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/nearest_crossing_body.wgsl"),
    input_access: [BufferGather],
    wgsl_includes: [WHITEWATER_COMMON],
}

impl Primitive for NearestCrossing {
    fn array_output_capacity(&self, port: &str, _params: &ParamValues, inputs: &[(&str, u32)]) -> Option<u32> {
        (port == "out").then(|| inputs.iter().find(|(name, _)| *name == "crossings").map(|&(_, n)| n)).flatten()
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let nodes = grid_nodes(ctx);
        let Some(cells) = grid_cells(nodes) else {
            ctx.error(format!("Nearest Crossing: a {nodes:?} solid lattice has too few or too many nodes"));
            return;
        };
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let (Some(crossings), Some(out)) = (ctx.inputs.array("crossings"), ctx.outputs.array("out")) else {
            return;
        };
        let count = cell_total(cells);
        if count * SURFACE_CROSSING_BYTES > crossings.size.min(out.size) {
            ctx.error(format!("Nearest Crossing: a {nodes:?}-node grid is larger than its arrays"));
            return;
        }
        let uniforms = NearestUniforms {
            nodes_x: nodes[0] as f32,
            nodes_y: nodes[1] as f32,
            nodes_z: nodes[2] as f32,
            step: ctx.param_f32("step", 1.0),
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
                GpuBinding::Buffer { binding: 2, buffer: out, offset: 0 },
            ],
            [(count as u32).div_ceil(256), 1, 1],
            "node.nearest_crossing",
        );
    }
}

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
mod extent;
