//! `node.chromatic_aberration` — pixel-exact replacement for legacy
//! [`ChromaticAberrationFX`](crate::effects::chromatic_aberration::ChromaticAberrationFX).
//! Sixth §6.1 migration.
//!
//! Splits the red and blue channels along a direction vector and
//! resamples; green stays at the unshifted UV. Two modes:
//!
//! - **Radial**: offset direction is `normalize(uv - center) ×
//!   smoothstep(0, 0.707, dist)` faded by `1 - falloff`.
//! - **Linear**: offset direction is `(cos(angle), sin(angle))`,
//!   uniform across the image.
//!
//! Per `PRIMITIVE_LIBRARY_DESIGN.md` §2.5, listed as a
//! Color/Distortion atomic — its math is one pass and doesn't need
//! the fused-composite treatment.

use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

crate::primitive! {
    name: ChromaticOffset,
    type_id: "node.chromatic_aberration",
    purpose: "Radial or linear RGB channel separation: red and blue channels shift along an offset vector; green stays unshifted.",
    inputs: {
        in: Texture2D required,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: "amount",
            label: "Amount",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "offset",
            label: "Offset",
            ty: ParamType::Float,
            default: ParamValue::Float(0.01),
            range: Some((0.0, 0.05)),
            enum_values: &[],
        },
        ParamDef {
            name: "mode",
            label: "Mode",
            ty: ParamType::Enum,
            default: ParamValue::Enum(0),
            range: Some((0.0, 1.0)),
            enum_values: &["Radial", "Linear"],
        },
        ParamDef {
            name: "angle",
            label: "Angle (deg)",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 360.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "falloff",
            label: "Falloff",
            ty: ParamType::Float,
            default: ParamValue::Float(0.5),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
    ],
    composition_notes: "1:1 replacement for legacy ChromaticAberration. Also used inside the Glitch fused composite for RGB-shift jitter.",
    examples: ["preset.effect.chromatic_aberration"],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ChromaticOffsetUniforms {
    amount: f32,
    mode: u32,
    angle: f32,
    falloff: f32,
    offset: f32,
    _pad: [f32; 3],
}

impl Primitive for ChromaticOffset {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let amount = read_f32(ctx, "amount", 0.0);
        let offset = read_f32(ctx, "offset", 0.01);
        let mode = match ctx.params.get("mode") {
            Some(ParamValue::Enum(v)) => (*v).min(1),
            Some(ParamValue::Float(f)) => (f.round() as u32).min(1),
            _ => 0,
        };
        let angle = read_f32(ctx, "angle", 0.0);
        let falloff = read_f32(ctx, "falloff", 0.5);

        let Some(in_tex) = ctx.inputs.texture_2d("in") else {
            return;
        };
        let Some(out_tex) = ctx.outputs.texture_2d("out") else {
            return;
        };
        let (width, height) = (out_tex.width, out_tex.height);

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                include_str!("shaders/chromatic_offset.wgsl"),
                "cs_main",
                "node.chromatic_aberration",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        let uniforms = ChromaticOffsetUniforms {
            amount,
            mode,
            angle,
            falloff,
            offset,
            _pad: [0.0; 3],
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
                    texture: in_tex,
                },
                GpuBinding::Sampler {
                    binding: 2,
                    sampler,
                },
                GpuBinding::Texture {
                    binding: 3,
                    texture: out_tex,
                },
            ],
            [width.div_ceil(16), height.div_ceil(16), 1],
            "node.chromatic_aberration",
        );
    }
}

fn read_f32(ctx: &EffectNodeContext<'_, '_>, name: &str, default: f32) -> f32 {
    match ctx.params.get(name) {
        Some(ParamValue::Float(f)) => *f,
        _ => default,
    }
}
