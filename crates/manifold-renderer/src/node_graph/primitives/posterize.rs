//! `node.posterize` — quantize each RGB channel to `levels` discrete
//! steps (round to nearest level, endpoints included). Alpha passes
//! through. TouchDesigner / Blender posterize.
//!
//! The standalone quantize atom. `levels` is a port-shadowed scalar, so
//! a beat/LFO can crush the palette rhythmically. Dither composes from
//! this (quantize + an ordered-threshold pattern) rather than baking
//! both into one fused kernel.

use std::borrow::Cow;

use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct PosterizeUniforms {
    levels: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

crate::primitive! {
    name: Posterize,
    type_id: "node.posterize",
    purpose: "Posterize: quantize each RGB channel to `levels` discrete steps (round to nearest, endpoints included). Alpha pass-through. The standalone quantize atom (TD / Blender posterize) — `levels` is a port-shadowed scalar so a beat/LFO can crush the palette rhythmically. Dither composes from this.",
    inputs: {
        in: Texture2D required,
        levels: ScalarF32 optional,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("levels"),
            label: "Levels",
            ty: ParamType::Float,
            default: ParamValue::Float(8.0),
            range: Some((2.0, 32.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Inherit,
    composition_notes: "Quantizes to N levels as round(c * (N-1)) / (N-1) per channel — so the [0,1] endpoints are preserved and N=2 gives pure black/white per channel. `levels` floors to >= 2. The scalar input is the standard port-shadow: a connected wire wins over the inline param.",
    examples: ["preset.effect.dither"],
    picker: { label: "Posterize", category: Atom },
    summary: "Crushes each colour into a small number of steps for a banded, blocky look. Fewer levels give a chunkier result.",
    category: ColorAndTone,
    role: Filter,
    aliases: ["posterize", "quantize", "banding"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/posterize_body.wgsl"),
}

impl Primitive for Posterize {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let levels = match ctx.inputs.scalar("levels") {
            Some(ParamValue::Float(f)) => f,
            _ => match ctx.params.get("levels") {
                Some(ParamValue::Float(f)) => *f,
                _ => 8.0,
            },
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
            // Single-source: standalone kernel generated from the same
            // `wgsl_body` the fusion codegen chains. posterize.wgsl is retained
            // as the parity oracle.
            let wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                .expect("node.posterize standalone codegen");
            gpu.device.create_compute_pipeline(
                &wgsl,
                crate::node_graph::freeze::codegen::ENTRY,
                "node.posterize",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        let uniforms = PosterizeUniforms {
            levels: levels.max(2.0),
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
            "node.posterize",
        );
    }
}
