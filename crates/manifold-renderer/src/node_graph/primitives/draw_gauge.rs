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

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GaugeUniforms {
    color: [f32; 3],
    alpha: f32,
    bottom_offset_px: f32,
    bar_height_px: f32,
    min_bar_width_px: f32,
    fill_scale: f32,
    thickness_px: f32,
    _pad: [u32; 3],
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
    composition_notes: "Wire detections into `detections` and the video into `in` (Blob Track uses offset 50, height 8, min width 80, fill scale 20, thickness 1.5). fill_scale is port-shadowed — drive it to make the gauge respond to something other than area. Skips to a zero-cost passthrough while the detector reports nothing.",
    examples: [],
    picker: { label: "Draw Gauge", category: Atom },
    summary: "Draws a small readout bar under every tracked object that fills up as the object gets bigger.",
    category: DetectionAndSampling,
    role: Filter,
    aliases: ["draw gauge", "hud", "overlay", "size bar", "meter"],
}

const GAUGE_SHADER: &str = r#"
struct U {
    color: vec3<f32>,
    alpha: f32,
    bottom_offset_px: f32,
    bar_height_px: f32,
    min_bar_width_px: f32,
    fill_scale: f32,
    thickness_px: f32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
};

struct Detection {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
};

@group(0) @binding(0) var<uniform> u: U;
@group(0) @binding(1) var<storage, read> detections: array<Detection>;
@group(0) @binding(2) var source_tex: texture_2d<f32>;
@group(0) @binding(3) var src_sampler: sampler;
@group(0) @binding(4) var output_tex: texture_storage_2d<rgba16float, write>;

fn line_seg(p: vec2<f32>, a: vec2<f32>, b: vec2<f32>, thickness: f32) -> f32 {
    let pa = p - a;
    let ba = b - a;
    let len_sq = dot(ba, ba);
    if len_sq < 0.000001 { return 0.0; }
    let h = saturate(dot(pa, ba) / len_sq);
    let d = length(pa - ba * h);
    return 1.0 - saturate(d / thickness);
}

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if gid.x >= dims.x || gid.y >= dims.y { return; }
    let uv = (vec2<f32>(gid.xy) + 0.5) / vec2<f32>(dims);
    var src = textureSampleLevel(source_tex, src_sampler, uv, 0.0);

    let dpi_scale = f32(dims.y) / 1080.0;
    let px_u = (1.0 / f32(dims.x)) * dpi_scale;
    let px_v = (1.0 / f32(dims.y)) * dpi_scale;
    let thickness = u.thickness_px * px_u;
    let bar_height = u.bar_height_px * px_v;
    let bottom_offset = u.bottom_offset_px * px_v;
    let min_bar_w = u.min_bar_width_px * px_u;

    var coverage = 0.0;
    let n = arrayLength(&detections);
    for (var i: u32 = 0u; i < n; i = i + 1u) {
        let d = detections[i];
        if d.width < 0.0001 && d.height < 0.0001 { continue; }
        let half_size = vec2<f32>(d.width, d.height) * 0.5;
        let center = vec2<f32>(d.x + half_size.x, d.y + half_size.y);

        let origin = vec2<f32>(center.x - half_size.x, center.y + half_size.y + bottom_offset);
        let bar_w = max(d.width, min_bar_w);
        let fill_frac = saturate(d.width * d.height * u.fill_scale);

        let tl = origin;
        let tr = origin + vec2<f32>(bar_w, 0.0);
        let bl = origin + vec2<f32>(0.0, bar_height);
        let br = origin + vec2<f32>(bar_w, bar_height);
        coverage = max(coverage, line_seg(uv, tl, tr, thickness));
        coverage = max(coverage, line_seg(uv, bl, br, thickness));
        coverage = max(coverage, line_seg(uv, tl, bl, thickness));
        coverage = max(coverage, line_seg(uv, tr, br, thickness));

        let rel = uv - origin;
        if rel.x >= 0.0 && rel.x <= bar_w * fill_frac && rel.y >= 0.0 && rel.y <= bar_height {
            coverage = max(coverage, 0.4);
        }
    }

    let add = coverage * u.alpha;
    src = vec4<f32>(src.rgb + u.color * add, src.a);
    textureStore(output_tex, vec2<i32>(gid.xy), src);
}
"#;

impl Primitive for DrawGauge {
    fn empty_skip_input_ports(&self) -> &'static [&'static str] {
        &["detections"]
    }

    fn skip_passthrough_ports(&self) -> Option<(&'static str, &'static str)> {
        Some(("in", "out"))
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let color = match ctx.params.get("color") {
            Some(ParamValue::Color(c)) => [c[0], c[1], c[2]],
            _ => [0.85, 0.92, 1.0],
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
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device
                .create_compute_pipeline(GAUGE_SHADER, "cs_main", "node.draw_gauge")
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        let uniforms = GaugeUniforms {
            color,
            alpha,
            bottom_offset_px,
            bar_height_px,
            min_bar_width_px,
            fill_scale,
            thickness_px,
            _pad: [0; 3],
        };

        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                GpuBinding::Buffer { binding: 1, buffer: det_buf, offset: 0 },
                GpuBinding::Texture { binding: 2, texture: in_tex },
                GpuBinding::Sampler { binding: 3, sampler },
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
