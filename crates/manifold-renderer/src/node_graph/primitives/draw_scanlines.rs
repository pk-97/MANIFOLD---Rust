//! `node.draw_scanlines` — a subtle repeating horizontal scanline
//! pattern composited additively over the whole frame. The one HUD
//! layer that draws regardless of detections (it's a screen treatment,
//! not a per-object marker), so it has no skip contract. Math ported
//! verbatim from the Blob Track HUD's `scanline` wgsl_compute kernel.

use std::borrow::Cow;
use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ScanlinesUniforms {
    color: [f32; 3],
    alpha: f32,
    period_px: f32,
    intensity: f32,
    _pad0: u32,
    _pad1: u32,
}

crate::primitive! {
    name: DrawScanlines,
    type_id: "node.draw_scanlines",
    purpose: "Composite a subtle repeating horizontal scanline pattern over the whole image, additively. period_px sets the line spacing in output pixels; intensity sets how much brightness each line adds. The monitor-glass screen treatment that finishes a HUD look.",
    inputs: {
        in: Texture2D required,
        alpha: ScalarF32 optional,
        intensity: ScalarF32 optional,
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
            name: Cow::Borrowed("period_px"),
            label: "Spacing",
            ty: ParamType::Float,
            default: ParamValue::Float(2.0),
            range: Some((1.0, 64.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("intensity"),
            label: "Intensity",
            ty: ParamType::Float,
            default: ParamValue::Float(0.04),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
    ],
    composition_notes: "Place last in a HUD stack so the scanlines sit over every layer (Blob Track uses spacing 2, intensity 0.04). alpha and intensity are port-shadowed — wire the HUD's shared amount control into alpha.",
    examples: [],
    picker: { label: "Draw Scanlines", category: Atom },
    summary: "Adds faint monitor-style scanlines across the whole image.",
    category: Stylize,
    role: Filter,
    aliases: ["draw scanlines", "hud", "overlay", "scanline", "crt"],
    boundary_reason: Blocked,
}

const SCANLINES_SHADER: &str = r#"
struct U {
    color: vec3<f32>,
    alpha: f32,
    period_px: f32,
    intensity: f32,
    _pad0: u32,
    _pad1: u32,
};

@group(0) @binding(0) var<uniform> u: U;
@group(0) @binding(1) var source_tex: texture_2d<f32>;
@group(0) @binding(2) var src_sampler: sampler;
@group(0) @binding(3) var output_tex: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if gid.x >= dims.x || gid.y >= dims.y { return; }
    let uv = (vec2<f32>(gid.xy) + 0.5) / vec2<f32>(dims);
    var src = textureSampleLevel(source_tex, src_sampler, uv, 0.0);

    let scanline = abs(fract(uv.y * f32(dims.y) / u.period_px) - 0.5) * 2.0;
    let scan_alpha = (1.0 - smoothstep(0.4, 0.5, scanline)) * u.intensity;

    let add = scan_alpha * u.alpha;
    src = vec4<f32>(src.rgb + u.color * add, src.a);
    textureStore(output_tex, vec2<i32>(gid.xy), src);
}
"#;

impl Primitive for DrawScanlines {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let color = match ctx.params.get("color") {
            Some(ParamValue::Color(c)) => [c[0], c[1], c[2]],
            _ => [0.85, 0.92, 1.0],
        };
        let alpha = ctx.scalar_or_param("alpha", 1.0);
        let period_px = ctx.scalar_or_param("period_px", 2.0);
        let intensity = ctx.scalar_or_param("intensity", 0.04);

        let Some(in_tex) = ctx.inputs.texture_2d("in") else {
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
                .create_compute_pipeline(SCANLINES_SHADER, "cs_main", "node.draw_scanlines")
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        let uniforms = ScanlinesUniforms {
            color,
            alpha,
            period_px,
            intensity,
            _pad0: 0,
            _pad1: 0,
        };

        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                GpuBinding::Texture { binding: 1, texture: in_tex },
                GpuBinding::Sampler { binding: 2, sampler },
                GpuBinding::Texture { binding: 3, texture: out_tex },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "node.draw_scanlines",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn draw_scanlines_declares_ports() {
        assert_eq!(DrawScanlines::TYPE_ID, "node.draw_scanlines");
        let prim = DrawScanlines::new();
        let node: &dyn EffectNode = &prim;
        // A screen treatment, not a per-object marker — never skips.
        assert!(node.empty_skip_input_ports().is_empty());
        assert_eq!(node.skip_passthrough_ports(), None);
    }

    #[test]
    fn uniforms_are_32_bytes() {
        assert_eq!(std::mem::size_of::<ScanlinesUniforms>(), 32);
    }
}
