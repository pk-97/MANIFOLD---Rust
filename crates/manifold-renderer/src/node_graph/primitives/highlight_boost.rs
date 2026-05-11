//! `primitive.highlight_boost` — pixel-exact replacement for legacy
//! [`HdrBoostFX`](crate::effects::hdr_boost::HdrBoostFX). Ninth §6.1
//! migration.
//!
//! Soft-knee threshold selects bright areas; the luminance excess
//! above the threshold is boosted by `pow(2, gain) - 1` EV stops,
//! preserving color ratios. `amount` crossfades the boosted result
//! against the source; the final color is clamped to non-negative.
//!
//! Renamed from the design doc's tentative `Threshold` primitive:
//! HDRBoost adds boost to the source rather than dropping
//! sub-threshold pixels, so it's a different operation from Bloom's
//! prefilter math (which also uses threshold+knee but with a
//! response curve that *extracts* highlights for downstream blur).
//! Bloom's prefilter will become its own primitive when §6.3 lands
//! `SeparableGaussian` + `MipChain` + the Bloom preset graph.

use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

crate::primitive! {
    name: HighlightBoost,
    type_id: "primitive.highlight_boost",
    purpose: "Boost luminance excess above a soft-knee threshold by `pow(2, gain) - 1` EV stops; preserves color ratios via per-pixel luma scaling. Final result is clamped to non-negative.",
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
            name: "gain",
            label: "Gain (EV)",
            ty: ParamType::Float,
            default: ParamValue::Float(1.5),
            range: Some((0.0, 5.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "threshold",
            label: "Threshold",
            ty: ParamType::Float,
            default: ParamValue::Float(0.15),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "knee",
            label: "Knee",
            ty: ParamType::Float,
            default: ParamValue::Float(0.3),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
    ],
    composition_notes: "1:1 replacement for legacy HDRBoost. Distinct from a future BloomPrefilter primitive: this BOOSTS color above threshold, BloomPrefilter EXTRACTS bright pixels for downstream blur.",
    examples: ["preset.effect.hdr_boost"],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct HighlightBoostUniforms {
    amount: f32,
    gain: f32,
    threshold: f32,
    knee: f32,
}

impl Primitive for HighlightBoost {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let amount = read_f32(ctx, "amount", 0.0);
        let gain = read_f32(ctx, "gain", 1.5);
        let threshold = read_f32(ctx, "threshold", 0.15);
        let knee = read_f32(ctx, "knee", 0.3);

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
                include_str!("shaders/highlight_boost.wgsl"),
                "cs_main",
                "primitive.highlight_boost",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        let uniforms = HighlightBoostUniforms {
            amount,
            gain,
            threshold,
            knee,
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
            "primitive.highlight_boost",
        );
    }
}

fn read_f32(ctx: &EffectNodeContext<'_, '_>, name: &str, default: f32) -> f32 {
    match ctx.params.get(name) {
        Some(ParamValue::Float(f)) => *f,
        _ => default,
    }
}
