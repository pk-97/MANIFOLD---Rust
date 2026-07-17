//! `node.edge_detect` — pixel-exact replacement for legacy
//! Originally `EdgeDetectFX`.
//! Tenth §6.1 migration and the first **fused composite primitive**.
//!
//! Sobel 3×3 + smoothstep-threshold in a single compute pass. The
//! atomic decomposition would be `Sobel3 → Threshold` with an
//! intermediate `Rgba16Float` write between primitives — that write
//! introduces fp16 quantization the legacy single-pass shader
//! avoids, breaking bit-exact parity. Ship as a fused composite for
//! now; atomic Sobel3 / Threshold are tracked separately and become
//! the canonical decomposition once the future fusion compiler can
//! re-merge adjacent pixel-local primitives into one dispatch.
//!
//! The legacy `EffectMetadata` declares a `mode` parameter (Sobel /
//! Laplacian / Frei-Chen) that `EdgeDetectFX::apply` never reads;
//! only Sobel is implemented in the shader. The primitive drops
//! `mode` from its surface to avoid documenting a parameter that has
//! no effect.

use std::borrow::Cow;
use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

crate::primitive! {
    name: EdgeDetect,
    type_id: "node.edge_detect",
    purpose: "Sobel 3×3 edge detection with smoothstep threshold, crossfaded against the source by amount. Brightness-based; no glow — chain with Bloom or Halation for glow.",
    inputs: {
        in: Texture2D required,
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
            name: Cow::Borrowed("threshold"),
            label: "Threshold",
            ty: ParamType::Float,
            default: ParamValue::Float(0.1),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Inherit,
    composition_notes: "Fused composite primitive — Sobel + smoothstep-threshold in one pass. Will split into atomic Sobel3 + Threshold when the fusion compiler can preserve bit-exact parity. 1:1 replacement for the legacy EdgeDetectFX effect; the legacy mode param (Laplacian/Frei-Chen) was never wired to the shader and is dropped.",
    examples: ["preset.effect.edge_detect"],
    picker: { label: "Edge Detect", category: Atom },
    summary: "Finds the edges in the image and draws them as bright lines on dark, a Sobel outline. Crossfade it back over the source for a sketch look.",
    category: Stylize,
    role: Filter,
    aliases: ["edge detect", "sobel", "outline", "Edge TOP"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/edge_detect_body.wgsl"),
    input_access: [Gather],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct EdgeDetectUniforms {
    amount: f32,
    threshold: f32,
    texel_size_x: f32,
    texel_size_y: f32,
}

impl Primitive for EdgeDetect {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let amount = match ctx.params.get("amount") {
            Some(ParamValue::Float(f)) => *f,
            _ => 0.0,
        };
        let threshold = match ctx.params.get("threshold") {
            Some(ParamValue::Float(f)) => *f,
            _ => 0.1,
        };

        let Some(in_tex) = ctx.inputs.texture_2d("in") else {
            return;
        };
        let Some(out_tex) = ctx.outputs.texture_2d("out") else {
            return;
        };
        let (width, height) = (out_tex.width, out_tex.height);
        // Texel size matches legacy's `1.0 / ctx.output_width/height`
        // — intrinsic to the output texture, identical at parity dims.
        let texel_size_x = 1.0 / width as f32;
        let texel_size_y = 1.0 / height as f32;

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            // Single-source: `in` is a Gather input (Sobel 3×3 neighbourhood).
            // Generated kernel binds uniform(0)/tex(1)/samp(2)/dst(3); the body
            // recovers the texel step from `dims` so it ignores the uniform's
            // texel_size_x/y fields. edge_detect.wgsl is the parity oracle.
            gpu.device.create_compute_pipeline(
                &crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                    .expect("node.edge_detect standalone codegen"),
                crate::node_graph::freeze::codegen::ENTRY,
                "node.edge_detect",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        let uniforms = EdgeDetectUniforms {
            amount,
            threshold,
            texel_size_x,
            texel_size_y,
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
            "node.edge_detect",
        );
    }
}
