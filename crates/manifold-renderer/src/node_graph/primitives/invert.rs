//! `node.invert` — pixel-exact replacement for the legacy
//! Originally `InvertColorsFX`
//! effect. First production primitive authored via the
//! [`primitive!`](crate::primitive) macro and the first §6.1
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
use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

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
        let intensity = match ctx.params.get("intensity") {
            Some(ParamValue::Float(f)) => *f,
            _ => 1.0,
        };

        // Resolve input/output textures up front — the borrows survive
        // the encoder's mutable borrow below.
        let Some(in_tex) = ctx.inputs.texture_2d("in") else {
            return;
        };
        let Some(out_tex) = ctx.outputs.texture_2d("out") else {
            return;
        };
        let (width, height) = (out_tex.width, out_tex.height);

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            // Single-source: the standalone kernel is generated from the same
            // `wgsl_body` the fusion codegen chains, so the standalone and fused
            // paths can never drift. invert.wgsl is retained as the parity oracle.
            let wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                .expect("node.invert standalone codegen");
            gpu.device.create_compute_pipeline(
                &wgsl,
                crate::node_graph::freeze::codegen::ENTRY,
                "node.invert",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        let uniforms = InvertUniforms {
            intensity,
            _pad: [0.0; 3],
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
            "node.invert",
        );
    }
}
