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

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    //! Real-GPU value-level proof: the generated standalone kernel against the
    //! committed formula, computed on the CPU.
    use super::*;

    fn vertex(position: [f32; 3], alpha: f32) -> MeshVertex {
        MeshVertex {
            position,
            normal: [0.0, 0.0, 1.0],
            uv: [position[0], position[1]],
            color: [1.0, 0.5, 0.25, alpha],
            ..bytemuck::Zeroable::zeroed()
        }
    }

    fn dispatch(src: &[MeshVertex], cutoff: f32) -> Vec<MeshVertex> {
        let device = manifold_gpu::testkit::test_device();
        let wgsl = manifold_node_engine::freeze::codegen::standalone_for_spec::<DropHiddenTriangles>()
            .expect("drop_hidden_triangles buffer codegen");
        let pipeline = device.create_compute_pipeline(&wgsl, manifold_node_engine::freeze::codegen::ENTRY, "drop-hidden-test");
        let bytes = std::mem::size_of_val(src) as u64;
        let input = device.create_buffer_shared(bytes);
        unsafe {
            input.write(0, bytemuck::cast_slice(src));
        }
        let output = device.create_buffer_shared(bytes);
        let uniforms = DropUniforms { cutoff, dispatch_count: src.len() as u32, _pad0: 0, _pad1: 0 };
        let mut enc = device.create_encoder("drop-hidden-test");
        enc.dispatch_compute(
            &pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                GpuBinding::Buffer { binding: 1, buffer: &input, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: &output, offset: 0 },
            ],
            [(src.len() as u32).div_ceil(256), 1, 1],
            "drop-hidden-test",
        );
        enc.commit_and_wait_completed();
        let ptr = output.mapped_ptr().expect("shared output buffer");
        unsafe { std::slice::from_raw_parts(ptr as *const MeshVertex, src.len()) }.to_vec()
    }

    /// The formula: a whole triangle at or below the cutoff moves onto its
    /// first vertex; everything else, and every other field, passes through.
    fn expected(src: &[MeshVertex], cutoff: f32) -> Vec<MeshVertex> {
        src.iter()
            .enumerate()
            .map(|(i, v)| {
                let base = i / 3 * 3;
                if base + 2 >= src.len() || src[base..base + 3].iter().any(|corner| corner.color[3] > cutoff) {
                    return *v;
                }
                MeshVertex { position: src[base].position, ..*v }
            })
            .collect()
    }

    #[test]
    fn hidden_triangles_collapse_and_the_rest_pass_through() {
        let src = vec![
            // All three at or below the cutoff: collapses.
            vertex([0.0, 0.0, 0.0], 0.0),
            vertex([1.0, 0.0, 0.0], 0.2),
            vertex([0.0, 1.0, 0.0], 0.5),
            // One corner shows: the triangle stays.
            vertex([2.0, 0.0, 0.0], 0.0),
            vertex([3.0, 0.0, 0.0], 1.0),
            vertex([2.0, 1.0, 0.0], 0.0),
            // A trailing partial triangle passes through.
            vertex([5.0, 5.0, 0.0], 0.0),
        ];
        let got = dispatch(&src, 0.5);
        let want = expected(&src, 0.5);
        for (i, (got, want)) in got.iter().zip(&want).enumerate() {
            assert_eq!(bytemuck::bytes_of(got), bytemuck::bytes_of(want), "vertex {i}: {:?} vs {:?}", got.position, want.position);
        }
        assert_eq!(got[1].position, [0.0, 0.0, 0.0], "a hidden triangle has no area");
        assert_eq!(got[4].position, [3.0, 0.0, 0.0], "a shown triangle keeps its shape");
    }
}
