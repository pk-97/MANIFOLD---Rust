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
        // is retained only as the gpu_tests parity oracle.
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

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    //! **Generated-vs-hand parity** (D3, BUG-114 — `docs/ADDING_PRIMITIVES.md`
    //! "The codegen path is mandatory"): the standalone kernel `run()`
    //! actually dispatches (built via `standalone_for_spec::<DrawGauge>()`)
    //! must reproduce `shaders/draw_gauge.wgsl` (the hand oracle) texel-for-texel.
    use manifold_gpu::{
        GpuBinding, GpuDevice, GpuSamplerDesc, GpuTexture, GpuTextureDesc, GpuTextureDimension,
        GpuTextureFormat, GpuTextureUsage,
    };

    use super::{DrawGauge, GaugeUniforms};
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
            label: "draw-gauge-source",
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
        let mut enc = device.create_encoder("draw-gauge-readback");
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
    fn generated_draw_gauge_matches_hand_kernel() {
        let device = crate::test_device();
        let (w, h) = (32u32, 32u32);
        let src = solid_source(&device, w, h);

        let detections = [
            Detection { x: 0.25, y: 0.25, width: 0.1, height: 0.1 },
            Detection { x: 0.6, y: 0.5, width: 0.08, height: 0.08 },
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
        let bottom_offset_px = 50.0_f32;
        let bar_height_px = 8.0_f32;
        let min_bar_width_px = 80.0_f32;
        let fill_scale = 20.0_f32;
        let thickness_px = 1.5_f32;

        // Hand layout (`shaders/draw_gauge.wgsl`'s `struct U`): color as
        // vec3<f32> + alpha + rest + 3×u32 pad — NOT the generated Params
        // layout (`GaugeUniforms`, PARAMS order: color as 4×f32 then the
        // scalar fields + 2×u32 pad).
        let mut hand_bytes = Vec::new();
        hand_bytes.extend_from_slice(&color[0].to_le_bytes());
        hand_bytes.extend_from_slice(&color[1].to_le_bytes());
        hand_bytes.extend_from_slice(&color[2].to_le_bytes());
        hand_bytes.extend_from_slice(&alpha.to_le_bytes());
        hand_bytes.extend_from_slice(&bottom_offset_px.to_le_bytes());
        hand_bytes.extend_from_slice(&bar_height_px.to_le_bytes());
        hand_bytes.extend_from_slice(&min_bar_width_px.to_le_bytes());
        hand_bytes.extend_from_slice(&fill_scale.to_le_bytes());
        hand_bytes.extend_from_slice(&thickness_px.to_le_bytes());
        hand_bytes.extend_from_slice(&[0u8; 12]); // 3×u32 pad

        let gen_uniforms = GaugeUniforms {
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
        let gen_bytes = bytemuck::bytes_of(&gen_uniforms).to_vec();

        let hand_wgsl = include_str!("shaders/draw_gauge.wgsl");
        let hand_pipeline = device.create_compute_pipeline(hand_wgsl, "cs_main", "draw-gauge-hand");
        let gen_wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<DrawGauge>()
            .expect("node.draw_gauge standalone codegen");
        let gen_pipeline = device.create_compute_pipeline(
            &gen_wgsl,
            crate::node_graph::freeze::codegen::ENTRY,
            "draw-gauge-generated",
        );

        let sampler = device.create_sampler(&GpuSamplerDesc::default());

        let hand_out = RenderTarget::new(&device, w, h, GpuTextureFormat::Rgba16Float, "hand-out");
        let mut enc = device.create_encoder("draw-gauge-hand-dispatch");
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
            "draw-gauge-hand-dispatch",
        );
        enc.commit_and_wait_completed();

        let gen_out = RenderTarget::new(&device, w, h, GpuTextureFormat::Rgba16Float, "gen-out");
        let mut enc = device.create_encoder("draw-gauge-gen-dispatch");
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
            "draw-gauge-gen-dispatch",
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
