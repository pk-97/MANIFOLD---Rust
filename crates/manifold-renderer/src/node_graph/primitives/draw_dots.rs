//! `node.draw_dots` — soft dot at every detection's centre, composited
//! additively onto a source texture. Coverage falls off linearly with
//! distance, giving a small glow rather than a hard disc. Math ported
//! verbatim from the Blob Track HUD's `center_dot` wgsl_compute kernel.

use std::borrow::Cow;
use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Generated-codegen uniform layout: PARAMS order — `color` (Color param →
/// 4 consecutive f32 fields, reassembled as `vec4<f32>` at the body call
/// site), `alpha`, `radius_px` — then padded to a 16-byte multiple (6 header
/// words + 2 pad = 8 words = 32 bytes). NOT the pre-conversion hand layout
/// (`vec3<f32>` + separate alpha); the `[f32; 4]` here matches the codegen's
/// 4 scalar fields byte-for-byte (no vec3-alignment gap).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct DotsUniforms {
    color: [f32; 4],
    alpha: f32,
    radius_px: f32,
    _pad0: u32,
    _pad1: u32,
}

crate::primitive! {
    name: DrawDots,
    type_id: "node.draw_dots",
    purpose: "Draw a soft dot at the centre of every detection in a Channels[X, Y, WIDTH, HEIGHT] array, additively over the source image. Coverage fades linearly to the edge of radius_px (1080p-reference pixels), so dots read as small glows at any resolution. The centre-point layer of a tracking HUD.",
    inputs: {
        in: Texture2D required,
        detections: Channels[X: F32, Y: F32, WIDTH: F32, HEIGHT: F32] required,
        alpha: ScalarF32 optional,
        radius_px: ScalarF32 optional,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("color"),
            label: "Color",
            ty: ParamType::Color,
            default: ParamValue::Color([0.85, 0.92, 1.0, 1.0]),
            range: None,
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("alpha"),
            label: "Alpha",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("radius_px"),
            label: "Radius",
            ty: ParamType::Float,
            default: ParamValue::Float(4.0),
            range: Some((0.5, 64.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "Wire detections into `detections` and the video into `in`; stacks with node.draw_markers / node.draw_ticks for a full HUD (Blob Track uses radius_px 4). alpha is port-shadowed for a shared HUD fade control. Skips to a zero-cost passthrough while the detector reports nothing.",
    examples: [],
    picker: { label: "Draw Dots", category: Atom },
    summary: "Draws a small glowing dot at the centre of every tracked object.",
    category: DetectionAndSampling,
    role: Filter,
    aliases: ["draw dots", "hud", "overlay", "center dot", "points"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/draw_dots_body.wgsl"),
    input_access: [Coincident, BufferIndex],
}

impl Primitive for DrawDots {
    fn empty_skip_input_ports(&self) -> &'static [&'static str] {
        &["detections"]
    }

    fn skip_passthrough_ports(&self) -> Option<(&'static str, &'static str)> {
        Some(("in", "out"))
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let color = match ctx.params.get("color") {
            Some(ParamValue::Color(c)) => [c[0], c[1], c[2], 1.0],
            _ => [0.85, 0.92, 1.0, 1.0],
        };
        let alpha = ctx.scalar_or_param("alpha", 1.0);
        let radius_px = ctx.scalar_or_param("radius_px", 4.0);

        let Some(in_tex) = ctx.inputs.texture_2d("in") else {
            return;
        };
        let Some(det_buf) = ctx.inputs.array("detections") else {
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
        // Codegen path (mandatory for per-element GPU atoms, D3/BUG-114): the
        // kernel is generated from `wgsl_body` so the atom fuses into a
        // texture region via the `BufferIndex` read path.
        let pipeline = self.pipeline.get_or_insert_with(|| {
            let wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                .expect("node.draw_dots standalone codegen");
            gpu.device.create_compute_pipeline(
                &wgsl,
                crate::node_graph::freeze::codegen::ENTRY,
                "node.draw_dots",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        // Uniform layout matches the generated Params struct: PARAMS order
        // (color → vec4, alpha, radius_px), no injected fields (no derived
        // uniforms, no multi-output, no optional textures) — 8 words, no pad.
        let uniforms = DotsUniforms { color, alpha, radius_px, _pad0: 0, _pad1: 0 };

        // Bindings match the generated standalone layout: uniform(0), texture
        // input `in`(1), sampler(2), array input `detections`→`buf_detections`(3),
        // output(4) — texture inputs bind before the array input in this
        // codegen path (texture is the atom's primary domain).
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                GpuBinding::Texture { binding: 1, texture: in_tex },
                GpuBinding::Sampler { binding: 2, sampler },
                GpuBinding::Buffer { binding: 3, buffer: det_buf, offset: 0 },
                GpuBinding::Texture { binding: 4, texture: out_tex },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "node.draw_dots",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn draw_dots_declares_ports_and_skip_contract() {
        assert_eq!(DrawDots::TYPE_ID, "node.draw_dots");
        let prim = DrawDots::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.empty_skip_input_ports(), &["detections"]);
        assert_eq!(node.skip_passthrough_ports(), Some(("in", "out")));
    }

    #[test]
    fn uniforms_are_32_bytes() {
        assert_eq!(std::mem::size_of::<DotsUniforms>(), 32);
    }
}

