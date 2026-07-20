//! `node.draw_gauge` — an outlined bar below each detection's bounding
//! box whose fill fraction is proportional to the detection's area,
//! composited additively onto a source texture. The fill renders at 0.4
//! intensity against the 1.0 outline (baked, part of the look). Math
//! ported verbatim from the Blob Track HUD's `size_gauge` wgsl_compute
//! kernel.

use std::borrow::Cow;
use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Generated-codegen uniform layout: PARAMS order — `color` (Color param → 4
/// consecutive f32 fields, reassembled as `vec4<f32>` at the body call
/// site), `alpha`, `bottom_offset_px`, `bar_height_px`, `min_bar_width_px`,
/// `fill_scale`, `thickness_px` — then padded to a 16-byte multiple (10
/// header words + 2 pad = 12 words = 48 bytes). NOT the pre-conversion hand
/// layout (`vec3<f32>` + separate alpha, 3×u32 pad); the `[f32; 4]` here
/// matches the codegen's 4 scalar fields byte-for-byte.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GaugeUniforms {
    color: [f32; 4],
    alpha: f32,
    bottom_offset_px: f32,
    bar_height_px: f32,
    min_bar_width_px: f32,
    fill_scale: f32,
    thickness_px: f32,
    _pad0: u32,
    _pad1: u32,
}

crate::primitive! {
    name: DrawGauge,
    type_id: "node.draw_gauge",
    purpose: "Draw an outlined readout bar below every detection in a Channels[X, Y, WIDTH, HEIGHT] array, additively over the source. The bar fills in proportion to the detection's area (fill_scale maps area to the 0..1 fill), so bigger objects read as fuller bars. Pixel params are 1080p-referenced. The size-readout layer of a tracking HUD.",
    inputs: {
        in: Texture2D required,
        detections: Channels[X: F32, Y: F32, WIDTH: F32, HEIGHT: F32] required,
        alpha: ScalarF32 optional,
        fill_scale: ScalarF32 optional,
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
            name: Cow::Borrowed("bottom_offset_px"),
            label: "Offset",
            ty: ParamType::Float,
            default: ParamValue::Float(50.0),
            range: Some((0.0, 200.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("bar_height_px"),
            label: "Bar Height",
            ty: ParamType::Float,
            default: ParamValue::Float(8.0),
            range: Some((1.0, 64.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("min_bar_width_px"),
            label: "Min Width",
            ty: ParamType::Float,
            default: ParamValue::Float(80.0),
            range: Some((1.0, 400.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("fill_scale"),
            label: "Fill Scale",
            ty: ParamType::Float,
            default: ParamValue::Float(20.0),
            range: Some((0.0, 200.0)),
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
    composition_notes: "Wire detections into `detections` and the video into `in` (Blob Track uses offset 50, height 8, min width 80, fill scale 20, thickness 1.5). fill_scale is port-shadowed — drive it to make the gauge respond to something other than area. Skips to a zero-cost passthrough while the detector reports nothing.",
    examples: [],
    picker: { label: "Draw Gauge", category: Atom },
    summary: "Draws a small readout bar under every tracked object that fills up as the object gets bigger.",
    category: DetectionAndSampling,
    role: Filter,
    aliases: ["draw gauge", "hud", "overlay", "size bar", "meter"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/draw_gauge_body.wgsl"),
    input_access: [Coincident, BufferIndex],
}

impl Primitive for DrawGauge {
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
        let bottom_offset_px = ctx.scalar_or_param("bottom_offset_px", 50.0);
        let bar_height_px = ctx.scalar_or_param("bar_height_px", 8.0);
        let min_bar_width_px = ctx.scalar_or_param("min_bar_width_px", 80.0);
        let fill_scale = ctx.scalar_or_param("fill_scale", 20.0);
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
        // texture region via the `BufferIndex` read path. `shaders/draw_gauge.wgsl`
        // (the hand-kernel parity oracle) was deleted 2026-07-20 (W1-B,
        // migration scaffolding retired).
        let pipeline = self.pipeline.get_or_insert_with(|| {
            let wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                .expect("node.draw_gauge standalone codegen");
            gpu.device.create_compute_pipeline(
                &wgsl,
                crate::node_graph::freeze::codegen::ENTRY,
                "node.draw_gauge",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        // Uniform layout matches the generated Params struct: PARAMS order
        // (color → vec4, alpha, bottom_offset_px, bar_height_px,
        // min_bar_width_px, fill_scale, thickness_px) — 10 header words + 2
        // pad = 12 words.
        let uniforms = GaugeUniforms {
            color,
            alpha,
            bottom_offset_px,
            bar_height_px,
            min_bar_width_px,
            fill_scale,
            thickness_px,
            _pad0: 0,
            _pad1: 0,
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
            "node.draw_gauge",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn draw_gauge_declares_ports_and_skip_contract() {
        assert_eq!(DrawGauge::TYPE_ID, "node.draw_gauge");
        let prim = DrawGauge::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.empty_skip_input_ports(), &["detections"]);
        assert_eq!(node.skip_passthrough_ports(), Some(("in", "out")));
    }

    #[test]
    fn uniforms_are_48_bytes() {
        assert_eq!(std::mem::size_of::<GaugeUniforms>(), 48);
    }
}

