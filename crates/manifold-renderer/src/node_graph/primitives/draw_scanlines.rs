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

/// Generated-codegen uniform layout: PARAMS order — `color` (Color param → 4
/// consecutive f32 fields, reassembled as `vec4<f32>` at the body call
/// site), `alpha`, `period_px`, `intensity` — then padded to a 16-byte
/// multiple (7 header words + 1 pad = 8 words = 32 bytes). NOT the
/// pre-conversion hand layout (`vec3<f32>` + separate alpha, 2×u32 pad); the
/// `[f32; 4]` here matches the codegen's 4 scalar fields byte-for-byte.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ScanlinesUniforms {
    color: [f32; 4],
    alpha: f32,
    period_px: f32,
    intensity: f32,
    _pad0: u32,
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
    depth_rule: Inherit,
    composition_notes: "Place last in a HUD stack so the scanlines sit over every layer (Blob Track uses spacing 2, intensity 0.04). alpha and intensity are port-shadowed — wire the HUD's shared amount control into alpha.",
    examples: [],
    picker: { label: "Draw Scanlines", category: Atom },
    summary: "Adds faint monitor-style scanlines across the whole image.",
    category: Stylize,
    role: Filter,
    aliases: ["draw scanlines", "hud", "overlay", "scanline", "crt"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/draw_scanlines_body.wgsl"),
}

impl Primitive for DrawScanlines {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let color = match ctx.params.get("color") {
            Some(ParamValue::Color(c)) => [c[0], c[1], c[2], 1.0],
            _ => [0.85, 0.92, 1.0, 1.0],
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
        // Codegen path (mandatory for per-element GPU atoms, D3/BUG-114): the
        // kernel is generated from `wgsl_body` so the atom fuses.
        let pipeline = self.pipeline.get_or_insert_with(|| {
            let wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                .expect("node.draw_scanlines standalone codegen");
            gpu.device.create_compute_pipeline(
                &wgsl,
                crate::node_graph::freeze::codegen::ENTRY,
                "node.draw_scanlines",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        // Uniform layout matches the generated Params struct: PARAMS order
        // (color → vec4, alpha, period_px, intensity) — 7 header words + 1
        // pad = 8 words.
        let uniforms = ScanlinesUniforms { color, alpha, period_px, intensity, _pad0: 0 };

        // Bindings match the generated standalone layout: uniform(0),
        // texture input `in`(1), sampler(2), output(3) — no array input.
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

