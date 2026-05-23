//! `node.heightmap_to_normal` — scalar height field → tangent-space
//! normal map via central-difference gradient.
//!
//! Reads `in.r` as height per pixel, computes `(dh/dx, dh/dy)` via
//! half-difference of adjacent samples, then emits the unnormalised
//! tangent-space normal `vec3(-dh/dx, -dh/dy, z_scale)` normalised.
//! Larger `z_scale` = flatter normals; smaller = steeper.
//!
//! Output: RGB = signed tangent-space normal in [-1, 1], A = 1.

use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct HeightmapNormalUniforms {
    z_scale: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

crate::primitive! {
    name: HeightmapToNormal,
    type_id: "node.heightmap_to_normal",
    purpose: "Scalar height field (read from `in.r`) → tangent-space normal map (RGB) via central-difference gradient. Larger `z_scale` flattens the normal; smaller `z_scale` steepens it. The universal building block for fake-lit / matcap / PBR shading of procedural heightmaps. Output is SIGNED (range [-1, 1] per channel) — caller may abs/scale/grade downstream.",
    inputs: {
        in: Texture2D required,
        z_scale: ScalarF32 optional,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: "z_scale",
            label: "Z Scale",
            ty: ParamType::Float,
            default: ParamValue::Float(0.5),
            range: Some((0.001, 4.0)),
            enum_values: &[],
        },
    ],
    composition_notes: "Height is read from the R channel only. If your height is a derived quantity (e.g. `length(color.rg)` in the oily-fluid family) wire `node.length_vec2` upstream first. Pair downstream with `node.lambert_directional`, `node.matcap_two_tone`, `node.fresnel_rim`, `node.blinn_specular` for shading variations. For chromatic-aberration-style displaced normals (oily-fluid Oil Slick), follow with `node.chromatic_displace` reading the normal map.",
    examples: [],
    picker: { label: "Heightmap → Normal", category: Atom },
}

impl Primitive for HeightmapToNormal {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let z_scale = match ctx.inputs.scalar("z_scale") {
            Some(ParamValue::Float(f)) => f,
            _ => match ctx.params.get("z_scale") {
                Some(ParamValue::Float(f)) => *f,
                _ => 0.5,
            },
        };

        let Some(src) = ctx.inputs.texture_2d("in") else {
            return;
        };
        let Some(target) = ctx.outputs.texture_2d("out") else {
            return;
        };
        let (w, h) = (target.width, target.height);
        if w == 0 || h == 0 {
            return;
        }

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                include_str!("shaders/heightmap_to_normal.wgsl"),
                "cs_main",
                "node.heightmap_to_normal",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        let uniforms = HeightmapNormalUniforms {
            z_scale,
            _pad0: 0.0,
            _pad1: 0.0,
            _pad2: 0.0,
        };

        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Texture {
                    binding: 1,
                    texture: src,
                },
                GpuBinding::Sampler {
                    binding: 2,
                    sampler,
                },
                GpuBinding::Texture {
                    binding: 3,
                    texture: target,
                },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "node.heightmap_to_normal",
        );
    }
}
