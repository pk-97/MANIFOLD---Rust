//! `node.contrast` — pivot-around-0.5 contrast:
//! `out = (c - 0.5) * contrast + 0.5`. 1.0 = unchanged, >1 = punchier,
//! <1 = flatter, 0 = flat grey. Alpha passes through.
//!
//! HDR-safe: no clamp, and — unlike `node.levels`, whose gamma `pow`
//! NaNs on negative inputs — the contrast pivot is pure affine, so it
//! handles the negatives a contrast push can produce on dark pixels.
//! Clamp downstream (`node.clamp`) if a consumer needs bounded
//! input. This is the contrast stage of a decomposed Color Grade.

use std::borrow::Cow;

use manifold_gpu::GpuSamplerDesc;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;
use super::standalone_pipeline::{dispatch_standalone_2d, standalone_pipeline};

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ContrastUniforms {
    contrast: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

crate::primitive! {
    name: Contrast,
    type_id: "node.contrast",
    purpose: "Pivot-around-0.5 contrast: out = (c - 0.5) * contrast + 0.5. 1.0 = unchanged, >1 = punchier, <1 = flatter. Alpha passes through. The `contrast` input port shadows the param for live modulation. HDR-safe affine — use this rather than node.levels for contrast (levels' gamma pow NaNs on the negatives a contrast push produces).",
    inputs: {
        in: Texture2D required,
        contrast: ScalarF32 optional,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("contrast"),
            label: "Contrast",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 2.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Inherit,
    composition_notes: "Pure affine around the 0.5 pivot — no clamp, so dark pixels can go negative at contrast > 1. Follow with node.clamp (min 0) before consumers that need bounded input; Color Grade does exactly this at the end of its chain. Wire wins over param.",
    examples: ["preset.effect.color_grade"],
    picker: { label: "Contrast", category: Atom },
    summary: "Pushes the lights and darks apart for a punchier image, or pulls them together for a flatter one. It pivots around mid grey.",
    category: ColorAndTone,
    role: Filter,
    aliases: ["contrast", "Level TOP", "Bright/Contrast"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/contrast_body.wgsl"),
}

impl Primitive for Contrast {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let contrast = match ctx.inputs.scalar("contrast") {
            Some(ParamValue::Float(f)) => f,
            _ => match ctx.params.get("contrast") {
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

        let uniforms = ContrastUniforms {
            contrast,
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
            "node.contrast",
        );
    }
}
