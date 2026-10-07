//! Single-dispatch scalar lattice operation; see GPU_FLUID_SURFACE_DESIGN.md, Fill Pits.
use super::sort_particles_into_cells::float_param;
use super::standalone_pipeline::standalone_pipeline;
use crate::node_graph::effect_node::{EffectNodeContext, ParamValues};
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

use manifold_gpu::GpuBinding;
use std::borrow::Cow;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniforms {
    nodes_x: f32,
    nodes_y: f32,
    nodes_z: f32,
    size_x: f32,
    size_y: f32,
    size_z: f32,
    band: f32,
    enabled: f32,
    dispatch_count: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

crate::primitive! {
    name: RedistanceLattice,
    type_id: "node.redistance_lattice",
    purpose: "Rebuild a node-centred signed distance field from the input zero surface, using the nearest marching-cubes triangle within Band metres. The sign comes from the input; distances outside Band saturate exactly. Disabled returns the input bits unchanged.",
    inputs: {
        levelset: Array(f32) required,
        nodes_x: ScalarF32 optional,
        nodes_y: ScalarF32 optional,
        nodes_z: ScalarF32 optional,
        size_x: ScalarF32 optional,
        size_y: ScalarF32 optional,
        size_z: ScalarF32 optional,
        band: ScalarF32 optional,
        enabled: ScalarF32 optional,
    },
    outputs: { out: Array(f32), },
    params: [
        float_param!("nodes_x", "Nodes X", 2.0, 2.0, 4096.0),
        float_param!("nodes_y", "Nodes Y", 2.0, 2.0, 4096.0),
        float_param!("nodes_z", "Nodes Z", 2.0, 2.0, 4096.0),
        float_param!("size_x", "Size X", 4.0, 0.001, 1000.0),
        float_param!("size_y", "Size Y", 4.0, 0.001, 1000.0),
        float_param!("size_z", "Size Z", 4.0, 0.001, 1000.0),
        float_param!("band", "Distance Band", 1.0, 0.001, 100.0),
        float_param!("enabled", "Enabled", 1.0, 0.0, 1.0),
    ],
    depth_rule: Terminal,
    composition_notes: "For morphological closing, offset by minus the grow distance, redistance with a band wider than the grow distance, then offset back by the grow distance before clipping to solids. Never substitute a blur for redistancing. The input band must cover the grown surface.",
    examples: [],
    picker: { label: "Redistance Lattice", category: Atom },
    summary: "Rebuild a node-centred signed distance field from the input zero surface, using the nearest marching-cubes triangle within Band metres. The sign comes from the input; distances outside Band saturate exactly. Disabled returns the input bits unchanged.",
    category: Particles3D,
    role: Filter,
    aliases: ["distance field", "morphological closing"],
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
            band: ctx.scalar_or_param("band", 1.0),
            enabled: ctx.scalar_or_param("enabled", 1.0),
            dispatch_count: count,
            _pad0: 0,
            _pad1: 0,
            _pad2: 0,
        };

        let nodes = [uniforms.nodes_x, uniforms.nodes_y, uniforms.nodes_z];
        let sizes = [uniforms.size_x, uniforms.size_y, uniforms.size_z];
        if uniforms.enabled != 0.0
            && (nodes
                .iter()
                .any(|n| !n.is_finite() || *n < 2.0 || n.fract() != 0.0)
                || nodes
                    .iter()
                    .try_fold(1u64, |n, axis| n.checked_mul(*axis as u64))
                    .is_none_or(|n| n > u64::from(count))
                || sizes.iter().any(|s| !s.is_finite() || *s <= 0.0)
                || !uniforms.band.is_finite()
                || uniforms.band <= 0.0)
        {
            ctx.error("Redistance Lattice: expected a complete node lattice, positive sizes and a positive finite distance band");
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

#[cfg(any(test, feature = "gpu-proofs"))]
mod extent;
