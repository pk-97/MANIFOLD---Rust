//! `node.draw_dots` — soft dot at every detection's centre, composited
//! additively onto a source texture. Coverage falls off linearly with
//! distance, giving a small glow rather than a hard disc. Math ported
//! verbatim from the Blob Track HUD's `center_dot` wgsl_compute kernel.

use std::borrow::Cow;
use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct DotsUniforms {
    color: [f32; 3],
    alpha: f32,
    radius_px: f32,
    _pad: [u32; 3],
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
    composition_notes: "Wire detections into `detections` and the video into `in`; stacks with node.draw_markers / node.draw_ticks for a full HUD (Blob Track uses radius_px 4). alpha is port-shadowed for a shared HUD fade control. Skips to a zero-cost passthrough while the detector reports nothing.",
    examples: [],
    picker: { label: "Draw Dots", category: Atom },
    summary: "Draws a small glowing dot at the centre of every tracked object.",
    category: DetectionAndSampling,
    role: Filter,
    aliases: ["draw dots", "hud", "overlay", "center dot", "points"],
}

const DOTS_SHADER: &str = r#"
struct U {
    color: vec3<f32>,
    alpha: f32,
    radius_px: f32,
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

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if gid.x >= dims.x || gid.y >= dims.y { return; }
    let uv = (vec2<f32>(gid.xy) + 0.5) / vec2<f32>(dims);
    var src = textureSampleLevel(source_tex, src_sampler, uv, 0.0);

    let dpi_scale = f32(dims.y) / 1080.0;
    let radius = u.radius_px * (1.0 / f32(dims.x)) * dpi_scale;

    var coverage = 0.0;
    let n = arrayLength(&detections);
    for (var i: u32 = 0u; i < n; i = i + 1u) {
        let d = detections[i];
        if d.width < 0.0001 && d.height < 0.0001 { continue; }
        let center = vec2<f32>(d.x + d.width * 0.5, d.y + d.height * 0.5);
        let dist = length(uv - center);
        coverage = max(coverage, 1.0 - saturate(dist / radius));
    }

    let add = coverage * u.alpha;
    src = vec4<f32>(src.rgb + u.color * add, src.a);
    textureStore(output_tex, vec2<i32>(gid.xy), src);
}
"#;

impl Primitive for DrawDots {
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
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device
                .create_compute_pipeline(DOTS_SHADER, "cs_main", "node.draw_dots")
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        let uniforms = DotsUniforms { color, alpha, radius_px, _pad: [0; 3] };

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
