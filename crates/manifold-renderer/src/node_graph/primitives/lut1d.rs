//! `node.color_lut` — pixel-exact replacement for legacy
//! [`InfraredFX`](crate::effects::infrared::InfraredFX). Eighth §6.1
//! migration and the first multi-input primitive in the trivial
//! phase.
//!
//! Looks up luminance (BT.601 weights) into a 1D LUT (stored as a
//! W×1 texture, typically Rgba16Float). Two parameters: `amount`
//! (crossfade against source) and `contrast` (luminance pivot at
//! 0.5). The LUT itself is supplied as a port — the Infrared preset
//! graph owns the 10 baked palette textures and routes the active
//! one based on the palette selector.
//!
//! `lum * 0.5` in the WGSL bakes the legacy `LUT_MAX_LUM = 2.0`
//! range. Other consumers that want a different range should write
//! their own preset wrapper that rescales luminance ahead of LUT1D.

use std::borrow::Cow;
use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

crate::primitive! {
    name: ColorLut,
    type_id: "node.color_lut",
    purpose: "1D LUT remap: sample a W×1 LUT texture indexed by BT.601 luminance (with contrast adjust), then crossfade against the source. The Infrared preset graph supplies the LUT.",
    inputs: {
        in: Texture2D required,
        lut: Texture2D required,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("amount"),
            label: "Amount",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("contrast"),
            label: "Contrast",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.5, 3.0)),
            enum_values: &[],
        },
    ],
    // depth_rule: `lut` is indexed by `in`'s own luminance (a data-dependent 1D lookup), not sampled at a coincident spatial UV — depth follows `in` only, not a CombineNearest peer
    depth_rule: Inherit,
    composition_notes: "1:1 building block for the legacy Infrared effect. The lut input is a W×1 Rgba16Float texture covering luminance [0, 2.0]; the Infrared preset graph supplies pre-baked palette textures.",
    examples: ["preset.effect.infrared"],
    picker: { label: "Color LUT", category: Atom },
    summary: "Remaps the image through a lookup-table strip indexed by brightness, the engine behind heat-map and infrared palettes.",
    category: ColorAndTone,
    role: Filter,
    aliases: ["lut", "lookup", "palette", "Lookup TOP"],
    pure: true,
    fusion_kind: MultiInputCoincident,
    wgsl_body: include_str!("shaders/lut1d_body.wgsl"),
    input_access: [Coincident, Gather],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Lut1dUniforms {
    amount: f32,
    contrast: f32,
    _pad0: f32,
    _pad1: f32,
}

impl Primitive for ColorLut {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let amount = match ctx.params.get("amount") {
            Some(ParamValue::Float(f)) => *f,
            _ => 0.0,
        };
        let contrast = match ctx.params.get("contrast") {
            Some(ParamValue::Float(f)) => *f,
            _ => 1.0,
        };

        let Some(in_tex) = ctx.inputs.texture_2d("in") else {
            return;
        };
        let Some(lut_tex) = ctx.inputs.texture_2d("lut") else {
            return;
        };
        let Some(out_tex) = ctx.outputs.texture_2d("out") else {
            return;
        };
        let (width, height) = (out_tex.width, out_tex.height);

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            // `in` is coincident (centre sample); `lut` is a Gather input sampled
            // at a luminance-indexed 1D coord, so the body receives it as a
            // texture+sampler arg. Generated kernel binds uniform(0)/in(1)/lut(2)/
            // samp(3)/dst(4), matching the set below. lut1d.wgsl is the oracle.
            gpu.device.create_compute_pipeline(
                &crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                    .expect("node.color_lut standalone codegen"),
                crate::node_graph::freeze::codegen::ENTRY,
                "node.color_lut",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        let uniforms = Lut1dUniforms {
            amount,
            contrast,
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
                GpuBinding::Texture {
                    binding: 2,
                    texture: lut_tex,
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
            "node.color_lut",
        );
    }
}
