//! `node.dither` — luminance-preserving dither quantize, driven by an external
//! threshold pattern.
//!
//! Quantizes Rec.709 luminance to 8→2 levels as `amount` goes 0→1, dither-biased
//! by the `pattern` input's R channel, preserves hue (scales the original colour
//! by the dithered/original luma ratio), and crossfades against the source by
//! `amount`. Math is verbatim from the legacy fused dither — the only change is
//! that the threshold comes from the `pattern` input instead of being computed
//! inline, so the quantizer composes with `node.dither_pattern` (the six classic
//! patterns) OR any BYO threshold texture (blue noise, a custom ramp, voronoi).

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct DitherUniforms {
    amount: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

crate::primitive! {
    name: Dither,
    type_id: "node.dither",
    purpose: "Luminance-preserving dither quantize driven by an external threshold pattern. Quantizes Rec.709 luma to 8->2 levels (by `amount`), dither-biased by the `pattern` input's R channel, preserves hue, and crossfades against the source. Pair with node.dither_pattern for the classic six patterns, or feed any threshold texture.",
    inputs: {
        in: Texture2D required,
        pattern: Texture2D required,
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
    ],
    // depth_rule: reads `pattern` as a fixed threshold texture, not a second content stream — depth follows `in` only, not a real CombineNearest peer
    depth_rule: Inherit,
    composition_notes: "`pattern` and `in` must share dimensions (the shader reads both via textureLoad at the same pixel). `amount` drives both the quantization level count (8 at 0 -> 2 at 1) and the final crossfade, matching the legacy fused dither. Split out of the old monolithic dither so the pattern is a reusable atom.",
    examples: ["preset.effect.dither"],
    picker: { label: "Dither", category: Atom },
    summary: "Reduces the image to a few brightness levels and hides the banding with a fine noise pattern. The classic low-bit look.",
    category: ColorAndTone,
    role: Filter,
    aliases: ["dither", "bayer", "ordered dither"],
    fusion_kind: MultiInputCoincident,
    wgsl_body: include_str!("shaders/dither_body.wgsl"),
    input_access: [CoincidentTexel, CoincidentTexel],
}

impl Primitive for Dither {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let amount = match ctx.params.get("amount") {
            Some(ParamValue::Float(f)) => *f,
            _ => 0.0,
        };

        let Some(in_tex) = ctx.inputs.texture_2d("in") else {
            return;
        };
        let Some(pattern_tex) = ctx.inputs.texture_2d("pattern") else {
            return;
        };
        let Some(out_tex) = ctx.outputs.texture_2d("out") else {
            return;
        };
        let (width, height) = (out_tex.width, out_tex.height);

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            // Single-source: the standalone kernel is generated from the same
            // `wgsl_body` the fusion codegen chains. Both inputs are
            // CoincidentTexel (exact-texel, no sampler), so the generated
            // bindings are uniform(0), in(1), pattern(2), dst(3) — matching the
            // sampler-free binding set below. dither.wgsl is the parity oracle.
            let wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                .expect("node.dither standalone codegen");
            gpu.device.create_compute_pipeline(
                &wgsl,
                crate::node_graph::freeze::codegen::ENTRY,
                "node.dither",
            )
        });

        let uniforms = DitherUniforms {
            amount,
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
                GpuBinding::Texture {
                    binding: 2,
                    texture: pattern_tex,
                },
                GpuBinding::Texture {
                    binding: 3,
                    texture: out_tex,
                },
            ],
            [width.div_ceil(16), height.div_ceil(16), 1],
            "node.dither",
        );
    }
}
