//! `node.lambert_directional` — Lambert (diffuse) lighting from a
//! tangent-space normal map + directional light + ambient floor.
//!
//! Output is grayscale [0, 1] (broadcast to RGB). Caller tints
//! downstream with `node.color_grade` / `node.color_ramp` if needed.

use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

// Layout matches the WGSL struct exactly: vec3<f32> aligns to 16
// bytes, so a pad slot sits between the light components and
// `ambient`. Total 32 bytes.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct LambertUniforms {
    light_x: f32,
    light_y: f32,
    light_z: f32,
    _vec3_pad: f32,
    ambient: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

crate::primitive! {
    name: LambertDirectional,
    type_id: "node.lambert_directional",
    purpose: "Lambert (diffuse) shading from a tangent-space normal map and a directional light: `out = max(dot(n, normalize(light_dir)), 0) * (1-ambient) + ambient`. Output is grayscale [0, 1] (broadcast to RGB). The basic directional-lighting atom — pair with `node.color_ramp` to tint, or sum with `node.fresnel_rim` / `node.blinn_specular` for stylized PBR.",
    inputs: {
        normal: Texture2D required,
        light_x: ScalarF32 optional,
        light_y: ScalarF32 optional,
        light_z: ScalarF32 optional,
        ambient: ScalarF32 optional,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: "light_x",
            label: "Light X",
            ty: ParamType::Float,
            default: ParamValue::Float(0.4),
            range: Some((-1.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "light_y",
            label: "Light Y",
            ty: ParamType::Float,
            default: ParamValue::Float(0.6),
            range: Some((-1.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "light_z",
            label: "Light Z",
            ty: ParamType::Float,
            default: ParamValue::Float(0.7),
            range: Some((-1.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "ambient",
            label: "Ambient",
            ty: ParamType::Float,
            default: ParamValue::Float(0.1),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
    ],
    composition_notes: "Light direction is normalised in-shader so any non-zero (x, y, z) works. Default light is over-the-shoulder camera (0.4, 0.6, 0.7). Port-shadowed components let you wire an LFO or `node.color_compass` to orbit the light at performance time.",
    examples: [],
    picker: { label: "Lambert (Directional)", category: Atom },
}

impl Primitive for LambertDirectional {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let read = |name: &str, default: f32| -> f32 {
            match ctx.inputs.scalar(name) {
                Some(ParamValue::Float(f)) => f,
                _ => match ctx.params.get(name) {
                    Some(ParamValue::Float(f)) => *f,
                    _ => default,
                },
            }
        };

        let light_x = read("light_x", 0.4);
        let light_y = read("light_y", 0.6);
        let light_z = read("light_z", 0.7);
        let ambient = read("ambient", 0.1);

        let Some(normal) = ctx.inputs.texture_2d("normal") else {
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
                include_str!("shaders/lambert_directional.wgsl"),
                "cs_main",
                "node.lambert_directional",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        let uniforms = LambertUniforms {
            light_x,
            light_y,
            light_z,
            _vec3_pad: 0.0,
            ambient,
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
                    texture: normal,
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
            "node.lambert_directional",
        );
    }
}
