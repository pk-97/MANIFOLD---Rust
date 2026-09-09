//! `node.hue_saturation` — HSV colour adjust: rotate hue, scale
//! saturation, scale value. RGB → HSV → adjust → RGB. TouchDesigner's
//! HSV Adjust / Blender's Hue-Saturation-Value.
//!
//! The standalone colour-rotation atom. `hue` is in degrees;
//! `saturation` and `value` are multipliers (1.0 = unchanged). All
//! three are port-shadowed scalars, so an LFO / MIDI / Color-Compass /
//! driver wire sweeps them live. Color Grade composes from this rather
//! than baking hue/saturation into one fused kernel.

use std::borrow::Cow;

use manifold_gpu::GpuSamplerDesc;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;
use super::standalone_pipeline::{dispatch_standalone_2d, standalone_pipeline};

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct HueSaturationUniforms {
    hue_degrees: f32,
    saturation: f32,
    value: f32,
    _pad0: f32,
}

crate::primitive! {
    name: HueSaturation,
    type_id: "node.hue_saturation",
    purpose: "HSV colour adjust: rotate hue (degrees), scale saturation, scale value. RGB→HSV→adjust→RGB (TD HSV Adjust / Blender Hue-Saturation-Value). The standalone colour-rotation atom — saturation/value are multipliers (1.0 = unchanged). All three are port-shadowed scalars: wire an LFO / MIDI / driver to sweep hue live. Color Grade composes from this.",
    inputs: {
        in: Texture2D required,
        hue: ScalarF32 optional,
        saturation: ScalarF32 optional,
        value: ScalarF32 optional,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("hue"),
            label: "Hue (deg)",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-180.0, 180.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("saturation"),
            label: "Saturation",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 2.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("value"),
            label: "Value",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 2.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Inherit,
    composition_notes: "Hue rotation wraps (fract on the [0,1] hue ring), so any degree value is valid. Saturation clamps to [0,1] in HSV space after scaling; value is an unclamped multiplier (HDR-safe). Alpha passes through. Each scalar input is the standard port-shadow: a connected wire wins over the inline param.",
    examples: ["preset.effect.color_grade"],
    picker: { label: "Hue / Saturation", category: Atom },
    summary: "Spins the hue around the colour wheel and adjusts how vivid and bright the image is. The HSV way to recolour.",
    category: ColorAndTone,
    role: Filter,
    aliases: ["hue", "saturation", "hsv", "recolour", "HSV Adjust TOP", "Hue Saturation Value"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/hue_saturation_body.wgsl"),
}

impl Primitive for HueSaturation {
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
        let uniforms = HueSaturationUniforms {
            hue_degrees: scalar("hue", 0.0),
            saturation: scalar("saturation", 1.0),
            value: scalar("value", 1.0),
            _pad0: 0.0,
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

        dispatch_standalone_2d(
            gpu,
            pipeline,
            bytemuck::bytes_of(&uniforms),
            &[in_tex],
            Some(sampler),
            out_tex,
            "node.hue_saturation",
        );
    }
}
