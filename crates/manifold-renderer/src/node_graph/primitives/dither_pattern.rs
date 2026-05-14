//! `node.dither` — pixel-exact replacement for legacy
//! [`DitherFX`](crate::effects::dither::DitherFX). Seventh §6.1
//! migration.
//!
//! 6 dithering algorithms (Bayer 8×8, Halftone dots, Lines,
//! CrossHatch, Blue Noise, Diamond) with luminance-preserving
//! quantization and an `amount`-controlled crossfade against the
//! source. Math, constants (cell sizes, line widths, Jimenez noise
//! coefficients), and dispatch shape preserved verbatim.

use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

crate::primitive! {
    name: DitherPattern,
    type_id: "node.dither",
    purpose: "Dithering: quantizes luminance to 8→2 levels based on a per-pixel threshold pattern (Bayer, Halftone, Lines, CrossHatch, Noise, Diamond). Hue is preserved via per-pixel luma scaling.",
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
            name: "algorithm",
            label: "Algorithm",
            ty: ParamType::Enum,
            default: ParamValue::Enum(0),
            range: Some((0.0, 5.0)),
            enum_values: &["Bayer", "Halftone", "Lines", "X-Hatch", "Noise", "Diamond"],
        },
    ],
    composition_notes: "1:1 replacement for legacy Dither. Pattern density scales with the output texture's pixel dimensions (intrinsic), so it stays size-coherent across render-scale changes.",
    examples: ["preset.effect.dither"],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct DitherPatternUniforms {
    amount: f32,
    algorithm: u32,
    resolution_x: f32,
    resolution_y: f32,
}

impl Primitive for DitherPattern {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let amount = match ctx.params.get("amount") {
            Some(ParamValue::Float(f)) => *f,
            _ => 0.0,
        };
        let algorithm = match ctx.params.get("algorithm") {
            Some(ParamValue::Enum(v)) => (*v).min(5),
            Some(ParamValue::Float(f)) => (f.round() as u32).min(5),
            _ => 0,
        };

        let Some(in_tex) = ctx.inputs.texture_2d("in") else {
            return;
        };
        let Some(out_tex) = ctx.outputs.texture_2d("out") else {
            return;
        };
        let (width, height) = (out_tex.width, out_tex.height);
        // Pattern density is intrinsic to the output buffer: same
        // value the legacy effect passes via `ctx.output_width/height`
        // when render res == output res (the common case and the
        // 128×128 parity-test case bit-for-bit).
        let resolution_x = width as f32;
        let resolution_y = height as f32;

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                include_str!("shaders/dither_pattern.wgsl"),
                "cs_main",
                "node.dither",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        let uniforms = DitherPatternUniforms {
            amount,
            algorithm,
            resolution_x,
            resolution_y,
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
            "node.dither",
        );
    }
}
