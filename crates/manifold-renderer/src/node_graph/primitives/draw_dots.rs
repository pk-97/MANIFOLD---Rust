//! `node.draw_dots` — soft dot at every detection's centre, composited
//! additively onto a source texture. Coverage falls off linearly with
//! distance, giving a small glow rather than a hard disc. Math ported
//! verbatim from the Blob Track HUD's `center_dot` wgsl_compute kernel.

use std::borrow::Cow;
use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Generated-codegen uniform layout: PARAMS order — `color` (Color param →
/// 4 consecutive f32 fields, reassembled as `vec4<f32>` at the body call
/// site), `alpha`, `radius_px` — then padded to a 16-byte multiple (6 header
/// words + 2 pad = 8 words = 32 bytes). NOT the pre-conversion hand layout
/// (`vec3<f32>` + separate alpha); the `[f32; 4]` here matches the codegen's
/// 4 scalar fields byte-for-byte (no vec3-alignment gap).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct DotsUniforms {
    color: [f32; 4],
    alpha: f32,
    radius_px: f32,
    _pad0: u32,
    _pad1: u32,
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
    depth_rule: Terminal,
    composition_notes: "Wire detections into `detections` and the video into `in`; stacks with node.draw_markers / node.draw_ticks for a full HUD (Blob Track uses radius_px 4). alpha is port-shadowed for a shared HUD fade control. Skips to a zero-cost passthrough while the detector reports nothing.",
    examples: [],
    picker: { label: "Draw Dots", category: Atom },
    summary: "Draws a small glowing dot at the centre of every tracked object.",
    category: DetectionAndSampling,
    role: Filter,
    aliases: ["draw dots", "hud", "overlay", "center dot", "points"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/draw_dots_body.wgsl"),
    input_access: [Coincident, BufferIndex],
}

impl Primitive for DrawDots {
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
        // Codegen path (mandatory for per-element GPU atoms, D3/BUG-114): the
        // kernel is generated from `wgsl_body` so the atom fuses into a
        // texture region via the `BufferIndex` read path. `shaders/draw_dots.wgsl`
        // is retained only as the gpu_tests parity oracle.
        let pipeline = self.pipeline.get_or_insert_with(|| {
            let wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                .expect("node.draw_dots standalone codegen");
            gpu.device.create_compute_pipeline(
                &wgsl,
                crate::node_graph::freeze::codegen::ENTRY,
                "node.draw_dots",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        // Uniform layout matches the generated Params struct: PARAMS order
        // (color → vec4, alpha, radius_px), no injected fields (no derived
        // uniforms, no multi-output, no optional textures) — 8 words, no pad.
        let uniforms = DotsUniforms { color, alpha, radius_px, _pad0: 0, _pad1: 0 };

        // Bindings match the generated standalone layout: uniform(0), texture
        // input `in`(1), sampler(2), array input `detections`→`buf_detections`(3),
        // output(4) — texture inputs bind before the array input in this
        // codegen path (texture is the atom's primary domain).
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

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    //! **Generated-vs-hand parity** (D3, BUG-114 — `docs/ADDING_PRIMITIVES.md`
    //! "The codegen path is mandatory"): the standalone kernel `run()`
    //! actually dispatches (built via `standalone_for_spec::<DrawDots>()`)
    //! must reproduce `shaders/draw_dots.wgsl` (the hand oracle, kept
    //! byte-for-byte identical to the pre-conversion kernel) texel-for-texel.
    //! This is also the proving atom for the `BufferIndex` read path: the
    //! generated kernel binds `buf_detections` as a `var<storage, read>`
    //! array the body indexes directly (no pre-read, no arg) — a shape no
    //! prior texture-domain atom exercised.
    use manifold_gpu::{
        GpuBinding, GpuDevice, GpuSamplerDesc, GpuTexture, GpuTextureDesc, GpuTextureDimension,
        GpuTextureFormat, GpuTextureUsage,
    };

    use super::{DotsUniforms, DrawDots};
    use crate::render_target::RenderTarget;

    #[repr(C)]
    #[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
    struct Detection {
        x: f32,
        y: f32,
        width: f32,
        height: f32,
    }

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
            label: "draw-dots-source",
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
        let mut enc = device.create_encoder("draw-dots-readback");
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
    fn generated_draw_dots_matches_hand_kernel() {
        let device = crate::test_device();
        let (w, h) = (32u32, 32u32);
        let src = solid_source(&device, w, h);

        let detections = [
            Detection { x: 0.25, y: 0.25, width: 0.05, height: 0.05 },
            Detection { x: 0.75, y: 0.6, width: 0.08, height: 0.08 },
            // A zeroed (width/height < 0.0001) trailing slot — the coverage
            // loop's `continue` guard, exercised the same for both kernels.
            Detection { x: 0.0, y: 0.0, width: 0.0, height: 0.0 },
        ];
        let det_bytes_len = std::mem::size_of_val(&detections) as u64;
        let hand_buf = device.create_buffer_shared(det_bytes_len);
        let gen_buf = device.create_buffer_shared(det_bytes_len);
        unsafe {
            hand_buf.write(0, bytemuck::bytes_of(&detections));
            gen_buf.write(0, bytemuck::bytes_of(&detections));
        }

        let color = [0.85_f32, 0.92, 1.0, 1.0];
        let alpha = 1.0_f32;
        let radius_px = 4.0_f32;

        // Hand layout (`shaders/draw_dots.wgsl`'s `struct U`): color as
        // vec3<f32> + alpha + radius_px + 3×u32 pad — NOT the generated
        // Params layout (`DotsUniforms`, PARAMS order: color as 4×f32 then
        // alpha/radius_px), so the two byte buffers are built separately.
        let mut hand_bytes = Vec::new();
        hand_bytes.extend_from_slice(&color[0].to_le_bytes());
        hand_bytes.extend_from_slice(&color[1].to_le_bytes());
        hand_bytes.extend_from_slice(&color[2].to_le_bytes());
        hand_bytes.extend_from_slice(&alpha.to_le_bytes());
        hand_bytes.extend_from_slice(&radius_px.to_le_bytes());
        hand_bytes.extend_from_slice(&[0u8; 12]); // 3×u32 pad

        let gen_uniforms =
            DotsUniforms { color, alpha, radius_px, _pad0: 0, _pad1: 0 };
        let gen_bytes = bytemuck::bytes_of(&gen_uniforms).to_vec();

        let hand_wgsl = include_str!("shaders/draw_dots.wgsl");
        let hand_pipeline = device.create_compute_pipeline(hand_wgsl, "cs_main", "draw-dots-hand");
        let gen_wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<DrawDots>()
            .expect("node.draw_dots standalone codegen");
        let gen_pipeline = device.create_compute_pipeline(
            &gen_wgsl,
            crate::node_graph::freeze::codegen::ENTRY,
            "draw-dots-generated",
        );

        let sampler = device.create_sampler(&GpuSamplerDesc::default());

        let hand_out = RenderTarget::new(&device, w, h, GpuTextureFormat::Rgba16Float, "hand-out");
        let mut enc = device.create_encoder("draw-dots-hand-dispatch");
        enc.dispatch_compute(
            &hand_pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: &hand_bytes },
                GpuBinding::Buffer { binding: 1, buffer: &hand_buf, offset: 0 },
                GpuBinding::Texture { binding: 2, texture: &src },
                GpuBinding::Sampler { binding: 3, sampler: &sampler },
                GpuBinding::Texture { binding: 4, texture: &hand_out.texture },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "draw-dots-hand-dispatch",
        );
        enc.commit_and_wait_completed();

        let gen_out = RenderTarget::new(&device, w, h, GpuTextureFormat::Rgba16Float, "gen-out");
        let mut enc = device.create_encoder("draw-dots-gen-dispatch");
        enc.dispatch_compute(
            &gen_pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: &gen_bytes },
                GpuBinding::Texture { binding: 1, texture: &src },
                GpuBinding::Sampler { binding: 2, sampler: &sampler },
                GpuBinding::Buffer { binding: 3, buffer: &gen_buf, offset: 0 },
                GpuBinding::Texture { binding: 4, texture: &gen_out.texture },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "draw-dots-gen-dispatch",
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
