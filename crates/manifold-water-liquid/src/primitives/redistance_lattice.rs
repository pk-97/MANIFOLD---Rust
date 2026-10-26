//! Single-dispatch scalar lattice operation: rebuilds a true distance field from the zero surface.
use crate::field_build::FIELD_REDISTANCE_CELLS;
use crate::float_param;
use manifold_node_engine::exec::effect_node::{EffectNodeContext, ParamValues};
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::primitive::Primitive;
use manifold_node_engine::primitives::standalone_pipeline::standalone_pipeline;

use manifold_gpu::GpuBinding;
use std::borrow::Cow;

/// Codegen uniform layout: params in PARAMS order, then `dispatch_count`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniforms {
    nodes_x: f32,
    nodes_y: f32,
    nodes_z: f32,
    size_x: f32,
    size_y: f32,
    size_z: f32,
    band_cells: f32,
    dispatch_count: u32,
}

manifold_node_engine::primitive! {
    name: RedistanceLattice,
    type_id: "node.redistance_lattice",
    purpose: "Rebuild a node-centred signed distance field from the input zero surface, using the nearest marching-cubes triangle within Band Cells of the lattice's smallest node spacing. The sign comes from the input; distances outside the band saturate exactly.",
    inputs: {
        levelset: Array(f32) required,
        nodes_x: ScalarF32 optional,
        nodes_y: ScalarF32 optional,
        nodes_z: ScalarF32 optional,
        size_x: ScalarF32 optional,
        size_y: ScalarF32 optional,
        size_z: ScalarF32 optional,
        band_cells: ScalarF32 optional,
    },
    outputs: { out: Array(f32), },
    params: [
        float_param!("nodes_x", "Nodes X", 2.0, 2.0, 4096.0),
        float_param!("nodes_y", "Nodes Y", 2.0, 2.0, 4096.0),
        float_param!("nodes_z", "Nodes Z", 2.0, 2.0, 4096.0),
        float_param!("size_x", "Size X", 4.0, 0.001, 1000.0),
        float_param!("size_y", "Size Y", 4.0, 0.001, 1000.0),
        float_param!("size_z", "Size Z", 4.0, 0.001, 1000.0),
        float_param!("band_cells", "Distance Band (Cells)", FIELD_REDISTANCE_CELLS, 0.5, 16.0),
    ],
    depth_rule: Terminal,
    composition_notes: "Band Cells sets the band as that many of the lattice's smallest node spacing, so the depth in cells is the same on any lattice size. Never substitute a blur for redistancing.",
    examples: [],
    picker: { label: "Redistance Lattice", category: Atom },
    summary: "Rebuilds a node-centred signed distance field from the input zero surface, so the field holds true distances again.",
    category: Particles3D,
    role: Filter,
    aliases: ["distance field"],
    fusion_kind: Pointwise,
    wgsl_body: concat!(include_str!("shaders/marching_cubes_common.wgsl"), "\n", include_str!("shaders/redistance_lattice_body.wgsl")),
    input_access: [BufferGather],
}

impl Primitive for RedistanceLattice {
    fn array_output_capacity(
        &self,
        port: &str,
        _params: &ParamValues,
        inputs: &[(&str, u32)],
    ) -> Option<u32> {
        (port == "out")
            .then(|| {
                inputs
                    .iter()
                    .find(|(name, _)| *name == "levelset")
                    .map(|&(_, n)| n)
            })
            .flatten()
    }
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let (Some(input), Some(output)) = (ctx.inputs.array("levelset"), ctx.outputs.array("out"))
        else {
            return;
        };
        let count = (input.size.min(output.size) / 4) as u32;
        if count == 0 {
            return;
        }
        let uniforms = Uniforms {
            nodes_x: ctx.scalar_or_param("nodes_x", 2.0),
            nodes_y: ctx.scalar_or_param("nodes_y", 2.0),
            nodes_z: ctx.scalar_or_param("nodes_z", 2.0),
            size_x: ctx.scalar_or_param("size_x", 4.0),
            size_y: ctx.scalar_or_param("size_y", 4.0),
            size_z: ctx.scalar_or_param("size_z", 4.0),
            band_cells: ctx.scalar_or_param("band_cells", FIELD_REDISTANCE_CELLS),
            dispatch_count: count,
        };

        let nodes = [uniforms.nodes_x, uniforms.nodes_y, uniforms.nodes_z];
        let sizes = [uniforms.size_x, uniforms.size_y, uniforms.size_z];
        if nodes
            .iter()
            .any(|n| !n.is_finite() || *n < 2.0 || n.fract() != 0.0)
            || nodes
                .iter()
                .try_fold(1u64, |n, axis| n.checked_mul(*axis as u64))
                .is_none_or(|n| n > u64::from(count))
            || sizes.iter().any(|s| !s.is_finite() || *s <= 0.0)
            || !(uniforms.band_cells.is_finite() && uniforms.band_cells > 0.0)
        {
            ctx.error("Redistance Lattice: expected a complete node lattice, positive sizes and a positive finite band in cells");
            return;
        }
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: input,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: output,
                    offset: 0,
                },
            ],
            [count.div_ceil(256), 1, 1],
            "node.redistance_lattice",
        );
    }
}

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
mod extent;

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests;

/// The marching-cubes tables and helpers node.redistance_lattice includes.
pub const MARCHING_CUBES_COMMON: &str = include_str!("shaders/marching_cubes_common.wgsl");
