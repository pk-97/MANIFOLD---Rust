//! `primitive.kaleido_fold` — pixel-exact replacement for legacy
//! [`KaleidoscopeFX`](crate::effects::kaleidoscope::KaleidoscopeFX).
//! Fourth §6.1 migration.
//!
//! Polar-coordinate segment mirroring: slices the UV plane into N
//! wedges around the center, mirrors alternating wedges for seamless
//! reflection. The legacy `segments` parameter is `f32` clamped to
//! `>= 2` on the CPU; the primitive preserves that exact behavior.

use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

crate::primitive! {
    name: KaleidoFold,
    type_id: "primitive.kaleido_fold",
    purpose: "Polar-coordinate segment mirroring. Slices the UV plane into N wedges around the center; alternating wedges reflect their sample for a seamless kaleidoscope.",
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
            name: "segments",
            label: "Segments",
            ty: ParamType::Float,
            default: ParamValue::Float(6.0),
            range: Some((2.0, 16.0)),
            enum_values: &[],
        },
    ],
    composition_notes: "1:1 replacement for legacy Kaleidoscope. QuadMirror is a distinct primitive (axis-aligned XY fold, no polar conversion); don't substitute one for the other.",
    examples: ["preset.effect.kaleidoscope"],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct KaleidoFoldUniforms {
    amount: f32,
    segments: f32,
    _pad0: f32,
    _pad1: f32,
}

impl Primitive for KaleidoFold {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let amount = match ctx.params.get("amount") {
            Some(ParamValue::Float(f)) => *f,
            _ => 0.0,
        };
        // Legacy floors to >= 2 on the CPU before uniform pack. Match
        // exactly — otherwise segments < 2 produces a TAU / N division
        // that diverges between paths.
        let segments = match ctx.params.get("segments") {
            Some(ParamValue::Float(f)) => f.max(2.0),
            _ => 6.0,
        };

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
                include_str!("shaders/kaleido_fold.wgsl"),
                "cs_main",
                "primitive.kaleido_fold",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        let uniforms = KaleidoFoldUniforms {
            amount,
            segments,
            _pad0: 0.0,
            _pad1: 0.0,
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
            "primitive.kaleido_fold",
        );
    }
}
