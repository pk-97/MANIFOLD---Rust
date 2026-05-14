//! `node.color_grade` — pixel-exact replacement for legacy
//! [`ColorGradeFX`](crate::effects::color_grade::ColorGradeFX). Second
//! §6.1 migration.
//!
//! Single compute pass with 9 parameters covering gain (exposure),
//! saturation (luma-based), hue rotation (HSV), contrast (pivot
//! around 0.5), and a colorize pipeline (tint via HSV with
//! highlight/neutral masking). All math, constants, and bindings
//! are byte-identical to `effects/shaders/color_grade.wgsl`.
//!
//! See `docs/ADDING_PRIMITIVES.md` for the authoring template.

use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

crate::primitive! {
    name: ColorGrade,
    type_id: "node.color_grade",
    purpose: "Color grading: gain, saturation (luma-based), hue rotation (HSV), contrast (pivot 0.5), and a tinted-colorize pipeline with highlight/neutral masking.",
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
            label: "Gain",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 2.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "saturation",
            label: "Saturation",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 2.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "hue",
            label: "Hue",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-180.0, 180.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "contrast",
            label: "Contrast",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 2.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "colorize",
            label: "Colorize",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "colorize_hue",
            label: "Tint Hue",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 360.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "colorize_saturation",
            label: "Tint Saturation",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 2.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "colorize_focus",
            label: "Colorize Focus",
            ty: ParamType::Float,
            default: ParamValue::Float(0.75),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
    ],
    composition_notes: "1:1 replacement for legacy ColorGrade. Chain after spatial effects, before bloom/halation. The colorize section is gated internally — values < 1e-4 short-circuit and pass through the graded image directly.",
    examples: ["preset.effect.color_grade"],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ColorGradeUniforms {
    amount: f32,
    gain: f32,
    saturation: f32,
    hue: f32,
    contrast: f32,
    colorize: f32,
    colorize_hue: f32,
    colorize_saturation: f32,
    colorize_focus: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

impl Primitive for ColorGrade {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let amount = read_f32(ctx, "amount", 0.0);
        let gain = read_f32(ctx, "gain", 1.0);
        let saturation = read_f32(ctx, "saturation", 1.0);
        let hue = read_f32(ctx, "hue", 0.0);
        let contrast = read_f32(ctx, "contrast", 1.0);
        let colorize = read_f32(ctx, "colorize", 0.0);
        let colorize_hue = read_f32(ctx, "colorize_hue", 0.0);
        let colorize_saturation = read_f32(ctx, "colorize_saturation", 1.0);
        let colorize_focus = read_f32(ctx, "colorize_focus", 0.75);

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
                include_str!("shaders/color_grade.wgsl"),
                "cs_main",
                "node.color_grade",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        let uniforms = ColorGradeUniforms {
            amount,
            gain,
            saturation,
            hue,
            contrast,
            colorize,
            colorize_hue,
            colorize_saturation,
            colorize_focus,
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
            "node.color_grade",
        );
    }
}

/// Read a `Float` param by name, falling back to a default. Multi-param
/// primitives accumulate enough call sites that the closure
/// boilerplate becomes noisy without a helper.
fn read_f32(ctx: &EffectNodeContext<'_, '_>, name: &str, default: f32) -> f32 {
    match ctx.params.get(name) {
        Some(ParamValue::Float(f)) => *f,
        _ => default,
    }
}
