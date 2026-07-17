//! `node.hdr_mix` — preserve a reference texture's
//! above-1.0 highlight energy through a compressed texture's gain
//! adjustment.
//!
//! Per pixel, splits each input's RGB into `sdr = min(rgb, 1.0)` and
//! `hdr = max(rgb - 1.0, 0.0)` portions. The result's SDR body comes
//! from `compressed`; its HDR portion lerps between `compressed`'s
//! HDR and `reference`'s HDR by `retention`. Output alpha is taken
//! from `compressed`.
//!
//! Use case: AutoGain's gain branch boosts/cuts everything uniformly,
//! including highlights — large `target` shifts push highlights
//! further into HDR (or pull them back), which downstream display
//! clipping reads as a hue shift. Wiring this atom between the gain
//! branch and the final composite with `retention = 1.0` keeps the
//! HDR ceiling pinned to the reference (the original source) while
//! the SDR body rides the compressor.
//!
//! At `retention = 0` the atom is a no-op pass-through of `compressed`.
//! At `retention = 1` the highlights stay anchored to `reference`'s
//! original level regardless of gain.

use std::borrow::Cow;

use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct HdrRetentionMixUniforms {
    retention: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

crate::primitive! {
    name: HdrRetentionMix,
    type_id: "node.hdr_mix",
    purpose: "Preserve a reference texture's above-1.0 highlight energy through a compressed texture's gain adjustment. Per-pixel: SDR body from `compressed`, HDR portion lerps between compressed's HDR and reference's HDR by `retention`. retention=1 keeps the HDR ceiling anchored to reference; retention=0 passes compressed through unchanged.",
    inputs: {
        compressed: Texture2D required,
        reference: Texture2D required,
        retention: ScalarF32 optional,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("retention"),
            label: "HDR Retention",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
    ],
    depth_rule: CombineNearest,
    composition_notes: "Pair with `node.exposure` for level-rider effects (AutoGain) where uniform RGB scaling would push highlights into / out of clip. Wire the gained branch into `compressed` and the un-gained source into `reference`. retention defaults to 1.0 (HDR ceiling pinned). The retention input is port-shadowable so the value can ride a control wire when needed.",
    examples: ["preset.effect.auto_gain"],
    picker: { label: "HDR Mix", category: Atom },
    summary: "Blends two images while keeping the bright above-white highlights from a reference, so a gain or grade doesn't crush the HDR detail. Reach for it when a process is flattening your highlights.",
    category: Composite,
    role: Filter,
    aliases: ["hdr mix", "hdr retention mix", "highlight retention", "hdr blend"],
    fusion_kind: MultiInputCoincident,
    wgsl_body: include_str!("shaders/hdr_retention_mix_body.wgsl"),
}

impl Primitive for HdrRetentionMix {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let retention = match ctx.inputs.scalar("retention") {
            Some(ParamValue::Float(f)) => f.clamp(0.0, 1.0),
            _ => match ctx.params.get("retention") {
                Some(ParamValue::Float(f)) => f.clamp(0.0, 1.0),
                _ => 1.0,
            },
        };

        let Some(compressed) = ctx.inputs.texture_2d("compressed") else {
            return;
        };
        let Some(reference) = ctx.inputs.texture_2d("reference") else {
            return;
        };
        let Some(out_tex) = ctx.outputs.texture_2d("out") else {
            return;
        };
        let (width, height) = (out_tex.width, out_tex.height);
        if width == 0 || height == 0 {
            return;
        }

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            let wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                .expect("node.hdr_mix standalone codegen");
            gpu.device.create_compute_pipeline(
                &wgsl,
                crate::node_graph::freeze::codegen::ENTRY,
                "node.hdr_mix",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        let uniforms = HdrRetentionMixUniforms {
            retention,
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
                    texture: compressed,
                },
                GpuBinding::Texture {
                    binding: 2,
                    texture: reference,
                },
                GpuBinding::Sampler {
                    binding: 3,
                    sampler,
                },
                GpuBinding::Texture {
                    binding: 4,
                    texture: out_tex,
                },
            ],
            [width.div_ceil(16), height.div_ceil(16), 1],
            "node.hdr_mix",
        );
    }
}
