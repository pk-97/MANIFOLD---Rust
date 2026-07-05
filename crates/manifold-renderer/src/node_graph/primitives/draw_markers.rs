//! `node.draw_markers` — stamp a line-drawn marker symbol at every
//! detection in a `Channels[X, Y, WIDTH, HEIGHT]` array, composited
//! additively onto a source texture.
//!
//! Two symbols, one honest param surface (both use every param):
//! `Corner Brackets` draws four L-corners on the detection's bounding
//! box; `Crosshair` draws a horizontal + vertical cross at its centre.
//! `size_fraction` scales the arms relative to the detection's smaller
//! half-extent; `thickness_px` is line thickness in 1080p-reference
//! pixels (resolution-independent look). Math ported verbatim from the
//! Blob Track HUD's `brackets` / `crosshair` wgsl_compute kernels —
//! the rebuilt preset must look pixel-identical.
//!
//! Data-driven skip: when the wired `detections` array has been empty
//! for two frames the executor aliases `in` → `out` (zero GPU work) —
//! see `skip_passthrough_ports` + `empty_skip_input_ports`.

use std::borrow::Cow;
use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct MarkersUniforms {
    color: [f32; 3],
    alpha: f32,
    size_fraction: f32,
    thickness_px: f32,
    symbol: u32,
    _pad0: u32,
}

crate::primitive! {
    name: DrawMarkers,
    type_id: "node.draw_markers",
    purpose: "Stamp a marker symbol at every detection in a Channels[X, Y, WIDTH, HEIGHT] array, drawn additively over the source image. Symbol picks the look: Corner Brackets traces the four corners of each detection's bounding box, Crosshair draws a cross at its centre. Arm length follows the detection's size via size_fraction; thickness_px keeps line weight constant across resolutions. The marker layer of a tracking HUD.",
    inputs: {
        in: Texture2D required,
        detections: Channels[X: F32, Y: F32, WIDTH: F32, HEIGHT: F32] required,
        alpha: ScalarF32 optional,
        size_fraction: ScalarF32 optional,
        thickness_px: ScalarF32 optional,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("symbol"),
            label: "Symbol",
            ty: ParamType::Enum,
            default: ParamValue::Enum(0),
            range: None,
            enum_values: &["Corner Brackets", "Crosshair"],
        },
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
            name: Cow::Borrowed("size_fraction"),
            label: "Size",
            ty: ParamType::Float,
            default: ParamValue::Float(0.4),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("thickness_px"),
            label: "Thickness",
            ty: ParamType::Float,
            default: ParamValue::Float(2.0),
            range: Some((0.5, 12.0)),
            enum_values: &[],
        },
    ],
    composition_notes: "Wire a detector chain (node.blob_tracker → node.track_persist → node.one_euro_filter) into `detections` and the video into `in`. Stack multiple instances for layered HUDs (brackets + crosshair = the Blob Track look: brackets at size_fraction 0.4 / thickness 2, crosshair at 0.3 / 1.5). alpha is port-shadowed — wire one amount control into every Draw node's alpha to fade the whole HUD. Skips to a zero-cost passthrough while the detector reports nothing.",
    examples: [],
    picker: { label: "Draw Markers", category: Atom },
    summary: "Draws a marker on every tracked object: corner brackets around it or a crosshair at its centre. The building block for tracking overlays.",
    category: DetectionAndSampling,
    role: Filter,
    aliases: ["draw markers", "hud", "overlay", "brackets", "crosshair", "tracking marker"],
}

const MARKERS_SHADER: &str = r#"
struct U {
    color: vec3<f32>,
    alpha: f32,
    size_fraction: f32,
    thickness_px: f32,
    symbol: u32,
    _pad0: u32,
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
    let thickness = u.thickness_px * (1.0 / f32(dims.x)) * dpi_scale;

    var coverage = 0.0;
    let n = arrayLength(&detections);
    for (var i: u32 = 0u; i < n; i = i + 1u) {
        let d = detections[i];
        if d.width < 0.0001 && d.height < 0.0001 { continue; }
        let half_size = vec2<f32>(d.width, d.height) * 0.5;
        let center = vec2<f32>(d.x + half_size.x, d.y + half_size.y);
        let arm = min(half_size.x, half_size.y) * u.size_fraction;

        if u.symbol == 0u {
            let tl = center - half_size;
            let tr = vec2<f32>(center.x + half_size.x, center.y - half_size.y);
            let bl = vec2<f32>(center.x - half_size.x, center.y + half_size.y);
            let br = center + half_size;

            coverage = max(coverage, line_seg(uv, tl, tl + vec2<f32>(arm, 0.0), thickness));
            coverage = max(coverage, line_seg(uv, tl, tl + vec2<f32>(0.0, arm), thickness));
            coverage = max(coverage, line_seg(uv, tr, tr - vec2<f32>(arm, 0.0), thickness));
            coverage = max(coverage, line_seg(uv, tr, tr + vec2<f32>(0.0, arm), thickness));
            coverage = max(coverage, line_seg(uv, bl, bl + vec2<f32>(arm, 0.0), thickness));
            coverage = max(coverage, line_seg(uv, bl, bl - vec2<f32>(0.0, arm), thickness));
            coverage = max(coverage, line_seg(uv, br, br - vec2<f32>(arm, 0.0), thickness));
            coverage = max(coverage, line_seg(uv, br, br - vec2<f32>(0.0, arm), thickness));
        } else {
            coverage = max(coverage, line_seg(uv, center - vec2<f32>(arm, 0.0), center + vec2<f32>(arm, 0.0), thickness));
            coverage = max(coverage, line_seg(uv, center - vec2<f32>(0.0, arm), center + vec2<f32>(0.0, arm), thickness));
        }
    }

    let add = coverage * u.alpha;
    src = vec4<f32>(src.rgb + u.color * add, src.a);
    textureStore(output_tex, vec2<i32>(gid.xy), src);
}
"#;

impl Primitive for DrawMarkers {
    fn empty_skip_input_ports(&self) -> &'static [&'static str] {
        &["detections"]
    }

    fn skip_passthrough_ports(&self) -> Option<(&'static str, &'static str)> {
        Some(("in", "out"))
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let symbol = match ctx.params.get("symbol") {
            Some(ParamValue::Enum(n)) => (*n).min(1),
            Some(ParamValue::Float(f)) => (f.round().max(0.0) as u32).min(1),
            _ => 0,
        };
        let color = match ctx.params.get("color") {
            Some(ParamValue::Color(c)) => [c[0], c[1], c[2]],
            _ => [0.85, 0.92, 1.0],
        };
        let alpha = ctx.scalar_or_param("alpha", 1.0);
        let size_fraction = ctx.scalar_or_param("size_fraction", 0.4);
        let thickness_px = ctx.scalar_or_param("thickness_px", 2.0);

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
                .create_compute_pipeline(MARKERS_SHADER, "cs_main", "node.draw_markers")
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        let uniforms = MarkersUniforms {
            color,
            alpha,
            size_fraction,
            thickness_px,
            symbol,
            _pad0: 0,
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
            "node.draw_markers",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn draw_markers_declares_ports_and_skip_contract() {
        use crate::node_graph::ports::PortType;
        assert_eq!(DrawMarkers::TYPE_ID, "node.draw_markers");
        assert_eq!(DrawMarkers::INPUTS[0].name, "in");
        assert_eq!(DrawMarkers::INPUTS[0].ty, PortType::Texture2D);
        assert_eq!(DrawMarkers::INPUTS[1].name, "detections");
        assert!(matches!(DrawMarkers::INPUTS[1].ty, PortType::Array(_)));
        let prim = DrawMarkers::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.empty_skip_input_ports(), &["detections"]);
        assert_eq!(node.skip_passthrough_ports(), Some(("in", "out")));
    }

    #[test]
    fn uniforms_are_32_bytes() {
        assert_eq!(std::mem::size_of::<MarkersUniforms>(), 32);
    }
}
