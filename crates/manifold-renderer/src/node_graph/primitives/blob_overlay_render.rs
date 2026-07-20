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
        // `shaders/blob_overlay_render.wgsl` (the hand-kernel parity oracle)
        // was deleted 2026-07-20 (W1-B, migration scaffolding retired).
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

