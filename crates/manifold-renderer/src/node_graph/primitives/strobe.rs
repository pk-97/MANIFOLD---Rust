//! `primitive.strobe` — pixel-exact replacement for legacy
//! [`StrobeFX`](crate::effects::strobe::StrobeFX). Eleventh §6.1
//! migration; fused composite.
//!
//! Beat-synced square wave flash with three modes (Opacity → black,
//! White → white, Gain → 3× boost when on). The legacy effect
//! exposes a "rate" slider that indexes into a hardcoded
//! `NOTE_RATES` table of strobes-per-beat; the primitive accepts the
//! resolved float rate directly so it's reusable for non-musical
//! strobing (the preset graph that replaces StrobeFX supplies the
//! NOTE_RATES table indexing at its parameter boundary).

use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Strobes-per-beat lookup table. Mirrors the legacy
/// `StrobeFX::NOTE_RATES`. Exposed so the Strobe preset graph and
/// parity tests can share the canonical values.
pub const NOTE_RATES: [f32; 10] = [0.25, 0.5, 1.0, 1.5, 2.0, 3.0, 4.0, 6.0, 8.0, 16.0];

crate::primitive! {
    name: Strobe,
    type_id: "primitive.strobe",
    purpose: "Beat-synced square wave flash. Three modes: Opacity (flash to black), White (flash to white), Gain (3× brightness when on). Rate is strobes-per-beat.",
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
            name: "rate",
            label: "Rate (strobes/beat)",
            ty: ParamType::Float,
            default: ParamValue::Float(4.0),
            range: Some((0.0, 16.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "mode",
            label: "Mode",
            ty: ParamType::Enum,
            default: ParamValue::Enum(0),
            range: Some((0.0, 2.0)),
            enum_values: &["Opacity", "White", "Gain"],
        },
        ParamDef {
            name: "beat",
            label: "Beat",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 1e9)),
            enum_values: &[],
        },
    ],
    composition_notes: "Fused composite — atomic BeatGate + Mix would round through fp16 between passes and break parity. NOTE_RATES is the canonical Manifold note-rate table; the Strobe preset graph indexes it via the legacy rate slider.",
    examples: ["preset.effect.strobe"],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct StrobeUniforms {
    amount: f32,
    rate: f32,
    mode: u32,
    beat: f32,
}

impl Primitive for Strobe {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let amount = read_f32(ctx, "amount", 0.0);
        let rate = read_f32(ctx, "rate", 4.0);
        let mode = match ctx.params.get("mode") {
            Some(ParamValue::Enum(v)) => (*v).min(2),
            Some(ParamValue::Float(f)) => (f.round() as u32).min(2),
            _ => 0,
        };
        let beat = read_f32(ctx, "beat", 0.0);

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
                include_str!("shaders/strobe.wgsl"),
                "cs_main",
                "primitive.strobe",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        let uniforms = StrobeUniforms {
            amount,
            rate,
            mode,
            beat,
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
            "primitive.strobe",
        );
    }
}

fn read_f32(ctx: &EffectNodeContext<'_, '_>, name: &str, default: f32) -> f32 {
    match ctx.params.get(name) {
        Some(ParamValue::Float(f)) => *f,
        _ => default,
    }
}
