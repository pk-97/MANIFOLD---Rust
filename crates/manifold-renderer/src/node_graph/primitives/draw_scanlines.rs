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
        // `shaders/draw_scanlines.wgsl` is retained only as the gpu_tests
        // parity oracle.
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

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    //! **Generated-vs-hand parity** (D3, BUG-114 — `docs/ADDING_PRIMITIVES.md`
    //! "The codegen path is mandatory"): the standalone kernel `run()`
    //! actually dispatches (built via `standalone_for_spec::<DrawScanlines>()`)
    //! must reproduce `shaders/draw_scanlines.wgsl` (the hand oracle)
    //! texel-for-texel. No array input on this atom — its only P4b-relevant
    //! change is the wgsl_body conversion; it needed P5's Color-param lift,
    //! not the BufferIndex mechanism.
    use manifold_gpu::{
        GpuBinding, GpuDevice, GpuSamplerDesc, GpuTexture, GpuTextureDesc, GpuTextureDimension,
        GpuTextureFormat, GpuTextureUsage,
    };

    use super::{DrawScanlines, ScanlinesUniforms};
    use crate::render_target::RenderTarget;

    fn solid_source(device: &GpuDevice, w: u32, h: u32) -> GpuTexture {
        use half::f16;
        let mut px = vec![f16::from_f32(0.0); (w * h * 4) as usize];
        for i in 0..(w * h) as usize {
            px[i * 4] = f16::from_f32(0.05);
            px[i * 4 + 1] = f16::from_f32(0.05);
            px[i * 4 + 2] = f16::from_f32(0.05);
            px[i * 4 + 3] = f16::from_f32(1.0);
        }
        let tex = device.create_texture(&GpuTextureDesc {
            width: w,
            height: h,
            depth: 1,
            format: GpuTextureFormat::Rgba16Float,
            dimension: GpuTextureDimension::D2,
            usage: GpuTextureUsage::CPU_UPLOAD | GpuTextureUsage::SHADER_READ,
            label: "draw-scanlines-source",
            mip_levels: 1,
        });
        let bytes =
            unsafe { std::slice::from_raw_parts(px.as_ptr().cast::<u8>(), std::mem::size_of_val(px.as_slice())) };
        device.upload_texture(&tex, bytes);
        tex
    }

    fn readback_rgba(device: &GpuDevice, tex: &GpuTexture, w: u32, h: u32) -> Vec<[f32; 4]> {
        use half::f16;
        let bytes_per_row = w * 8;
        let total = u64::from(h * bytes_per_row);
        let readback = device.create_buffer_shared(total);
        let mut enc = device.create_encoder("draw-scanlines-readback");
        enc.copy_texture_to_buffer(tex, &readback, w, h, bytes_per_row);
        enc.commit_and_wait_completed();
        let ptr = readback.mapped_ptr().expect("shared readback buffer");
        let halves: &[u16] =
            unsafe { std::slice::from_raw_parts(ptr.cast::<u16>(), (w * h * 4) as usize) };
        (0..(w * h) as usize)
            .map(|i| {
                let o = i * 4;
                [
                    f16::from_bits(halves[o]).to_f32(),
                    f16::from_bits(halves[o + 1]).to_f32(),
                    f16::from_bits(halves[o + 2]).to_f32(),
                    f16::from_bits(halves[o + 3]).to_f32(),
                ]
            })
            .collect()
    }

    #[test]
    fn generated_draw_scanlines_matches_hand_kernel() {
        let device = crate::test_device();
        let (w, h) = (32u32, 32u32);
        let src = solid_source(&device, w, h);

        let color = [0.85_f32, 0.92, 1.0, 1.0];
        let alpha = 1.0_f32;
        let period_px = 2.0_f32;
        let intensity = 0.04_f32;

        // Hand layout (`shaders/draw_scanlines.wgsl`'s `struct U`): color as
        // vec3<f32> + alpha/period/intensity + 2×u32 pad — NOT the generated
        // Params layout (`ScanlinesUniforms`, PARAMS order: color as 4×f32
        // then alpha/period/intensity + 1×u32 pad).
        let mut hand_bytes = Vec::new();
        hand_bytes.extend_from_slice(&color[0].to_le_bytes());
        hand_bytes.extend_from_slice(&color[1].to_le_bytes());
        hand_bytes.extend_from_slice(&color[2].to_le_bytes());
        hand_bytes.extend_from_slice(&alpha.to_le_bytes());
        hand_bytes.extend_from_slice(&period_px.to_le_bytes());
        hand_bytes.extend_from_slice(&intensity.to_le_bytes());
        hand_bytes.extend_from_slice(&[0u8; 8]); // 2×u32 pad

        let gen_uniforms = ScanlinesUniforms { color, alpha, period_px, intensity, _pad0: 0 };
        let gen_bytes = bytemuck::bytes_of(&gen_uniforms).to_vec();

        let hand_wgsl = include_str!("shaders/draw_scanlines.wgsl");
        let hand_pipeline = device.create_compute_pipeline(hand_wgsl, "cs_main", "draw-scanlines-hand");
        let gen_wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<DrawScanlines>()
            .expect("node.draw_scanlines standalone codegen");
        let gen_pipeline = device.create_compute_pipeline(
            &gen_wgsl,
            crate::node_graph::freeze::codegen::ENTRY,
            "draw-scanlines-generated",
        );

        let sampler = device.create_sampler(&GpuSamplerDesc::default());

        let hand_out = RenderTarget::new(&device, w, h, GpuTextureFormat::Rgba16Float, "hand-out");
        let mut enc = device.create_encoder("draw-scanlines-hand-dispatch");
        enc.dispatch_compute(
            &hand_pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: &hand_bytes },
                GpuBinding::Texture { binding: 1, texture: &src },
                GpuBinding::Sampler { binding: 2, sampler: &sampler },
                GpuBinding::Texture { binding: 3, texture: &hand_out.texture },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "draw-scanlines-hand-dispatch",
        );
        enc.commit_and_wait_completed();

        let gen_out = RenderTarget::new(&device, w, h, GpuTextureFormat::Rgba16Float, "gen-out");
        let mut enc = device.create_encoder("draw-scanlines-gen-dispatch");
        enc.dispatch_compute(
            &gen_pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: &gen_bytes },
                GpuBinding::Texture { binding: 1, texture: &src },
                GpuBinding::Sampler { binding: 2, sampler: &sampler },
                GpuBinding::Texture { binding: 3, texture: &gen_out.texture },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "draw-scanlines-gen-dispatch",
        );
        enc.commit_and_wait_completed();

        let hand_px = readback_rgba(&device, &hand_out.texture, w, h);
        let gen_px = readback_rgba(&device, &gen_out.texture, w, h);
        for (i, (hp, gp)) in hand_px.iter().zip(gen_px.iter()).enumerate() {
            for c in 0..4 {
                assert!(
                    (hp[c] - gp[c]).abs() < 1e-5,
                    "texel={i} ch={c}: hand={} gen={}",
                    hp[c],
                    gp[c]
                );
            }
        }
    }
}
