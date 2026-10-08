//! `node.texture_sum_5` — per-pixel weighted-sum of five textures.
//!
//! `out = (a + b + c + d + e) / divisor`, all channels. `divisor=1.0`
//! (default) keeps the result a plain sum; `divisor=N` divides through —
//! the natural "average of N textures" shape (Plasma's contrast curve
//! pre-step, multi-tap composites, signed-field merges).

use std::borrow::Cow;
use manifold_gpu::GpuSamplerDesc;

use manifold_node_engine::exec::effect_node::EffectNodeContext;
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::primitive::Primitive;
use manifold_node_engine::primitives::standalone_pipeline::{dispatch_standalone_2d, standalone_pipeline};

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct TextureSum5Uniforms {
    divisor: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

manifold_node_engine::primitive! {
    name: TextureSum5,
    type_id: "node.texture_sum_5",
    purpose: "Per-pixel weighted-sum of five textures: out = (a+b+c+d+e) / divisor. divisor=1 keeps the result a plain sum (compose mode), divisor=5 turns it into an average. Collapses the four-deep Mix(Add) chain (+ optional scale_offset_texture for division) that a manual N-term composition would otherwise need.",
    inputs: {
        a: Texture2D required,
        b: Texture2D required,
        c: Texture2D required,
        d: Texture2D required,
        e: Texture2D required,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("divisor"),
            label: "Divisor",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 100.0)),
            enum_values: &[],
        },
    ],
    depth_rule: CombineNearest,
    composition_notes: "divisor=1 for a plain sum, divisor=5 for the canonical five-term average. Divide-by-zero clamps to 0 to keep the output finite. The shader does not clamp the range — a five-term sum of [-1,1] sin terms with divisor=5 lands in [-1,1] (so it feeds directly into smoothstep_bipolar without further scaling).",
    examples: [],
    summary: "Legacy fixed five-input sum, superseded by node.multi_blend (dynamic N inputs). Hidden from the palette but still loads in saved graphs.",
    category: Composite,
    role: Filter,
    aliases: ["add textures", "sum", "blend", "multi blend"],
    fusion_kind: MultiInputCoincident,
    wgsl_body: include_str!("shaders/texture_sum_5_body.wgsl"),
}

impl Primitive for TextureSum5 {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let divisor = ctx.param_f32("divisor", 1.0);

        let Some(a) = ctx.inputs.texture_2d("a") else {
            return;
        };
        let Some(b) = ctx.inputs.texture_2d("b") else {
            return;
        };
        let Some(c) = ctx.inputs.texture_2d("c") else {
            return;
        };
        let Some(d) = ctx.inputs.texture_2d("d") else {
            return;
        };
        let Some(e) = ctx.inputs.texture_2d("e") else {
            return;
        };
        let Some(out_tex) = ctx.outputs.texture_2d("out") else {
            return;
        };
        let (w, h) = (out_tex.width, out_tex.height);
        if w == 0 || h == 0 {
            return;
        }

        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        let uniforms = TextureSum5Uniforms {
            divisor,
            _pad0: 0.0,
            _pad1: 0.0,
            _pad2: 0.0,
        };

        dispatch_standalone_2d(
            gpu,
            pipeline,
            bytemuck::bytes_of(&uniforms),
            &[a, b, c, d, e],
            Some(sampler),
            out_tex,
            "node.texture_sum_5",
        );
    }
}
