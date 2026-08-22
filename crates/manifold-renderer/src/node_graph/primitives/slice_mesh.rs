//! `node.slice_mesh` — per-vertex planar cut of an `Array<MeshVertex>`.
//!
//! Verts whose coordinate along the chosen `axis` is past the `cut` value are
//! clamped onto the plane, producing a flat cut face. Normals, uv, and tangent
//! pass through unchanged.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use crate::generators::mesh_common::MeshVertex;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

const SLICE_AXES: &[&str] = &["X", "Y", "Z"];

/// Generated-codegen uniform layout: scalar params in PARAMS order (`axis`
/// Enum→u32, `cut` f32), then the derived `weights_len` (u32), then the
/// codegen-injected `dispatch_count`, padded to a 16-byte multiple. 4 words =
/// 16 bytes. Matches `standalone_for_spec::<SliceMesh>()`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SliceUniforms {
    axis: u32,
    cut: f32,
    weights_len: u32,
    dispatch_count: u32,
}

crate::primitive! {
    name: SliceMesh,
    type_id: "node.slice_mesh",
    purpose: "Per-vertex planar cut of an Array<MeshVertex>. axis selects the cut coordinate (X/Y/Z); verts whose coordinate is past `cut` are clamped onto the plane. `w` is the optional per-vertex `weights` input (a short or unwired weights buffer degrades to 1.0, never silent 0). Normals, uv, and tangent pass through unchanged — wire node.facet_normals downstream if the cut face needs flat shading.",
    inputs: {
        in: Array(MeshVertex) required,
        weights: Array(f32) optional,
        cut: ScalarF32 optional,
    },
    outputs: {
        out: Array(MeshVertex),
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("axis"),
            label: "Axis",
            ty: ParamType::Enum,
            default: ParamValue::Enum(1), // Y
            range: Some((0.0, (SLICE_AXES.len() - 1) as f32)),
            enum_values: SLICE_AXES,
        },
        ParamDef {
            name: Cow::Borrowed("cut"),
            label: "Cut",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-1000.0, 1000.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "The 'planar cut / wipe' deformer. Sweep `cut` across the model to reveal or hide it one axis at a time. Because clamped vertices keep their original normals, the cut face reads as a hard silhouette until you wire node.facet_normals downstream.",
    examples: [],
    picker: { label: "Slice", category: Atom },
    summary: "Clamps all vertices past a plane onto the plane, turning a mesh into a flat cut face you can sweep across.",
    category: Geometry3D,
    role: Filter,
    aliases: ["slice", "slice mesh", "cut", "wipe", "planar cut"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/slice_mesh_body.wgsl"),
    // `in` and `weights` are both COINCIDENT (default) — keeps the atom fully
    // pointwise/fusable so it can chain with other mesh deformers. `weights_len`
    // is a frame-derived uniform the body uses to bounds-check the coincident
    // weight read (degrade to 1.0 past the buffer).
    derived_uniforms: ["weights_len:u32"],
}

impl Primitive for SliceMesh {
    /// Output `out` is sized to match input `in` — slicing is a per-vertex
    /// transform, no expansion.
    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &crate::node_graph::effect_node::ParamValues,
        input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        if port_name != "out" {
            return None;
        }
        input_capacities.iter().find(|(p, _)| *p == "in").map(|(_, n)| *n)
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let axis = match ctx.params.get("axis") {
            Some(ParamValue::Enum(v)) => (*v).min((SLICE_AXES.len() - 1) as u32),
            _ => 1,
        };
        let cut = ctx.scalar_or_param("cut", 0.0);

        let Some(src) = ctx.inputs.array("in") else {
            return;
        };
        let weights_wired = ctx.inputs.array("weights");
        let weights_buf = weights_wired.unwrap_or(src);
        let Some(dst) = ctx.outputs.array("out") else {
            return;
        };

        let vertex_size = std::mem::size_of::<MeshVertex>() as u64;
        let in_count = (src.size / vertex_size) as u32;
        let out_count = (dst.size / vertex_size) as u32;
        let count = in_count.min(out_count);
        if count == 0 {
            return;
        }
        let weights_len = weights_wired.map(|b| (b.size / 4) as u32).unwrap_or(0);

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            // Codegen path: the runtime kernel is generated from `wgsl_body`
            // so this atom stays pointwise/fusable in the graph compiler.
            gpu.device.create_compute_pipeline(
                &crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                    .expect("node.slice_mesh standalone codegen"),
                crate::node_graph::freeze::codegen::ENTRY,
                "node.slice_mesh",
            )
        });

        let uniforms = SliceUniforms {
            axis,
            cut,
            weights_len,
            dispatch_count: count,
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
                    buffer: src,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: weights_buf,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 3,
                    buffer: dst,
                    offset: 0,
                },
            ],
            [count.div_ceil(256), 1, 1],
            "node.slice_mesh",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn slice_mesh_declares_ports() {
        use crate::node_graph::ports::{ArrayType, PortType, ScalarType};
        let mesh_layout = ArrayType::of_known::<MeshVertex>();
        let f32_layout = ArrayType::of_known::<f32>();

        assert_eq!(SliceMesh::TYPE_ID, "node.slice_mesh");

        let in_port = SliceMesh::INPUTS.iter().find(|p| p.name == "in").unwrap();
        assert!(in_port.required);
        assert_eq!(in_port.ty, PortType::Array(mesh_layout));

        let weights_port = SliceMesh::INPUTS.iter().find(|p| p.name == "weights").unwrap();
        assert!(!weights_port.required);
        assert_eq!(weights_port.ty, PortType::Array(f32_layout));

        let cut_port = SliceMesh::INPUTS.iter().find(|p| p.name == "cut").unwrap();
        assert!(!cut_port.required);
        assert_eq!(cut_port.ty, PortType::Scalar(ScalarType::F32));

        let axis_param = SliceMesh::PARAMS.iter().find(|p| p.name == "axis").unwrap();
        assert_eq!(axis_param.ty, ParamType::Enum);

        assert_eq!(SliceMesh::OUTPUTS.len(), 1);
        assert_eq!(SliceMesh::OUTPUTS[0].ty, PortType::Array(mesh_layout));
    }

    #[test]
    fn slice_mesh_output_follows_in_input() {
        use crate::node_graph::effect_node::ParamValues;
        let prim = SliceMesh::new();
        let params = ParamValues::default();
        let inputs = [("in", 36_u32)];
        assert_eq!(
            Primitive::array_output_capacity(&prim, "out", &params, &inputs),
            Some(36),
        );
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = SliceMesh::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.slice_mesh");
    }
}
