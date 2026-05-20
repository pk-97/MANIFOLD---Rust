//! `node.blob_overlay_render` — draw hollow rectangles around each
//! blob in an `Array<Blob>` on top of a source Texture2D.
//!
//! Companion to `node.blob_detect_ffi`. Minimal box-drawing
//! variant — not the full HUD pipeline (brackets / crosshairs /
//! labels) that the legacy `node.blob_tracking` ships. Agents
//! compose richer overlays themselves by chaining multiple
//! draw primitives, OR by using the legacy wrapper for that
//! full visual treatment.

use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::generators::mesh_common::Blob;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct OverlayUniforms {
    overlay_color: [f32; 3],
    alpha: f32,
    border_width: f32,
    blob_count: u32,
    _pad0: u32,
    _pad1: u32,
}

crate::primitive! {
    name: BlobOverlayRender,
    type_id: "node.blob_overlay_render",
    purpose: "Draw hollow rectangles around each blob in an Array<Blob> on top of a source Texture2D. Companion to node.blob_detect_ffi for sparse blob visualisation. Minimal box-drawing — for the full HUD treatment (brackets, crosshairs, ticks, labels) use the legacy node.blob_tracking wrapper.",
    inputs: {
        in: Texture2D required,
        blobs: Array(Blob) required,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: "color",
            label: "Color",
            ty: ParamType::Color,
            default: ParamValue::Color([0.0, 1.0, 0.5, 1.0]),
            range: None,
            enum_values: &[],
        },
        ParamDef {
            name: "alpha",
            label: "Alpha",
            ty: ParamType::Float,
            default: ParamValue::Float(0.8),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "border_width",
            label: "Border Width",
            ty: ParamType::Float,
            default: ParamValue::Float(0.003),
            range: Some((0.0005, 0.05)),
            enum_values: &[],
        },
        ParamDef {
            name: "blob_count",
            label: "Blob Count",
            ty: ParamType::Int,
            default: ParamValue::Int(32),
            range: Some((0.0, 32.0)),
            enum_values: &[],
        },
    ],
    composition_notes: "Wire node.blob_detect_ffi.blobs → this primitive's blobs port. `blob_count` is an upper bound (the shader iterates this many entries from the array but skips any with zero width/height, so it's safe to leave at 32 even if the actual detection count is lower). `border_width` is in UV units (0.003 ≈ 2px at 720p). For thicker boxes raise this; for solid filled boxes set border_width > max(blob.width, blob.height).",
    examples: [],
    picker: { label: "Blob Overlay", category: Atom },
}

impl Primitive for BlobOverlayRender {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let color = match ctx.params.get("color") {
            Some(ParamValue::Color(c)) => [c[0], c[1], c[2]],
            _ => [0.0, 1.0, 0.5],
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
            Some(ParamValue::Int(i)) => (*i).max(0) as u32,
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
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                include_str!("shaders/blob_overlay_render.wgsl"),
                "cs_main",
                "node.blob_overlay_render",
            )
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        let uniforms = OverlayUniforms {
            overlay_color: color,
            alpha,
            border_width,
            blob_count,
            _pad0: 0,
            _pad1: 0,
        };

        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: blob_buf,
                    offset: 0,
                },
                GpuBinding::Texture {
                    binding: 2,
                    texture: in_tex,
                },
                GpuBinding::Sampler {
                    binding: 3,
                    sampler,
                },
                GpuBinding::Texture {
                    binding: 4,
                    texture: out_tex,
                },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "node.blob_overlay_render",
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
        use crate::node_graph::ports::{ArrayType, PortType};
        let blob_layout = ArrayType {
            item_size: std::mem::size_of::<Blob>() as u32,
            item_align: std::mem::align_of::<Blob>() as u32,
        };
        assert_eq!(BlobOverlayRender::TYPE_ID, "node.blob_overlay_render");
        assert_eq!(BlobOverlayRender::INPUTS.len(), 2);
        assert_eq!(BlobOverlayRender::INPUTS[0].name, "in");
        assert_eq!(BlobOverlayRender::INPUTS[0].ty, PortType::Texture2D);
        assert_eq!(BlobOverlayRender::INPUTS[1].name, "blobs");
        assert_eq!(
            BlobOverlayRender::INPUTS[1].ty,
            PortType::Array(blob_layout)
        );
        assert_eq!(BlobOverlayRender::OUTPUTS.len(), 1);
        assert_eq!(BlobOverlayRender::OUTPUTS[0].ty, PortType::Texture2D);
    }

    #[test]
    fn blob_overlay_render_has_color_alpha_border_count_params() {
        let names: Vec<&str> = BlobOverlayRender::PARAMS.iter().map(|p| p.name).collect();
        assert_eq!(
            names,
            vec!["color", "alpha", "border_width", "blob_count"]
        );
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = BlobOverlayRender::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.blob_overlay_render");
    }
}
