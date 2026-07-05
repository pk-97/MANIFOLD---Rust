//! `node.draw_connections` — dashed lines between paired detections,
//! with an optional soft dot at each pair's midpoint, composited
//! additively onto a source texture. Pairs come in as
//! `Channels[A_INDEX, B_INDEX]` (node.connect_nearest's output)
//! indexing into the detections array. Lines render at 0.5 intensity
//! and midpoints at 0.4 (baked, part of the look). Math ported
//! verbatim from the Blob Track HUD's `dashed_connections` +
//! `midpoint_diamonds` wgsl_compute kernels — folded into one dispatch
//! by summing the two coverage terms (the legacy pair of additive
//! passes sums the same way; the only difference is one f16 store
//! round between them, ~1 ulp).

use std::borrow::Cow;
use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ConnectionsUniforms {
    color: [f32; 3],
    alpha: f32,
    thickness_px: f32,
    dash_period_px: f32,
    dash_fill: f32,
    midpoint_radius_px: f32,
}

crate::primitive! {
    name: DrawConnections,
    type_id: "node.draw_connections",
    purpose: "Draw dashed lines between paired detections, additively over the source. Pairs arrive as Channels[A_INDEX, B_INDEX] (wire node.connect_nearest) indexing the Channels[X, Y, WIDTH, HEIGHT] detections array; each line runs centre to centre. midpoint_radius_px adds a soft dot at each pair's midpoint (0 turns it off). Pixel params are 1080p-referenced. The relationship layer of a tracking HUD.",
    inputs: {
        in: Texture2D required,
        detections: Channels[X: F32, Y: F32, WIDTH: F32, HEIGHT: F32] required,
        edges: Channels[A_INDEX: U32, B_INDEX: U32] required,
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
            name: Cow::Borrowed("thickness_px"),
            label: "Thickness",
            ty: ParamType::Float,
            default: ParamValue::Float(1.5),
            range: Some((0.5, 12.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("dash_period_px"),
            label: "Dash Length",
            ty: ParamType::Float,
            default: ParamValue::Float(12.0),
            range: Some((1.0, 128.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("dash_fill"),
            label: "Dash Fill",
            ty: ParamType::Float,
            default: ParamValue::Float(0.4),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("midpoint_radius_px"),
            label: "Midpoint Dot",
            ty: ParamType::Float,
            default: ParamValue::Float(5.0),
            range: Some((0.0, 64.0)),
            enum_values: &[],
        },
    ],
    composition_notes: "Wire node.connect_nearest's edges output into `edges` and the same detections it consumed into `detections`. dash_fill is the OFF fraction of each dash cycle (legacy step semantics); set midpoint_radius_px to 0 for plain lines. Skips to a zero-cost passthrough while no pairs exist.",
    examples: [],
    picker: { label: "Draw Connections", category: Atom },
    summary: "Draws dashed lines linking tracked objects that are near each other, with an optional dot at the middle of each link.",
    category: DetectionAndSampling,
    role: Filter,
    aliases: ["draw connections", "hud", "overlay", "connection lines", "links", "constellation"],
}

const CONNECTIONS_SHADER: &str = r#"
struct U {
    color: vec3<f32>,
    alpha: f32,
    thickness_px: f32,
    dash_period_px: f32,
    dash_fill: f32,
    midpoint_radius_px: f32,
};

struct Detection {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
};

struct Edge {
    a_index: u32,
    b_index: u32,
};

@group(0) @binding(0) var<uniform> u: U;
@group(0) @binding(1) var<storage, read> detections: array<Detection>;
@group(0) @binding(2) var<storage, read> edges: array<Edge>;
@group(0) @binding(3) var source_tex: texture_2d<f32>;
@group(0) @binding(4) var src_sampler: sampler;
@group(0) @binding(5) var output_tex: texture_storage_2d<rgba16float, write>;

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
    let dash_period = u.dash_period_px * px_u;
    let mid_radius = u.midpoint_radius_px * px_u;
    let det_count = arrayLength(&detections);

    var line_cov = 0.0;
    var mid_cov = 0.0;
    let n = arrayLength(&edges);
    for (var i: u32 = 0u; i < n; i = i + 1u) {
        let e = edges[i];
        if e.a_index == 0xFFFFFFFFu { continue; }
        if e.a_index >= det_count || e.b_index >= det_count { continue; }
        let da = detections[e.a_index];
        let db = detections[e.b_index];
        let center_a = vec2<f32>(da.x + da.width * 0.5, da.y + da.height * 0.5);
        let center_b = vec2<f32>(db.x + db.width * 0.5, db.y + db.height * 0.5);

        let ba = center_b - center_a;
        let len_sq = dot(ba, ba);
        if len_sq < 0.000001 { continue; }
        let pa = uv - center_a;
        let t_val = saturate(dot(pa, ba) / len_sq);
        let len = sqrt(len_sq);
        let dash_phase = fract(t_val * len / dash_period);
        let dash_mask = step(u.dash_fill, dash_phase);

        line_cov = max(line_cov, line_seg(uv, center_a, center_b, thickness) * 0.5 * dash_mask);

        if mid_radius > 0.0 {
            let mid = (center_a + center_b) * 0.5;
            let mid_dist = length(uv - mid);
            mid_cov = max(mid_cov, (1.0 - saturate(mid_dist / mid_radius)) * 0.4);
        }
    }

    let add = (line_cov + mid_cov) * u.alpha;
    src = vec4<f32>(src.rgb + u.color * add, src.a);
    textureStore(output_tex, vec2<i32>(gid.xy), src);
}
"#;

impl Primitive for DrawConnections {
    fn empty_skip_input_ports(&self) -> &'static [&'static str] {
        &["edges"]
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
        let thickness_px = ctx.scalar_or_param("thickness_px", 1.5);
        let dash_period_px = ctx.scalar_or_param("dash_period_px", 12.0);
        let dash_fill = ctx.scalar_or_param("dash_fill", 0.4);
        let midpoint_radius_px = ctx.scalar_or_param("midpoint_radius_px", 5.0);

        let Some(in_tex) = ctx.inputs.texture_2d("in") else {
            return;
        };
        let Some(det_buf) = ctx.inputs.array("detections") else {
            return;
        };
        let Some(edge_buf) = ctx.inputs.array("edges") else {
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
                .create_compute_pipeline(CONNECTIONS_SHADER, "cs_main", "node.draw_connections")
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        let uniforms = ConnectionsUniforms {
            color,
            alpha,
            thickness_px,
            dash_period_px,
            dash_fill,
            midpoint_radius_px,
        };

        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                GpuBinding::Buffer { binding: 1, buffer: det_buf, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: edge_buf, offset: 0 },
                GpuBinding::Texture { binding: 3, texture: in_tex },
                GpuBinding::Sampler { binding: 4, sampler },
                GpuBinding::Texture { binding: 5, texture: out_tex },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "node.draw_connections",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn draw_connections_declares_ports_and_skip_contract() {
        use crate::node_graph::ports::PortType;
        assert_eq!(DrawConnections::TYPE_ID, "node.draw_connections");
        assert_eq!(DrawConnections::INPUTS[1].name, "detections");
        assert_eq!(DrawConnections::INPUTS[2].name, "edges");
        assert!(matches!(DrawConnections::INPUTS[2].ty, PortType::Array(_)));
        let prim = DrawConnections::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.empty_skip_input_ports(), &["edges"]);
        assert_eq!(node.skip_passthrough_ports(), Some(("in", "out")));
    }

    #[test]
    fn uniforms_are_32_bytes() {
        assert_eq!(std::mem::size_of::<ConnectionsUniforms>(), 32);
    }
}
