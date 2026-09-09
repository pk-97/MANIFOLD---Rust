//! `node.saturation` — luma-based saturation. Lerp each pixel between
//! its Rec. 709 luma grayscale and its original colour by `saturation`:
//! `out = mix(vec3(luma), c, saturation)`. 0 = grayscale, 1 = unchanged,
//! >1 = oversaturated. Alpha passes through.
//!
//! Distinct from `node.hue_saturation`, which scales saturation in HSV
//! space (preserving HSV value). Luma-based desaturation pulls toward
//! perceptual grey and is what Color Grade's saturation control uses —
//! the two give visibly different mid-tones, so both are first-class
//! atoms (TouchDesigner ships both: Level TOP saturation vs HSV Adjust).

use std::borrow::Cow;

use manifold_gpu::GpuSamplerDesc;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;
use super::standalone_pipeline::{dispatch_standalone_2d, standalone_pipeline};

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SaturationUniforms {
    saturation: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

crate::primitive! {
    name: Saturation,
    type_id: "node.saturation",
    purpose: "Luma-based saturation: out = mix(vec3(rec709_luma), c, saturation). 0 = grayscale, 1 = unchanged, >1 = oversaturated. Alpha passes through. The `saturation` input port shadows the param — wire any scalar source (LFO, audio bridge) to pump saturation live. Distinct from node.hue_saturation (HSV-space saturation); this pulls toward perceptual grey, the look Color Grade uses.",
    inputs: {
        in: Texture2D required,
        saturation: ScalarF32 optional,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("saturation"),
            label: "Saturation",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 2.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Inherit,
    composition_notes: "Rec. 709 weights (0.2126, 0.7152, 0.0722) — same as node.luminance. Wire wins over param. HDR-safe (no clamp). Pair with node.exposure (exposure) + node.contrast + node.hue_saturation to rebuild a full colour-grade chain from atoms.",
    examples: ["preset.effect.color_grade"],
    picker: { label: "Saturation", category: Atom },
    summary: "Pulls colours toward grey or pushes them more vivid.",
    category: ColorAndTone,
    role: Filter,
    aliases: ["vibrance", "desaturate", "Level TOP"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/saturation_body.wgsl"),
}

impl Primitive for Saturation {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let saturation = match ctx.inputs.scalar("saturation") {
            Some(ParamValue::Float(f)) => f,
            _ => match ctx.params.get("saturation") {
                Some(ParamValue::Float(f)) => *f,
                _ => 1.0,
            },
        };

        let Some(in_tex) = ctx.inputs.texture_2d("in") else {
            return;
        };
        let Some(out_tex) = ctx.outputs.texture_2d("out") else {
            return;
        };

        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        let uniforms = SaturationUniforms {
            saturation,
            _pad0: 0.0,
            _pad1: 0.0,
            _pad2: 0.0,
        };

        dispatch_standalone_2d(
            gpu,
            pipeline,
            bytemuck::bytes_of(&uniforms),
            &[in_tex],
            Some(sampler),
            out_tex,
            "node.saturation",
        );
    }
}
