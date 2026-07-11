//! `node.facet_normals` — recompute exact per-triangle flat normals for an
//! `Array<MeshVertex>` flat triangle list. The v1 normal-policy "reset"
//! atom (MESH_DEFORM_AND_CURVE_GEOMETRY_DESIGN.md D4): after a heavy
//! `push_along_normals` / (future) `morph_mesh` whose normals were left
//! approximate, wire this downstream to trade the smooth-normal look for
//! an exact faceted one.
//!
//! One thread per triangle: `n = normalize(cross(v1.pos - v0.pos, v2.pos -
//! v0.pos))`, written to all three of that triangle's vertices. Exact on
//! the flat triangle-list convention (`spawn_from_mesh.rs` module doc):
//! triangle `t` reads/writes verts `[3t, 3t+3)` — each vertex belongs to
//! exactly one triangle, so there's no gather and no race between threads.
//! A trailing partial triangle (`vertex_count % 3 != 0`, 1 or 2 leftover
//! vertices) passes through with its existing normal unchanged — there's
//! no triangle to compute a normal from. Dispatch count is
//! `ceil(vertex_count / 3)` so the same one-thread-per-triangle dispatch
//! also covers that trailing group.

use manifold_gpu::GpuBinding;

use crate::generators::mesh_common::MeshVertex;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::primitive::Primitive;

/// Uniform layout: `vertex_count` (total, for the trailing-partial bounds
/// check), `full_triangles` (dispatch boundary between the full-triangle
/// branch and the passthrough branch), padded to 16 bytes.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct FacetUniforms {
    vertex_count: u32,
    full_triangles: u32,
    _pad0: u32,
    _pad1: u32,
}

crate::primitive! {
    name: FacetNormals,
    type_id: "node.facet_normals",
    purpose: "Recompute exact per-triangle flat normals for an Array<MeshVertex> flat triangle list: one thread per triangle t computes n = normalize(cross(v1.pos - v0.pos, v2.pos - v0.pos)) and writes it to verts [3t, 3t+3). Positions and uv pass through unchanged. A trailing partial triangle (vertex_count % 3 != 0) passes through with its existing normal unchanged. The v1 normal-policy reset (D4): wire downstream of a heavy push_along_normals or morph_mesh to trade their approximate/unchanged normals for an exact faceted look.",
    inputs: {
        in: Array(MeshVertex) required,
    },
    outputs: {
        out: Array(MeshVertex),
    },
    params: [],
    composition_notes: "Reach for this whenever a deformer's normal policy is left approximate (push_along_normals, morph_mesh) and the amount is large enough that the unchanged/lerped normals start reading wrong under lighting — the result is a faceted low-poly look, not smooth shading. Only correct on the flat triangle-list layout (no shared vertices, no index buffer); node.triangulate_grid's grid-topology output already carries correct finite-difference normals and doesn't need this.",
    examples: ["Breathe"],
    picker: { label: "Facet Normals", category: Atom },
    summary: "Recomputes a mesh's normals from its own triangle geometry, giving flat, faceted shading — the exact fix for a mesh whose normals went stale after a heavy deformation.",
    category: Geometry3D,
    role: Filter,
    aliases: ["facet normals", "flat normals", "recompute normals", "normal reset"],
}

impl Primitive for FacetNormals {
    /// Output `out` is sized to match input `in` — normal recompute is a
    /// per-vertex-slot transform, no expansion.
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
        let Some(src) = ctx.inputs.array("in") else {
            return;
        };
        let Some(dst) = ctx.outputs.array("out") else {
            return;
        };

        let vertex_size = std::mem::size_of::<MeshVertex>() as u64;
        let in_count = (src.size / vertex_size) as u32;
        let out_count = (dst.size / vertex_size) as u32;
        let vertex_count = in_count.min(out_count);
        if vertex_count == 0 {
            return;
        }
        let full_triangles = vertex_count / 3;
        let remainder = vertex_count % 3;
        let dispatch_count = full_triangles + u32::from(remainder > 0);

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                include_str!("shaders/facet_normals.wgsl"),
                "cs_main",
                "node.facet_normals",
            )
        });

        let uniforms = FacetUniforms {
            vertex_count,
            full_triangles,
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
                    buffer: src,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: dst,
                    offset: 0,
                },
            ],
            [dispatch_count.div_ceil(256), 1, 1],
            "node.facet_normals",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn facet_normals_declares_mesh_in_and_out_only() {
        use crate::node_graph::ports::{ArrayType, PortType};
        let mesh_layout = ArrayType::of_known::<MeshVertex>();

        assert_eq!(FacetNormals::TYPE_ID, "node.facet_normals");
        assert_eq!(FacetNormals::INPUTS.len(), 1);
        assert_eq!(FacetNormals::INPUTS[0].name, "in");
        assert!(FacetNormals::INPUTS[0].required);
        assert_eq!(FacetNormals::INPUTS[0].ty, PortType::Array(mesh_layout));

        assert_eq!(FacetNormals::OUTPUTS.len(), 1);
        assert_eq!(FacetNormals::OUTPUTS[0].ty, PortType::Array(mesh_layout));

        assert!(FacetNormals::PARAMS.is_empty(), "facet_normals has no params per §3 table");
    }

    #[test]
    fn facet_normals_output_follows_in_input() {
        use crate::node_graph::effect_node::ParamValues;
        let prim = FacetNormals::new();
        let params = ParamValues::default();
        let inputs = [("in", 36_u32)];
        assert_eq!(
            Primitive::array_output_capacity(&prim, "out", &params, &inputs),
            Some(36),
        );
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = FacetNormals::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.facet_normals");
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    //! Real-GPU value-level tests. Analytic cross-product normal compared
    //! element-wise against the GPU readback, per DECOMPOSING_GENERATORS.md
    //! §9's parity bar (no legacy predecessor exists to diff against — the
    //! "hand kernel" is the committed formula, computed in Rust).
    use super::*;

    fn mk_vertex(pos: [f32; 3], normal: [f32; 3], uv: [f32; 2]) -> MeshVertex {
        MeshVertex {
            position: pos,
            _pad0: 0.0,
            normal,
            _pad1: 0.0,
            uv,
            _pad2: [0.0, 0.0],
        }
    }

    fn dispatch_facet(device: &manifold_gpu::GpuDevice, src: &[MeshVertex]) -> Vec<MeshVertex> {
        let wgsl = include_str!("shaders/facet_normals.wgsl");
        let pipeline = device.create_compute_pipeline(wgsl, "cs_main", "facet-normals-test");
        let sbuf = device.create_buffer_shared(std::mem::size_of_val(src) as u64);
        unsafe {
            sbuf.write(0, bytemuck::cast_slice(src));
        }
        let dbuf = device.create_buffer_shared(std::mem::size_of_val(src) as u64);

        let vertex_count = src.len() as u32;
        let full_triangles = vertex_count / 3;
        let remainder = vertex_count % 3;
        let dispatch_count = full_triangles + u32::from(remainder > 0);

        let uniforms = FacetUniforms {
            vertex_count,
            full_triangles,
            _pad0: 0,
            _pad1: 0,
        };
        let bindings = [
            GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
            GpuBinding::Buffer { binding: 1, buffer: &sbuf, offset: 0 },
            GpuBinding::Buffer { binding: 2, buffer: &dbuf, offset: 0 },
        ];
        let mut enc = device.create_encoder("facet-normals-test");
        enc.dispatch_compute(&pipeline, &bindings, [dispatch_count.div_ceil(256), 1, 1], "facet-normals-test");
        enc.commit_and_wait_completed();

        let ptr = dbuf.mapped_ptr().expect("shared dst buffer");
        unsafe { std::slice::from_raw_parts(ptr as *const MeshVertex, src.len()) }.to_vec()
    }

    #[test]
    fn analytic_normal_on_a_right_triangle() {
        let device = crate::test_device();
        // Same right-triangle fixture as spawn_from_mesh.rs's gpu_tests:
        // v0=(0,0,0), v1=(4,0,0), v2=(0,3,0) — analytic normal (0,0,1).
        let v0 = [0.0f32, 0.0, 0.0];
        let v1 = [4.0f32, 0.0, 0.0];
        let v2 = [0.0f32, 3.0, 0.0];
        let src = vec![
            mk_vertex(v0, [0.0, 0.0, 0.0], [0.0, 0.0]),
            mk_vertex(v1, [0.0, 0.0, 0.0], [1.0, 0.0]),
            mk_vertex(v2, [0.0, 0.0, 0.0], [0.0, 1.0]),
        ];
        let out = dispatch_facet(&device, &src);

        for i in 0..3 {
            assert!((out[i].normal[0]).abs() < 1e-5, "vertex {i} normal.x: {}", out[i].normal[0]);
            assert!((out[i].normal[1]).abs() < 1e-5, "vertex {i} normal.y: {}", out[i].normal[1]);
            assert!((out[i].normal[2] - 1.0).abs() < 1e-5, "vertex {i} normal.z: {}", out[i].normal[2]);
            assert_eq!(out[i].position, src[i].position);
            assert_eq!(out[i].uv, src[i].uv);
        }
    }

    #[test]
    fn trailing_partial_triangle_passes_through_unchanged() {
        let device = crate::test_device();
        // 4 vertices: verts 0-2 form a full triangle, vert 3 is a trailing
        // partial group of size 1 — must pass through with its ORIGINAL
        // normal (arbitrary, non-recomputed value), not the triangle's.
        let src = vec![
            mk_vertex([0.0, 0.0, 0.0], [0.0, 0.0, 0.0], [0.0, 0.0]),
            mk_vertex([1.0, 0.0, 0.0], [0.0, 0.0, 0.0], [1.0, 0.0]),
            mk_vertex([0.0, 1.0, 0.0], [0.0, 0.0, 0.0], [0.0, 1.0]),
            mk_vertex([9.0, 9.0, 9.0], [1.0, 0.0, 0.0], [0.3, 0.3]), // trailing partial
        ];
        let out = dispatch_facet(&device, &src);

        assert_eq!(out.len(), 4);
        // Full triangle (0,1,2): normal is +Z.
        for i in 0..3 {
            assert!((out[i].normal[2] - 1.0).abs() < 1e-5, "vertex {i} should get the triangle normal");
        }
        // Trailing partial (3): passes through UNCHANGED, including its
        // original (arbitrary) normal — nothing recomputed.
        assert_eq!(out[3].position, src[3].position);
        assert_eq!(out[3].normal, src[3].normal, "trailing partial vertex normal must be untouched");
        assert_eq!(out[3].uv, src[3].uv);
    }

    #[test]
    fn count_is_preserved() {
        let device = crate::test_device();
        let src = vec![
            mk_vertex([0.0, 0.0, 0.0], [0.0, 0.0, 0.0], [0.0, 0.0]),
            mk_vertex([1.0, 0.0, 0.0], [0.0, 0.0, 0.0], [1.0, 0.0]),
            mk_vertex([0.0, 1.0, 0.0], [0.0, 0.0, 0.0], [0.0, 1.0]),
            mk_vertex([1.0, 1.0, 0.0], [0.0, 0.0, 0.0], [1.0, 1.0]),
            mk_vertex([2.0, 0.0, 0.0], [0.0, 0.0, 0.0], [2.0, 0.0]),
            mk_vertex([2.0, 1.0, 0.0], [0.0, 0.0, 0.0], [2.0, 1.0]),
        ];
        let out = dispatch_facet(&device, &src);
        assert_eq!(out.len(), src.len());
    }
}
