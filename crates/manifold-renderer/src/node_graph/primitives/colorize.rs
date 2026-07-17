//! `node.colorize` — tint an image toward a chosen hue, with the tint
//! strength masked per-pixel by (brightness × neutrality × focus). A
//! selective duotone/colorize toward highlights: bright, already-neutral
//! pixels take the tint; dark or already-saturated pixels resist it.
//!
//! Verbatim port of the ColorGrade colorize pass (the highlight/neutral
//! masking + tinted blend from `effects/shaders/color_grade.wgsl`), so
//! it can stand in for that section in a decomposed graph and also be
//! reused standalone for "push the highlights toward teal" looks.

use std::borrow::Cow;

use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ColorizeUniforms {
    amount: f32,
    hue: f32,
    saturation: f32,
    focus: f32,
}

crate::primitive! {
    name: Colorize,
    type_id: "node.colorize",
    purpose: "Tint an image toward a hue, masked per-pixel by (brightness × neutrality × focus): a selective colorize/duotone toward highlights. Bright neutral pixels take the tint; dark or already-saturated pixels resist it. `amount` is colorize strength [0,1], `hue` the tint hue (deg), `saturation` the tint saturation, `focus` how tightly the mask favours highlights/neutrals (0 = tint everything). All four port-shadow their params for live modulation.",
    inputs: {
        in: Texture2D required,
        amount: ScalarF32 optional,
        hue: ScalarF32 optional,
        saturation: ScalarF32 optional,
        focus: ScalarF32 optional,
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
            name: Cow::Borrowed("hue"),
            label: "Tint Hue",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 360.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("saturation"),
            label: "Tint Saturation",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 2.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("focus"),
            label: "Focus",
            ty: ParamType::Float,
            default: ParamValue::Float(0.75),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Inherit,
    composition_notes: "tinted = tint_rgb * graded_luma; out = mix(c, tinted, amount * element_mask), where element_mask = mix(1, smoothstep(0.18,0.95,luma) * (1 - smoothstep(0.10,0.80,sat)), focus). At amount=0 it passes through unchanged. Place after the tonal grade (gain/saturation/hue/contrast) so the masks read graded luma/saturation, matching legacy Color Grade ordering. Each input port shadows its param.",
    examples: ["preset.effect.color_grade"],
    picker: { label: "Colorize", category: Atom },
    summary: "Tints the image toward a single colour, strongest on the bright neutral areas. Good for duotones and washes.",
    category: ColorAndTone,
    role: Filter,
    aliases: ["colorize", "tint", "duotone"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/colorize_body.wgsl"),
}

impl Primitive for Colorize {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let scalar = |name: &str, default: f32| -> f32 {
            match ctx.inputs.scalar(name) {
                Some(ParamValue::Float(f)) => f,
                _ => match ctx.params.get(name) {
                    Some(ParamValue::Float(f)) => *f,
                    _ => default,
                },
            }
        };
        let uniforms = ColorizeUniforms {
            amount: scalar("amount", 0.0),
            hue: scalar("hue", 0.0),
            saturation: scalar("saturation", 1.0),
            focus: scalar("focus", 0.75),
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
            let wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                .expect("node.colorize standalone codegen");
            gpu.device.create_compute_pipeline(
                &wgsl,
                crate::node_graph::freeze::codegen::ENTRY,
                "node.colorize",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

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
            "node.colorize",
        );
    }
}
