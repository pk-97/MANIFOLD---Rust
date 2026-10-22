//! `node.drop_hidden_triangles` — collapse every triangle whose vertices are
//! all hidden by alpha, so an alpha cut (node.cut_out_box) shows on any
//! material, not only one in alpha Mask mode.
//!
//! A flat triangle list stores each triangle as three vertices `[3t, 3t+3)`.
//! Each vertex reads its triangle's three alphas; when all are at or below
//! `cutoff`, it moves onto the triangle's first vertex, so the triangle has
//! no area. Alpha, normals and uv pass through, so a Mask material still
//! draws the soft edge of the triangles that stay.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use manifold_node_engine::exec::effect_node::EffectNodeContext;
use manifold_node_engine::mesh::MeshVertex;
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::primitive::Primitive;
use manifold_node_engine::primitives::standalone_pipeline::standalone_pipeline;

/// Generated-codegen uniform layout: `cutoff`, then `dispatch_count`, padded
/// to 16 bytes.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct DropUniforms {
    cutoff: f32,
    dispatch_count: u32,
    _pad0: u32,
    _pad1: u32,
}

manifold_node_engine::primitive! {
    name: DropHiddenTriangles,
    type_id: "node.drop_hidden_triangles",
    purpose: "Collapse every triangle of an Array<MeshVertex> flat triangle list whose three vertex alphas are all <= cutoff: each of its vertices moves onto the triangle's first vertex, leaving zero area, so it neither draws, shadows nor traces on any material. One thread per vertex, gathering its own triangle (base = 3*(idx/3)), so a triangle collapses whole. Alpha, normals, uv and colour pass through; a trailing partial triangle passes through.",
    inputs: {
        in: Array(MeshVertex) required,
        cutoff: ScalarF32 optional,
    },
    outputs: {
        out: Array(MeshVertex),
    },
    params: [
        ParamDef { name: Cow::Borrowed("cutoff"), label: "Cutoff", ty: ParamType::Float, default: ParamValue::Float(0.5), range: Some((0.0, 1.0)), enum_values: &[] },
    ],
    depth_rule: Terminal,
    composition_notes: "Wire after node.cut_out_box (or any node that hides vertices by alpha) so the cut shows at once on an opaque material. Cutoff 0.5 matches a Mask material's default alpha cutoff, so the hole has the same edge either way; a Mask material still feathers the triangles that stay.",
    examples: [],
    picker: { label: "Drop Hidden Triangles", category: Atom },
    summary: "Removes the parts of a mesh that an alpha cut has hidden, so the hole shows on any material.",
    category: Geometry3D,
    role: Filter,
    aliases: ["drop hidden", "remove faces", "cull triangles", "delete faces"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/drop_hidden_triangles_body.wgsl"),
    input_access: [BufferGather],
}

impl Primitive for DropHiddenTriangles {
    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &manifold_node_engine::exec::effect_node::ParamValues,
        input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        (port_name == "out").then(|| input_capacities.iter().find(|(p, _)| *p == "in").map(|(_, n)| *n)).flatten()
    }

    /// The triangle count never changes: topology tracks `in`.
    fn mesh_output_rule(&self, port: &str) -> manifold_node_engine::scene::mesh_change::MeshOutputRule<'_> {
        use manifold_node_engine::scene::mesh_change::{MeshAspect, MeshDependency, MeshOutputRule, MeshRevisionRule};
        if port == "out" {
            return MeshOutputRule {
                topology: MeshRevisionRule::Dependencies(&[MeshDependency { input: Cow::Borrowed("in"), aspect: MeshAspect::Topology }]),
                positions: MeshRevisionRule::Written,
            };
        }
        MeshOutputRule { topology: MeshRevisionRule::Written, positions: MeshRevisionRule::Written }
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let cutoff = ctx.scalar_or_param("cutoff", 0.5);
        let Some(src) = ctx.inputs.array("in") else {
            return;
        };
        let Some(dst) = ctx.outputs.array("out") else {
            return;
        };
        let vertex_size = std::mem::size_of::<MeshVertex>() as u64;
        let count = ((src.size / vertex_size) as u32).min((dst.size / vertex_size) as u32);
        if count == 0 {
            return;
        }
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let uniforms = DropUniforms { cutoff, dispatch_count: count, _pad0: 0, _pad1: 0 };
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                GpuBinding::Buffer { binding: 1, buffer: src, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: dst, offset: 0 },
            ],
            [count.div_ceil(256), 1, 1],
            "node.drop_hidden_triangles",
        );
    }
}
