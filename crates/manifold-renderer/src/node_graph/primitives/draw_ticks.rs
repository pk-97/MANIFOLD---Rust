//! `node.draw_ticks` — four horizontal tick marks descending from the
//! top-right corner of each detection's bounding box, alternating
//! long/short, composited additively onto a source texture at half
//! intensity (the legacy `* 0.5` modulation is baked — it's part of
//! the look, not a control). Math ported verbatim from the Blob Track
//! HUD's `tick_marks` wgsl_compute kernel.

use std::borrow::Cow;
use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Generated-codegen uniform layout: PARAMS order — `color` (Color param → 4
/// consecutive f32 fields, reassembled as `vec4<f32>` at the body call
/// site), `alpha`, `right_offset_px`, `long_tick_px`, `short_tick_px`,
/// `thickness_px` — then padded to a 16-byte multiple (9 header words + 3
/// pad = 12 words = 48 bytes). NOT the pre-conversion hand layout
/// (`vec3<f32>` + separate alpha, no explicit pad); the `[f32; 4]` here
/// matches the codegen's 4 scalar fields byte-for-byte.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct TicksUniforms {
    color: [f32; 4],
    alpha: f32,
    right_offset_px: f32,
    long_tick_px: f32,
    short_tick_px: f32,
    thickness_px: f32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

crate::primitive! {
    name: DrawTicks,
    type_id: "node.draw_ticks",
    purpose: "Draw four alternating long/short tick marks down the right side of every detection in a Channels[X, Y, WIDTH, HEIGHT] array, additively over the source at half intensity. Tick spacing follows the detection's height; lengths and offset are in 1080p-reference pixels. The scale-readout garnish of a tracking HUD.",
    inputs: {
        in: Texture2D required,
        detections: Channels[X: F32, Y: F32, WIDTH: F32, HEIGHT: F32] required,
        alpha: ScalarF32 optional,
        thickness_px: ScalarF32 optional,
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
            name: Cow::Borrowed("right_offset_px"),
            label: "Offset",
            ty: ParamType::Float,
            default: ParamValue::Float(8.0),
            range: Some((0.0, 64.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("long_tick_px"),
            label: "Long Tick",
            ty: ParamType::Float,
            default: ParamValue::Float(12.0),
            range: Some((1.0, 64.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("short_tick_px"),
            label: "Short Tick",
            ty: ParamType::Float,
            default: ParamValue::Float(6.0),
            range: Some((1.0, 64.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("thickness_px"),
            label: "Thickness",
            ty: ParamType::Float,
            default: ParamValue::Float(1.5),
            range: Some((0.5, 12.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "Wire detections into `detections` and the video into `in`; stacks with the other Draw nodes (Blob Track uses offset 8, long 12, short 6, thickness 1.5). alpha is port-shadowed for a shared HUD fade. Skips to a zero-cost passthrough while the detector reports nothing.",
    examples: [],
    picker: { label: "Draw Ticks", category: Atom },
    summary: "Draws small measurement-style tick marks beside every tracked object.",
    category: DetectionAndSampling,
    role: Filter,
    aliases: ["draw ticks", "hud", "overlay", "tick marks", "ruler"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/draw_ticks_body.wgsl"),
    input_access: [Coincident, BufferIndex],
}

impl Primitive for DrawTicks {
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
        let right_offset_px = ctx.scalar_or_param("right_offset_px", 8.0);
        let long_tick_px = ctx.scalar_or_param("long_tick_px", 12.0);
        let short_tick_px = ctx.scalar_or_param("short_tick_px", 6.0);
        let thickness_px = ctx.scalar_or_param("thickness_px", 1.5);

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
                .expect("node.draw_ticks standalone codegen");
            gpu.device.create_compute_pipeline(
                &wgsl,
                crate::node_graph::freeze::codegen::ENTRY,
                "node.draw_ticks",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        // Uniform layout matches the generated Params struct: PARAMS order
        // (color → vec4, alpha, right_offset_px, long_tick_px,
        // short_tick_px, thickness_px) — 9 header words + 3 pad = 12 words.
        let uniforms = TicksUniforms {
            color,
            alpha,
            right_offset_px,
            long_tick_px,
            short_tick_px,
            thickness_px,
            _pad0: 0,
            _pad1: 0,
            _pad2: 0,
        };

        // Bindings match the generated standalone layout: uniform(0), texture
        // input `in`(1), sampler(2), array input `detections`→`buf_detections`(3),
        // output(4).
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
            "node.draw_ticks",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn draw_ticks_declares_ports_and_skip_contract() {
        assert_eq!(DrawTicks::TYPE_ID, "node.draw_ticks");
        let prim = DrawTicks::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.empty_skip_input_ports(), &["detections"]);
        assert_eq!(node.skip_passthrough_ports(), Some(("in", "out")));
    }

    #[test]
    fn uniforms_are_48_bytes() {
        assert_eq!(std::mem::size_of::<TicksUniforms>(), 48);
    }
}

