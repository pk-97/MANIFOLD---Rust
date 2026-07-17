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

/// Generated-codegen uniform layout: PARAMS order — `color` (Color param →
/// 4 consecutive f32 fields, reassembled as `vec4<f32>` at the body call
/// site), `alpha`, `thickness_px`, `dash_period_px`, `dash_fill`,
/// `midpoint_radius_px` — then padded to a 16-byte multiple (9 header words
/// plus 3 pad words = 12 words = 48 bytes total). NOT the pre-conversion
/// hand layout (`vec3<f32>` plus a separate alpha, no explicit pad); the
/// `[f32; 4]` here matches the codegen's 4 scalar fields byte-for-byte.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ConnectionsUniforms {
    color: [f32; 4],
    alpha: f32,
    thickness_px: f32,
    dash_period_px: f32,
    dash_fill: f32,
    midpoint_radius_px: f32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
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
    depth_rule: Terminal,
    composition_notes: "Wire node.connect_nearest's edges output into `edges` and the same detections it consumed into `detections`. dash_fill is the OFF fraction of each dash cycle (legacy step semantics); set midpoint_radius_px to 0 for plain lines. Skips to a zero-cost passthrough while no pairs exist.",
    examples: [],
    picker: { label: "Draw Connections", category: Atom },
    summary: "Draws dashed lines linking tracked objects that are near each other, with an optional dot at the middle of each link.",
    category: DetectionAndSampling,
    role: Filter,
    aliases: ["draw connections", "hud", "overlay", "connection lines", "links", "constellation"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/draw_connections_body.wgsl"),
    input_access: [Coincident, BufferIndex, BufferIndex],
}

impl Primitive for DrawConnections {
    fn empty_skip_input_ports(&self) -> &'static [&'static str] {
        &["edges"]
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
        // Codegen path (mandatory for per-element GPU atoms, D3/BUG-114): the
        // kernel is generated from `wgsl_body` so the atom fuses into a
        // texture region via the `BufferIndex` read path — this atom
        // exercises TWO BufferIndex-tagged array inputs (detections + edges),
        // the generic mechanism P4a built, not just one.
        // `shaders/draw_connections.wgsl` is retained only as the gpu_tests
        // parity oracle.
        let pipeline = self.pipeline.get_or_insert_with(|| {
            let wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                .expect("node.draw_connections standalone codegen");
            gpu.device.create_compute_pipeline(
                &wgsl,
                crate::node_graph::freeze::codegen::ENTRY,
                "node.draw_connections",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        // Uniform layout matches the generated Params struct: PARAMS order
        // (color → vec4, alpha, thickness_px, dash_period_px, dash_fill,
        // midpoint_radius_px) — 9 header words + 3 pad = 12 words.
        let uniforms = ConnectionsUniforms {
            color,
            alpha,
            thickness_px,
            dash_period_px,
            dash_fill,
            midpoint_radius_px,
            _pad0: 0,
            _pad1: 0,
            _pad2: 0,
        };

        // Bindings match the generated standalone layout: uniform(0), texture
        // input `in`(1), sampler(2), array inputs in declaration order —
        // `detections`→`buf_detections`(3), `edges`→`buf_edges`(4), output(5).
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                GpuBinding::Texture { binding: 1, texture: in_tex },
                GpuBinding::Sampler { binding: 2, sampler },
                GpuBinding::Buffer { binding: 3, buffer: det_buf, offset: 0 },
                GpuBinding::Buffer { binding: 4, buffer: edge_buf, offset: 0 },
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
    fn uniforms_are_48_bytes() {
        assert_eq!(std::mem::size_of::<ConnectionsUniforms>(), 48);
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    //! **Generated-vs-hand parity** (D3, BUG-114 — `docs/ADDING_PRIMITIVES.md`
    //! "The codegen path is mandatory"): the standalone kernel `run()`
    //! actually dispatches (built via `standalone_for_spec::<DrawConnections>()`)
    //! must reproduce `shaders/draw_connections.wgsl` (the hand oracle)
    //! texel-for-texel. Also the proving atom for TWO BufferIndex-tagged
    //! array inputs on one atom (detections + edges) — the generic
    //! mechanism generalizes past draw_dots' single-array case.
    use manifold_gpu::{
        GpuBinding, GpuDevice, GpuSamplerDesc, GpuTexture, GpuTextureDesc, GpuTextureDimension,
        GpuTextureFormat, GpuTextureUsage,
    };

    use super::{ConnectionsUniforms, DrawConnections};
    use crate::render_target::RenderTarget;

    #[repr(C)]
    #[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
    struct Detection {
        x: f32,
        y: f32,
        width: f32,
        height: f32,
    }

    #[repr(C)]
    #[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
    struct Edge {
        a_index: u32,
        b_index: u32,
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
            label: "draw-connections-source",
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
        let mut enc = device.create_encoder("draw-connections-readback");
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
    fn generated_draw_connections_matches_hand_kernel() {
        let device = crate::test_device();
        let (w, h) = (32u32, 32u32);
        let src = solid_source(&device, w, h);

        let detections = [
            Detection { x: 0.25, y: 0.25, width: 0.1, height: 0.1 },
            Detection { x: 0.75, y: 0.6, width: 0.12, height: 0.12 },
            Detection { x: 0.4, y: 0.7, width: 0.08, height: 0.08 },
        ];
        let edges = [
            Edge { a_index: 0, b_index: 1 },
            Edge { a_index: 1, b_index: 2 },
            // Sentinel — the `0xFFFFFFFF` continue-guard, exercised the same
            // for both kernels.
            Edge { a_index: 0xFFFFFFFF, b_index: 0xFFFFFFFF },
        ];
        let det_bytes_len = std::mem::size_of_val(&detections) as u64;
        let edge_bytes_len = std::mem::size_of_val(&edges) as u64;
        let hand_det_buf = device.create_buffer_shared(det_bytes_len);
        let gen_det_buf = device.create_buffer_shared(det_bytes_len);
        let hand_edge_buf = device.create_buffer_shared(edge_bytes_len);
        let gen_edge_buf = device.create_buffer_shared(edge_bytes_len);
        unsafe {
            hand_det_buf.write(0, bytemuck::bytes_of(&detections));
            gen_det_buf.write(0, bytemuck::bytes_of(&detections));
            hand_edge_buf.write(0, bytemuck::bytes_of(&edges));
            gen_edge_buf.write(0, bytemuck::bytes_of(&edges));
        }

        let color = [0.85_f32, 0.92, 1.0, 1.0];
        let alpha = 1.0_f32;
        let thickness_px = 1.5_f32;
        let dash_period_px = 12.0_f32;
        let dash_fill = 0.4_f32;
        let midpoint_radius_px = 5.0_f32;

        // Hand layout (`shaders/draw_connections.wgsl`'s `struct U`): color
        // as vec3<f32> + the rest, no explicit pad — NOT the generated
        // Params layout (`ConnectionsUniforms`, PARAMS order: color as
        // 4×f32 then the scalar fields + 3×u32 pad).
        let mut hand_bytes = Vec::new();
        hand_bytes.extend_from_slice(&color[0].to_le_bytes());
        hand_bytes.extend_from_slice(&color[1].to_le_bytes());
        hand_bytes.extend_from_slice(&color[2].to_le_bytes());
        hand_bytes.extend_from_slice(&alpha.to_le_bytes());
        hand_bytes.extend_from_slice(&thickness_px.to_le_bytes());
        hand_bytes.extend_from_slice(&dash_period_px.to_le_bytes());
        hand_bytes.extend_from_slice(&dash_fill.to_le_bytes());
        hand_bytes.extend_from_slice(&midpoint_radius_px.to_le_bytes());

        let gen_uniforms = ConnectionsUniforms {
            color,
            alpha,
            thickness_px,
            dash_period_px,
            dash_fill,
            midpoint_radius_px,
            _pad0: 0,
            _pad1: 0,
            _pad2: 0,
        };
        let gen_bytes = bytemuck::bytes_of(&gen_uniforms).to_vec();

        let hand_wgsl = include_str!("shaders/draw_connections.wgsl");
        let hand_pipeline =
            device.create_compute_pipeline(hand_wgsl, "cs_main", "draw-connections-hand");
        let gen_wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<DrawConnections>()
            .expect("node.draw_connections standalone codegen");
        let gen_pipeline = device.create_compute_pipeline(
            &gen_wgsl,
            crate::node_graph::freeze::codegen::ENTRY,
            "draw-connections-generated",
        );

        let sampler = device.create_sampler(&GpuSamplerDesc::default());

        let hand_out = RenderTarget::new(&device, w, h, GpuTextureFormat::Rgba16Float, "hand-out");
        let mut enc = device.create_encoder("draw-connections-hand-dispatch");
        enc.dispatch_compute(
            &hand_pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: &hand_bytes },
                GpuBinding::Buffer { binding: 1, buffer: &hand_det_buf, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: &hand_edge_buf, offset: 0 },
                GpuBinding::Texture { binding: 3, texture: &src },
                GpuBinding::Sampler { binding: 4, sampler: &sampler },
                GpuBinding::Texture { binding: 5, texture: &hand_out.texture },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "draw-connections-hand-dispatch",
        );
        enc.commit_and_wait_completed();

        let gen_out = RenderTarget::new(&device, w, h, GpuTextureFormat::Rgba16Float, "gen-out");
        let mut enc = device.create_encoder("draw-connections-gen-dispatch");
        enc.dispatch_compute(
            &gen_pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: &gen_bytes },
                GpuBinding::Texture { binding: 1, texture: &src },
                GpuBinding::Sampler { binding: 2, sampler: &sampler },
                GpuBinding::Buffer { binding: 3, buffer: &gen_det_buf, offset: 0 },
                GpuBinding::Buffer { binding: 4, buffer: &gen_edge_buf, offset: 0 },
                GpuBinding::Texture { binding: 5, texture: &gen_out.texture },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "draw-connections-gen-dispatch",
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
