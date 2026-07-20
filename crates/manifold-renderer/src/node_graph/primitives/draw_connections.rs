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
        // `shaders/draw_connections.wgsl` (the hand-kernel parity oracle)
        // was deleted 2026-07-20 (W1-B, migration scaffolding retired).
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

