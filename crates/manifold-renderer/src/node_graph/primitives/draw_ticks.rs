//! `node.draw_ticks` — four horizontal tick marks descending from the
//! top-right corner of each detection's bounding box, alternating
//! long/short, composited additively onto a source texture at half
//! intensity (the legacy `* 0.5` modulation is baked — it's part of
//! the look, not a control). Math ported verbatim from the Blob Track
//! HUD's `tick_marks` wgsl_compute kernel.

use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct TicksUniforms {
    color: [f32; 3],
    alpha: f32,
    right_offset_px: f32,
    long_tick_px: f32,
    short_tick_px: f32,
    thickness_px: f32,
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
            name: "color",
            label: "Color",
            ty: ParamType::Color,
            default: ParamValue::Color([0.85, 0.92, 1.0, 1.0]),
            range: None,
            enum_values: &[],
        },
        ParamDef {
            name: "alpha",
            label: "Alpha",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "right_offset_px",
            label: "Offset",
            ty: ParamType::Float,
            default: ParamValue::Float(8.0),
            range: Some((0.0, 64.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "long_tick_px",
            label: "Long Tick",
            ty: ParamType::Float,
            default: ParamValue::Float(12.0),
            range: Some((1.0, 64.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "short_tick_px",
            label: "Short Tick",
            ty: ParamType::Float,
            default: ParamValue::Float(6.0),
            range: Some((1.0, 64.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "thickness_px",
            label: "Thickness",
            ty: ParamType::Float,
            default: ParamValue::Float(1.5),
            range: Some((0.5, 12.0)),
            enum_values: &[],
        },
    ],
    composition_notes: "Wire detections into `detections` and the video into `in`; stacks with the other Draw nodes (Blob Track uses offset 8, long 12, short 6, thickness 1.5). alpha is port-shadowed for a shared HUD fade. Skips to a zero-cost passthrough while the detector reports nothing.",
    examples: [],
    picker: { label: "Draw Ticks", category: Atom },
    summary: "Draws small measurement-style tick marks beside every tracked object.",
    category: DetectionAndSampling,
    role: Filter,
    aliases: ["draw ticks", "hud", "overlay", "tick marks", "ruler"],
}

const TICKS_SHADER: &str = r#"
struct U {
    color: vec3<f32>,
    alpha: f32,
    right_offset_px: f32,
    long_tick_px: f32,
    short_tick_px: f32,
    thickness_px: f32,
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
    let thickness = u.thickness_px * px_u;
    let right_offset = u.right_offset_px * px_u;

    var coverage = 0.0;
    let n = arrayLength(&detections);
    for (var i: u32 = 0u; i < n; i = i + 1u) {
        let d = detections[i];
        if d.width < 0.0001 && d.height < 0.0001 { continue; }
        let half_size = vec2<f32>(d.width, d.height) * 0.5;
        let center = vec2<f32>(d.x + half_size.x, d.y + half_size.y);

        let tick_base = vec2<f32>(center.x + half_size.x + right_offset, center.y - half_size.y);
        let tick_spacing = half_size.y * 0.5;

        for (var t: u32 = 0u; t < 4u; t = t + 1u) {
            let tick_start = tick_base + vec2<f32>(0.0, tick_spacing * f32(t));
            let tick_len = select(u.short_tick_px * px_u, u.long_tick_px * px_u, (t % 2u) == 0u);
            coverage = max(coverage, line_seg(uv, tick_start, tick_start + vec2<f32>(tick_len, 0.0), thickness) * 0.5);
        }
    }

    let add = coverage * u.alpha;
    src = vec4<f32>(src.rgb + u.color * add, src.a);
    textureStore(output_tex, vec2<i32>(gid.xy), src);
}
"#;

impl Primitive for DrawTicks {
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
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device
                .create_compute_pipeline(TICKS_SHADER, "cs_main", "node.draw_ticks")
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        let uniforms = TicksUniforms {
            color,
            alpha,
            right_offset_px,
            long_tick_px,
            short_tick_px,
            thickness_px,
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
    fn uniforms_are_32_bytes() {
        assert_eq!(std::mem::size_of::<TicksUniforms>(), 32);
    }
}
