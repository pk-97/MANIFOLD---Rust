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

/// Generated-codegen uniform layout: PARAMS order — `symbol` (Enum → u32),
/// `color` (Color param → 4 consecutive f32 fields, reassembled as
/// `vec4<f32>` at the body call site), `alpha`, `size_fraction`,
/// `thickness_px` — 8 header words, already a 16-byte multiple (no pad). NOT
/// the pre-conversion hand layout (`vec3<f32>` color + trailing `symbol`);
/// the `[f32; 4]` here matches the codegen's 4 scalar fields byte-for-byte.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct MarkersUniforms {
    symbol: u32,
    color: [f32; 4],
    alpha: f32,
    size_fraction: f32,
    thickness_px: f32,
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
    depth_rule: Terminal,
    composition_notes: "Wire a detector chain (node.blob_tracker → node.track_persist → node.one_euro_filter) into `detections` and the video into `in`. Stack multiple instances for layered HUDs (brackets + crosshair = the Blob Track look: brackets at size_fraction 0.4 / thickness 2, crosshair at 0.3 / 1.5). alpha is port-shadowed — wire one amount control into every Draw node's alpha to fade the whole HUD. Skips to a zero-cost passthrough while the detector reports nothing.",
    examples: [],
    picker: { label: "Draw Markers", category: Atom },
    summary: "Draws a marker on every tracked object: corner brackets around it or a crosshair at its centre. The building block for tracking overlays.",
    category: DetectionAndSampling,
    role: Filter,
    aliases: ["draw markers", "hud", "overlay", "brackets", "crosshair", "tracking marker"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/draw_markers_body.wgsl"),
    input_access: [Coincident, BufferIndex],
}

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
            Some(ParamValue::Color(c)) => [c[0], c[1], c[2], 1.0],
            _ => [0.85, 0.92, 1.0, 1.0],
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
        // Codegen path (mandatory for per-element GPU atoms, D3/BUG-114): the
        // kernel is generated from `wgsl_body` so the atom fuses into a
        // texture region via the `BufferIndex` read path.
        // `shaders/draw_markers.wgsl` is retained only as the gpu_tests
        // parity oracle.
        let pipeline = self.pipeline.get_or_insert_with(|| {
            let wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                .expect("node.draw_markers standalone codegen");
            gpu.device.create_compute_pipeline(
                &wgsl,
                crate::node_graph::freeze::codegen::ENTRY,
                "node.draw_markers",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        // Uniform layout matches the generated Params struct: PARAMS order
        // (symbol, color → vec4, alpha, size_fraction, thickness_px), no
        // injected fields — 8 words, no pad.
        let uniforms = MarkersUniforms { symbol, color, alpha, size_fraction, thickness_px };

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

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    //! **Generated-vs-hand parity** (D3, BUG-114 — `docs/ADDING_PRIMITIVES.md`
    //! "The codegen path is mandatory"): the standalone kernel `run()`
    //! actually dispatches (built via `standalone_for_spec::<DrawMarkers>()`)
    //! must reproduce `shaders/draw_markers.wgsl` (the hand oracle) texel-for-texel.
    use manifold_gpu::{
        GpuBinding, GpuDevice, GpuSamplerDesc, GpuTexture, GpuTextureDesc, GpuTextureDimension,
        GpuTextureFormat, GpuTextureUsage,
    };

    use super::{DrawMarkers, MarkersUniforms};
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
            label: "draw-markers-source",
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
        let mut enc = device.create_encoder("draw-markers-readback");
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

    fn run_both(symbol: u32) {
        let device = crate::test_device();
        let (w, h) = (32u32, 32u32);
        let src = solid_source(&device, w, h);

        let detections = [
            Detection { x: 0.25, y: 0.25, width: 0.1, height: 0.1 },
            Detection { x: 0.75, y: 0.6, width: 0.12, height: 0.12 },
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
        let size_fraction = 0.4_f32;
        let thickness_px = 2.0_f32;

        // Hand layout (`shaders/draw_markers.wgsl`'s `struct U`): color as
        // vec3<f32> + alpha + size_fraction + thickness_px + symbol + pad —
        // NOT the generated Params layout (`MarkersUniforms`, PARAMS order:
        // symbol, color as 4×f32, alpha, size_fraction, thickness_px).
        let mut hand_bytes = Vec::new();
        hand_bytes.extend_from_slice(&color[0].to_le_bytes());
        hand_bytes.extend_from_slice(&color[1].to_le_bytes());
        hand_bytes.extend_from_slice(&color[2].to_le_bytes());
        hand_bytes.extend_from_slice(&alpha.to_le_bytes());
        hand_bytes.extend_from_slice(&size_fraction.to_le_bytes());
        hand_bytes.extend_from_slice(&thickness_px.to_le_bytes());
        hand_bytes.extend_from_slice(&symbol.to_le_bytes());
        hand_bytes.extend_from_slice(&0u32.to_le_bytes());

        let gen_uniforms =
            MarkersUniforms { symbol, color, alpha, size_fraction, thickness_px };
        let gen_bytes = bytemuck::bytes_of(&gen_uniforms).to_vec();

        let hand_wgsl = include_str!("shaders/draw_markers.wgsl");
        let hand_pipeline = device.create_compute_pipeline(hand_wgsl, "cs_main", "draw-markers-hand");
        let gen_wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<DrawMarkers>()
            .expect("node.draw_markers standalone codegen");
        let gen_pipeline = device.create_compute_pipeline(
            &gen_wgsl,
            crate::node_graph::freeze::codegen::ENTRY,
            "draw-markers-generated",
        );

        let sampler = device.create_sampler(&GpuSamplerDesc::default());

        let hand_out = RenderTarget::new(&device, w, h, GpuTextureFormat::Rgba16Float, "hand-out");
        let mut enc = device.create_encoder("draw-markers-hand-dispatch");
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
            "draw-markers-hand-dispatch",
        );
        enc.commit_and_wait_completed();

        let gen_out = RenderTarget::new(&device, w, h, GpuTextureFormat::Rgba16Float, "gen-out");
        let mut enc = device.create_encoder("draw-markers-gen-dispatch");
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
            "draw-markers-gen-dispatch",
        );
        enc.commit_and_wait_completed();

        let hand_px = readback_rgba(&device, &hand_out.texture, w, h);
        let gen_px = readback_rgba(&device, &gen_out.texture, w, h);
        for (i, (hp, gp)) in hand_px.iter().zip(gen_px.iter()).enumerate() {
            for c in 0..4 {
                assert!(
                    (hp[c] - gp[c]).abs() < 1e-5,
                    "symbol={symbol} texel={i} ch={c}: hand={} gen={}",
                    hp[c],
                    gp[c]
                );
            }
        }
    }

    #[test]
    fn generated_draw_markers_matches_hand_kernel_corner_brackets() {
        run_both(0);
    }

    #[test]
    fn generated_draw_markers_matches_hand_kernel_crosshair() {
        run_both(1);
    }
}
