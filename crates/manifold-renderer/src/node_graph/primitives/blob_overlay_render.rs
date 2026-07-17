//! `node.blob_overlay` — draw hollow rectangles around each
//! blob in an `Array<Blob>` on top of a source Texture2D.
//!
//! Companion to `node.blob_tracker`. Minimal box-drawing
//! variant — not the full HUD pipeline (brackets / crosshairs /
//! labels) that the legacy `node.blob_tracking` ships. Agents
//! compose richer overlays themselves by chaining multiple
//! draw primitives, OR by using the legacy wrapper for that
//! full visual treatment.

use std::borrow::Cow;

use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Generated-codegen uniform layout: PARAMS order — `color` (Color param →
/// 4 consecutive f32 fields, reassembled as `vec4<f32>` at the body call
/// site), `alpha`, `border_width`, `blob_count` (Int → i32) — then padded to
/// a 16-byte multiple (7 header words + 1 pad = 8 words = 32 bytes). NOT
/// the pre-conversion hand layout (`vec3<f32>` + separate alpha, `u32`
/// blob_count); the `[f32; 4]` here matches the codegen's 4 scalar fields
/// byte-for-byte, and `blob_count` is `i32` to match `param_wgsl_type`'s
/// mapping for `ParamType::Int` (the bit pattern is identical for the
/// non-negative range this param actually takes).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct OverlayUniforms {
    color: [f32; 4],
    alpha: f32,
    border_width: f32,
    blob_count: i32,
    _pad0: u32,
}

crate::primitive! {
    name: BlobOverlayRender,
    type_id: "node.blob_overlay",
    purpose: "Draw hollow rectangles around each blob in an Array<Blob> on top of a source Texture2D. Companion to node.blob_tracker for sparse blob visualisation. Minimal box-drawing — for the full HUD treatment (brackets, crosshairs, ticks, labels) use the legacy node.blob_tracking wrapper.",
    inputs: {
        in: Texture2D required,
        // Phase 4b: typed Channels signature matching what
        // node.blob_tracker emits. The wire's byte layout (16 bytes:
        // x, y, width, height as f32, 4-byte aligned) is the public
        // contract; the consumer reads it directly from the bound
        // buffer in the WGSL shader.
        blobs: Channels[X: F32, Y: F32, WIDTH: F32, HEIGHT: F32] required,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("color"),
            label: "Color",
            ty: ParamType::Color,
            default: ParamValue::Color([0.0, 1.0, 0.5, 1.0]),
            range: None,
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("alpha"),
            label: "Alpha",
            ty: ParamType::Float,
            default: ParamValue::Float(0.8),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("border_width"),
            label: "Border Width",
            ty: ParamType::Float,
            default: ParamValue::Float(0.003),
            range: Some((0.0005, 0.05)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("blob_count"),
            label: "Blob Count",
            ty: ParamType::Int,
            default: ParamValue::Float(32.0),
            range: Some((0.0, 32.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "Wire node.blob_tracker.blobs → this primitive's blobs port. `blob_count` is an upper bound (the shader iterates this many entries from the array but skips any with zero width/height, so it's safe to leave at 32 even if the actual detection count is lower). `border_width` is in UV units (0.003 ≈ 2px at 720p). For thicker boxes raise this; for solid filled boxes set border_width > max(blob.width, blob.height).",
    examples: [],
    picker: { label: "Blob Overlay", category: Atom },
    summary: "Draws boxes around each tracked blob on top of the image, so you can see what the Blob Tracker is finding. A debug view for blob tracking.",
    category: DetectionAndSampling,
    role: Filter,
    aliases: ["blob overlay", "blob overlay render", "tracking boxes", "debug view"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/blob_overlay_render_body.wgsl"),
    input_access: [Coincident, BufferIndex],
}

impl Primitive for BlobOverlayRender {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let color = match ctx.params.get("color") {
            Some(ParamValue::Color(c)) => [c[0], c[1], c[2], 1.0],
            _ => [0.0, 1.0, 0.5, 1.0],
        };
        let alpha = match ctx.params.get("alpha") {
            Some(ParamValue::Float(f)) => *f,
            _ => 0.8,
        };
        let border_width = match ctx.params.get("border_width") {
            Some(ParamValue::Float(f)) => *f,
            _ => 0.003,
        };
        let blob_count = match ctx.params.get("blob_count") {
            Some(ParamValue::Float(i)) => i.round().max(0_f32) as i32,
            _ => 32,
        };

        let Some(in_tex) = ctx.inputs.texture_2d("in") else {
            return;
        };
        let Some(blob_buf) = ctx.inputs.array("blobs") else {
            return;
        };
        let Some(out_tex) = ctx.outputs.texture_2d("out") else {
            return;
        };
        let (w, h) = (out_tex.width, out_tex.height);

        let gpu = ctx.gpu_encoder();
        // Codegen path (mandatory for per-element GPU atoms, D3/BUG-114): the
        // kernel is generated from `wgsl_body` so the atom fuses into a
        // texture region via the `BufferIndex` read path.
        // `shaders/blob_overlay_render.wgsl` is retained only as the
        // gpu_tests parity oracle.
        let pipeline = self.pipeline.get_or_insert_with(|| {
            let wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                .expect("node.blob_overlay standalone codegen");
            gpu.device.create_compute_pipeline(
                &wgsl,
                crate::node_graph::freeze::codegen::ENTRY,
                "node.blob_overlay",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        // Uniform layout matches the generated Params struct: PARAMS order
        // (color → vec4, alpha, border_width, blob_count) — 7 header words
        // + 1 pad = 8 words.
        let uniforms = OverlayUniforms { color, alpha, border_width, blob_count, _pad0: 0 };

        // Bindings match the generated standalone layout: uniform(0), texture
        // input `in`(1), sampler(2), array input `blobs`→`buf_blobs`(3),
        // output(4).
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Texture {
                    binding: 1,
                    texture: in_tex,
                },
                GpuBinding::Sampler {
                    binding: 2,
                    sampler,
                },
                GpuBinding::Buffer {
                    binding: 3,
                    buffer: blob_buf,
                    offset: 0,
                },
                GpuBinding::Texture {
                    binding: 4,
                    texture: out_tex,
                },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "node.blob_overlay",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn blob_overlay_render_declares_two_inputs_and_one_output() {
        use crate::node_graph::channel_names::well_known;
        use crate::node_graph::ports::{
            ArrayType, ChannelElementType, ChannelSpec, MatchMode, PortType,
        };

        const EXPECTED: &[ChannelSpec] = &[
            ChannelSpec { name: well_known::X,      ty: ChannelElementType::F32 },
            ChannelSpec { name: well_known::Y,      ty: ChannelElementType::F32 },
            ChannelSpec { name: well_known::WIDTH,  ty: ChannelElementType::F32 },
            ChannelSpec { name: well_known::HEIGHT, ty: ChannelElementType::F32 },
        ];
        let expected = ArrayType::of_channels(EXPECTED, MatchMode::Exact);

        assert_eq!(BlobOverlayRender::TYPE_ID, "node.blob_overlay");
        assert_eq!(BlobOverlayRender::INPUTS.len(), 2);
        assert_eq!(BlobOverlayRender::INPUTS[0].name, "in");
        assert_eq!(BlobOverlayRender::INPUTS[0].ty, PortType::Texture2D);
        assert_eq!(BlobOverlayRender::INPUTS[1].name, "blobs");
        assert_eq!(BlobOverlayRender::INPUTS[1].ty, PortType::Array(expected));
        assert_eq!(BlobOverlayRender::OUTPUTS.len(), 1);
        assert_eq!(BlobOverlayRender::OUTPUTS[0].ty, PortType::Texture2D);
    }

    #[test]
    fn blob_overlay_render_has_color_alpha_border_count_params() {
        let names: Vec<&str> = BlobOverlayRender::PARAMS.iter().map(|p| p.name.as_ref()).collect();
        assert_eq!(
            names,
            vec!["color", "alpha", "border_width", "blob_count"]
        );
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = BlobOverlayRender::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.blob_overlay");
    }

    #[test]
    fn uniforms_are_32_bytes() {
        assert_eq!(std::mem::size_of::<OverlayUniforms>(), 32);
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    //! **Generated-vs-hand parity** (D3, BUG-114 — `docs/ADDING_PRIMITIVES.md`
    //! "The codegen path is mandatory"): the standalone kernel `run()`
    //! actually dispatches (built via `standalone_for_spec::<BlobOverlayRender>()`)
    //! must reproduce `shaders/blob_overlay_render.wgsl` (the hand oracle)
    //! texel-for-texel.
    use manifold_gpu::{
        GpuBinding, GpuDevice, GpuSamplerDesc, GpuTexture, GpuTextureDesc, GpuTextureDimension,
        GpuTextureFormat, GpuTextureUsage,
    };

    use super::{BlobOverlayRender, OverlayUniforms};
    use crate::render_target::RenderTarget;

    #[repr(C)]
    #[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
    struct Blob {
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
            label: "blob-overlay-source",
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
        let mut enc = device.create_encoder("blob-overlay-readback");
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
    fn generated_blob_overlay_matches_hand_kernel() {
        let device = crate::test_device();
        let (w, h) = (32u32, 32u32);
        let src = solid_source(&device, w, h);

        let blobs = [
            Blob { x: 0.25, y: 0.25, width: 0.2, height: 0.2 },
            Blob { x: 0.6, y: 0.5, width: 0.15, height: 0.15 },
            // A zeroed slot — the `<= 0.0001` continue-guard, exercised the
            // same for both kernels.
            Blob { x: 0.0, y: 0.0, width: 0.0, height: 0.0 },
        ];
        let blob_bytes_len = std::mem::size_of_val(&blobs) as u64;
        let hand_buf = device.create_buffer_shared(blob_bytes_len);
        let gen_buf = device.create_buffer_shared(blob_bytes_len);
        unsafe {
            hand_buf.write(0, bytemuck::bytes_of(&blobs));
            gen_buf.write(0, bytemuck::bytes_of(&blobs));
        }

        let color = [0.0_f32, 1.0, 0.5, 1.0];
        let alpha = 0.8_f32;
        let border_width = 0.02_f32;
        let blob_count = 3_i32;

        // Hand layout (`shaders/blob_overlay_render.wgsl`'s `struct
        // Uniforms`): overlay_color as vec3<f32> + alpha/border_width +
        // blob_count as u32 + 2×u32 pad — NOT the generated Params layout
        // (`OverlayUniforms`, PARAMS order: color as 4×f32 then
        // alpha/border_width/blob_count(i32) + 1×u32 pad).
        let mut hand_bytes = Vec::new();
        hand_bytes.extend_from_slice(&color[0].to_le_bytes());
        hand_bytes.extend_from_slice(&color[1].to_le_bytes());
        hand_bytes.extend_from_slice(&color[2].to_le_bytes());
        hand_bytes.extend_from_slice(&alpha.to_le_bytes());
        hand_bytes.extend_from_slice(&border_width.to_le_bytes());
        hand_bytes.extend_from_slice(&(blob_count as u32).to_le_bytes());
        hand_bytes.extend_from_slice(&[0u8; 8]); // 2×u32 pad

        let gen_uniforms = OverlayUniforms { color, alpha, border_width, blob_count, _pad0: 0 };
        let gen_bytes = bytemuck::bytes_of(&gen_uniforms).to_vec();

        let hand_wgsl = include_str!("shaders/blob_overlay_render.wgsl");
        let hand_pipeline = device.create_compute_pipeline(hand_wgsl, "cs_main", "blob-overlay-hand");
        let gen_wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<BlobOverlayRender>()
            .expect("node.blob_overlay standalone codegen");
        let gen_pipeline = device.create_compute_pipeline(
            &gen_wgsl,
            crate::node_graph::freeze::codegen::ENTRY,
            "blob-overlay-generated",
        );

        let sampler = device.create_sampler(&GpuSamplerDesc::default());

        let hand_out = RenderTarget::new(&device, w, h, GpuTextureFormat::Rgba16Float, "hand-out");
        let mut enc = device.create_encoder("blob-overlay-hand-dispatch");
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
            "blob-overlay-hand-dispatch",
        );
        enc.commit_and_wait_completed();

        let gen_out = RenderTarget::new(&device, w, h, GpuTextureFormat::Rgba16Float, "gen-out");
        let mut enc = device.create_encoder("blob-overlay-gen-dispatch");
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
            "blob-overlay-gen-dispatch",
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
