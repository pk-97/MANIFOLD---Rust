//! `node.mesh_texture_weights` — read a texture across a mesh as one weight
//! per vertex, by box projection from the vertex's place in its frame.
//!
//! UV coordinates jump at a mesh's seams (every face of a built-in cube has
//! its own), so a texture sampled by UV pulls a displaced mesh apart there.
//! This node samples by position instead: three planar samples blended by
//! the normal (Blender's Box projection), with mirrored repeat so any image
//! tiles without a seam. Wire its `weights` into a deformer's weights (or a
//! mask's incoming weights) to displace by an image, a layer or noise.

use std::borrow::Cow;

use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use manifold_node_engine::exec::effect_node::EffectNodeContext;
use manifold_node_engine::mesh::MeshVertex;
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::primitive::Primitive;
use manifold_node_engine::primitives::standalone_pipeline::standalone_pipeline;

/// Generated-codegen uniform layout: params in PARAMS order, then
/// `dispatch_count`, padded to 16 bytes.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct TextureWeightsUniforms {
    scale: f32,
    offset_x: f32,
    offset_y: f32,
    bias: f32,
    frame_radius: f32,
    source_offset_x: f32,
    source_offset_y: f32,
    source_offset_z: f32,
    dispatch_count: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

manifold_node_engine::primitive! {
    name: MeshTextureWeights,
    type_id: "node.mesh_texture_weights",
    purpose: "One f32 weight per vertex of an Array<MeshVertex>: the red channel of `field` box-projected at the vertex's frame position q = (position + source_offset) / frame_radius * scale. Three samples on the yz, xz and xy planes (each shifted by offset_x/offset_y) blend by the normal's fourth power; coordinates repeat mirrored so any image tiles seamlessly. weight = blend - bias. A function of position and normal only, so split copies of a corner with one welded normal read one weight.",
    inputs: {
        in: Array(MeshVertex) required,
        field: Texture2D required,
        scale: ScalarF32 optional,
        offset_x: ScalarF32 optional,
        offset_y: ScalarF32 optional,
        bias: ScalarF32 optional,
        frame_radius: ScalarF32 optional,
        source_offset_x: ScalarF32 optional,
        source_offset_y: ScalarF32 optional,
        source_offset_z: ScalarF32 optional,
    },
    outputs: {
        weights: Array(f32),
    },
    params: [
        ParamDef { name: Cow::Borrowed("scale"), label: "Scale", ty: ParamType::Float, default: ParamValue::Float(1.0), range: Some((0.01, 64.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("offset_x"), label: "Offset X", ty: ParamType::Float, default: ParamValue::Float(0.0), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("offset_y"), label: "Offset Y", ty: ParamType::Float, default: ParamValue::Float(0.0), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("bias"), label: "Bias", ty: ParamType::Float, default: ParamValue::Float(0.5), range: Some((0.0, 1.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("frame_radius"), label: "Frame Radius", ty: ParamType::Float, default: ParamValue::Float(1.0), range: Some((0.000001, 1000.0)), enum_values: &[] },
        ParamDef { name: Cow::Borrowed("source_offset_x"), label: "Source Offset X", ty: ParamType::Float, default: ParamValue::Float(0.0), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("source_offset_y"), label: "Source Offset Y", ty: ParamType::Float, default: ParamValue::Float(0.0), range: None, enum_values: &[] },
        ParamDef { name: Cow::Borrowed("source_offset_z"), label: "Source Offset Z", ty: ParamType::Float, default: ParamValue::Float(0.0), range: None, enum_values: &[] },
    ],
    depth_rule: Terminal,
    composition_notes: "Displace by an image: node.smooth_normals → this (in) and any Texture2D (a noise, node.layer_source, an image file) → field, then the weights into node.push_along_normals (directly or through node.mesh_spatial_mask's incoming weights), with the smoothed mesh as the push's direction. Bias 0.5 pushes dark parts in and light parts out. Scale is repeats per frame radius.",
    examples: [],
    picker: { label: "Mesh Texture Weights", category: Atom },
    summary: "Reads a picture across a 3D mesh from all three sides at once, giving every point a value that can push the surface in or out.",
    category: Geometry3D,
    role: Source,
    aliases: ["texture weights", "box projection", "triplanar", "displacement map"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/mesh_texture_weights_body.wgsl"),
}

impl Primitive for MeshTextureWeights {
    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &manifold_node_engine::exec::effect_node::ParamValues,
        input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        (port_name == "weights").then(|| input_capacities.iter().find(|(p, _)| *p == "in").map(|(_, n)| *n)).flatten()
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let uniforms = TextureWeightsUniforms {
            scale: ctx.scalar_or_param("scale", 1.0),
            offset_x: ctx.scalar_or_param("offset_x", 0.0),
            offset_y: ctx.scalar_or_param("offset_y", 0.0),
            bias: ctx.scalar_or_param("bias", 0.5),
            frame_radius: ctx.scalar_or_param("frame_radius", 1.0),
            source_offset_x: ctx.scalar_or_param("source_offset_x", 0.0),
            source_offset_y: ctx.scalar_or_param("source_offset_y", 0.0),
            source_offset_z: ctx.scalar_or_param("source_offset_z", 0.0),
            dispatch_count: 0,
            _pad0: 0,
            _pad1: 0,
            _pad2: 0,
        };
        let Some(src) = ctx.inputs.array("in") else {
            return;
        };
        let Some(field) = ctx.inputs.texture_2d("field") else {
            return;
        };
        let Some(dst) = ctx.outputs.array("weights") else {
            return;
        };
        let vertex_size = std::mem::size_of::<MeshVertex>() as u64;
        let count = ((src.size / vertex_size) as u32).min((dst.size / 4) as u32);
        if count == 0 {
            return;
        }
        let uniforms = TextureWeightsUniforms { dispatch_count: count, ..uniforms };
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let sampler = self.sampler.get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                GpuBinding::Buffer { binding: 1, buffer: src, offset: 0 },
                GpuBinding::Texture { binding: 2, texture: field },
                GpuBinding::Sampler { binding: 3, sampler },
                GpuBinding::Buffer { binding: 4, buffer: dst, offset: 0 },
            ],
            [count.div_ceil(256), 1, 1],
            "node.mesh_texture_weights",
        );
    }
}
