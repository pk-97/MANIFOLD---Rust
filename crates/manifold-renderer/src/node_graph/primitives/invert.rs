//! `node.invert` — pixel-exact replacement for the legacy
//! Originally `InvertColorsFX`
//! effect. First production primitive authored via the
//! [`primitive!`](crate::primitive) macro and the first section 6.1
//! migration from the Phase 4a primitive library design.
//!
//! Math: `mix(source, vec4(1-r, 1-g, 1-b, a), intensity)`. Alpha is
//! intentionally preserved by the inverted vector — see the legacy
//! shader at `effects/shaders/invert_colors.wgsl` for the source of
//! truth.
//!
//! See `docs/ADDING_PRIMITIVES.md` for the authoring template this
//! primitive follows.

use std::borrow::Cow;
use manifold_gpu::GpuSamplerDesc;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;
use super::standalone_pipeline::{dispatch_standalone_2d, standalone_pipeline};

crate::primitive! {
    name: Invert,
    type_id: "node.invert",
    purpose: "Inverts RGB channels and blends against the source by intensity. Alpha is preserved.",
    inputs: {
        in: Texture2D required,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("intensity"),
            label: "Intensity",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Inherit,
    composition_notes: "1:1 replacement for the legacy InvertColorsFX effect. Use Invert alone for a single-pass invert; chain with ColorGradeHSV or Threshold for analog-style processing pipelines.",
    examples: ["preset.effect.invert"],
    picker: { label: "Invert", category: Atom },
    summary: "Flips every colour to its opposite, turning a negative of the image. Blend it part-way for a partial invert.",
    category: ColorAndTone,
    role: Filter,
    aliases: ["invert", "negative", "Invert TOP"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/invert_body.wgsl"),
}

/// Uniform shape mirrored from the legacy shader. 16-byte aligned via
/// 3-element f32 padding. `#[repr(C)]` + `bytemuck::Pod` lets us emit
/// the bytes directly to the WGSL binding.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct InvertUniforms {
    intensity: f32,
    _pad: [f32; 3],
}

impl Primitive for Invert {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let intensity = ctx.param_f32("intensity", 1.0);

        // Resolve input/output textures up front — the borrows survive
        // the encoder's mutable borrow below.
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

        let uniforms = InvertUniforms {
            intensity,
            _pad: [0.0; 3],
        };

        dispatch_standalone_2d(
            gpu,
            pipeline,
            bytemuck::bytes_of(&uniforms),
            &[in_tex],
            Some(sampler),
            out_tex,
            "node.invert",
        );
    }
}
