//! `node.make_triangles` — convert a positions-only NxM
//! `Array<MeshVertex>` grid into a triangle-list (N-1)×(M-1)×6
//! vertex stream with per-vertex normals computed from
//! finite-difference tangents.
//!
//! Adapter primitive that lets `node.grid_mesh`'s
//! positions feed cleanly into `node.render_mesh` (which expects
//! triangle-list topology). The source grid is read in row-major
//! order (`row * cols + col`); the output is laid out as six
//! consecutive vertices per quad in the canonical
//! 0/1/2-0/2/3-shape used by the legacy MetallicGlass renderer.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use crate::generators::mesh_common::MeshVertex;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Generated-codegen uniform layout: scalar params in PARAMS order (`src_cols`,
/// `src_rows`, both Int → i32) then the codegen-injected `dispatch_count`
/// (= dst capacity, the guard), padded to 16 bytes.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct TriangulateUniforms {
    src_cols: i32,
    src_rows: i32,
    dispatch_count: u32,
    _pad0: u32,
}

crate::primitive! {
    name: TriangulateGrid,
    type_id: "node.make_triangles",
    purpose: "Convert a positions-only NxM Array<MeshVertex> grid into a triangle-list (N-1)*(M-1)*6 vertex stream with finite-difference normals. The adapter primitive between node.grid_mesh (positions) and node.render_mesh (triangle list). For MetallicGlass-shaped graphs: GenerateGridMesh → DisplaceMesh → TriangulateGrid → Render3DMesh.",
    inputs: {
        in: Array(MeshVertex) required,
    },
    outputs: {
        out: Array(MeshVertex),
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("src_cols"),
            label: "Source Columns",
            ty: ParamType::Int,
            default: ParamValue::Float(256.0),
            range: Some((2.0, 4096.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("src_rows"),
            label: "Source Rows",
            ty: ParamType::Int,
            default: ParamValue::Float(256.0),
            range: Some((2.0, 4096.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "src_cols × src_rows must match the upstream producer's grid resolution. Output capacity must be at least (src_cols - 1) × (src_rows - 1) × 6 vertices. Default 256×256 grid → 390,150 triangle vertices ≈ 12.5 MB. Border normals are clamped to the nearest in-bounds neighbour (no special-case ghost rows). Source must be in row-major order: idx = row * cols + col.",
    examples: [],
    picker: { label: "Make Triangles", category: Atom },
    summary: "Turns a grid of points into a solid mesh of triangles, so a flat field of points becomes a surface you can render.",
    category: Geometry3D,
    role: Filter,
    aliases: ["triangulate", "make triangles", "triangulate grid", "mesh", "surface"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/triangulate_grid_body.wgsl"),
    input_access: [BufferGather],
}

impl Primitive for TriangulateGrid {
    /// Output capacity = `(src_cols-1) × (src_rows-1) × 6` triangle
    /// vertices — the canonical 6-vertex-per-quad triangulation.
    fn array_output_capacity(
        &self,
        port_name: &str,
        params: &crate::node_graph::effect_node::ParamValues,
        _input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        if port_name != "out" {
            return None;
        }
        let read_dim = |name| match params.get(name) {
            Some(ParamValue::Float(n)) => Some(n.round().max(2_f32) as u32),
            _ => None,
        };
        let cols = read_dim("src_cols")?;
        let rows = read_dim("src_rows")?;
        Some((cols - 1).saturating_mul(rows - 1).saturating_mul(6))
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let src_cols = match ctx.params.get("src_cols") {
            Some(ParamValue::Float(n)) => n.round().max(2_f32) as u32,
            _ => 256,
        };
        let src_rows = match ctx.params.get("src_rows") {
            Some(ParamValue::Float(n)) => n.round().max(2_f32) as u32,
            _ => 256,
        };

        let Some(src) = ctx.inputs.array("in") else {
            return;
        };
        let Some(dst) = ctx.outputs.array("out") else {
            return;
        };
        let vertex_size = std::mem::size_of::<MeshVertex>() as u64;
        let dst_capacity = (dst.size / vertex_size) as u32;
        if dst_capacity == 0 {
            return;
        }

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            // Single-source: kernel generated from the `wgsl_body` (buffer GATHER
            // path — the body indexes the input grid global). triangulate_grid.wgsl
            // is the parity oracle.
            gpu.device.create_compute_pipeline(
                &crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                    .expect("node.make_triangles standalone codegen"),
                crate::node_graph::freeze::codegen::ENTRY,
                "node.make_triangles",
            )
        });

        let uniforms = TriangulateUniforms {
            src_cols: src_cols as i32,
            src_rows: src_rows as i32,
            dispatch_count: dst_capacity,
            _pad0: 0,
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
            [dst_capacity.div_ceil(256), 1, 1],
            "node.make_triangles",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn triangulate_grid_declares_mesh_array_in_and_out() {
        use crate::node_graph::ports::{ArrayType, PortType};
        let layout = ArrayType::of_known::<MeshVertex>();
        assert_eq!(TriangulateGrid::TYPE_ID, "node.make_triangles");
        assert_eq!(TriangulateGrid::INPUTS.len(), 1);
        assert_eq!(TriangulateGrid::INPUTS[0].ty, PortType::Array(layout));
        assert_eq!(TriangulateGrid::OUTPUTS.len(), 1);
        assert_eq!(TriangulateGrid::OUTPUTS[0].ty, PortType::Array(layout));
    }

    #[test]
    fn triangulate_grid_has_cols_and_rows_params() {
        let names: Vec<&str> = TriangulateGrid::PARAMS.iter().map(|p| p.name.as_ref()).collect();
        assert_eq!(names, vec!["src_cols", "src_rows"]);
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = TriangulateGrid::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.make_triangles");
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    //! Buffer-domain GATHER parity oracle (freeze §12) — triangulate_grid had no
    //! GPU test. The generated kernel (the body indexes the input grid global
    //! buf_in to read each output vertex's quad corner + the finite-difference
    //! normal neighbours) must reproduce the hand kernel vertex-for-vertex,
    //! including the padding past the triangle count. Same math → bit-exact.
    use super::*;

    fn grid_vertex(pos: [f32; 3], uv: [f32; 2]) -> MeshVertex {
        MeshVertex { position: pos, _pad0: 0.0, normal: [0.0; 3], _pad1: 0.0, uv, _pad2: [0.0; 2] }
    }

    fn dispatch_tri(
        wgsl: &str,
        grid: &[MeshVertex],
        dst_cap: u32,
        uniform: &[u8],
    ) -> Vec<MeshVertex> {
        let device = crate::test_device();
        let pipeline = device.create_compute_pipeline(wgsl, "cs_main", "tri-oracle");
        let src_buf = device.create_buffer_shared(std::mem::size_of_val(grid) as u64);
        let dst_buf = device.create_buffer_shared(dst_cap as u64 * 48);
        unsafe {
            src_buf.write(0, bytemuck::cast_slice(grid));
        }
        let mut enc = device.create_encoder("tri-oracle");
        enc.dispatch_compute(
            &pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: uniform },
                GpuBinding::Buffer { binding: 1, buffer: &src_buf, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: &dst_buf, offset: 0 },
            ],
            [dst_cap.div_ceil(64), 1, 1],
            "tri-oracle",
        );
        enc.commit_and_wait_completed();
        let ptr = dst_buf.mapped_ptr().expect("shared dst buffer");
        let slice = unsafe { std::slice::from_raw_parts(ptr as *const MeshVertex, dst_cap as usize) };
        slice.to_vec()
    }

    #[test]
    fn generated_triangulate_matches_hand_kernel() {
        const COLS: u32 = 3;
        const ROWS: u32 = 3;
        // 3x3 grid, row-major: idx = row*cols + col. Give heights so normals vary.
        let mut grid = Vec::new();
        for row in 0..ROWS {
            for col in 0..COLS {
                let x = col as f32 / (COLS - 1) as f32;
                let y = row as f32 / (ROWS - 1) as f32;
                let h = (x * 2.0).sin() * (y * 2.0).cos() * 0.3;
                grid.push(grid_vertex([x, h, y], [x, y]));
            }
        }
        // (3-1)*(3-1)*6 = 24 triangle verts; pad to 30 to exercise the padding.
        const DST_CAP: u32 = 30;

        // Hand layout: src_cols(u32), src_rows(u32), dst_capacity(u32), pad.
        let mut hand = Vec::new();
        hand.extend_from_slice(&COLS.to_le_bytes());
        hand.extend_from_slice(&ROWS.to_le_bytes());
        hand.extend_from_slice(&DST_CAP.to_le_bytes());
        hand.extend_from_slice(&0u32.to_le_bytes());

        // Generated layout: src_cols(i32), src_rows(i32), dispatch_count(u32), pad.
        let mut gen_bytes = Vec::new();
        gen_bytes.extend_from_slice(&(COLS as i32).to_le_bytes());
        gen_bytes.extend_from_slice(&(ROWS as i32).to_le_bytes());
        gen_bytes.extend_from_slice(&DST_CAP.to_le_bytes());
        gen_bytes.extend_from_slice(&0u32.to_le_bytes());

        let hand_wgsl = include_str!("shaders/triangulate_grid.wgsl");
        let gen_wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<TriangulateGrid>()
            .expect("triangulate_grid buffer codegen");
        assert!(gen_wgsl.contains("var<storage, read> buf_in"), "gather input is read-only global");

        let from_hand = dispatch_tri(hand_wgsl, &grid, DST_CAP, &hand);
        let from_gen = dispatch_tri(&gen_wgsl, &grid, DST_CAP, &gen_bytes);

        for i in 0..DST_CAP as usize {
            for c in 0..3 {
                assert!(
                    (from_hand[i].position[c] - from_gen[i].position[c]).abs() < 1e-6,
                    "vertex {i} position[{c}]: hand={} gen={}",
                    from_hand[i].position[c],
                    from_gen[i].position[c]
                );
                assert!(
                    (from_hand[i].normal[c] - from_gen[i].normal[c]).abs() < 1e-6,
                    "vertex {i} normal[{c}]"
                );
            }
            assert_eq!(from_hand[i].uv, from_gen[i].uv, "vertex {i} uv");
        }
    }

    /// BUG-120: on a flat XZ grid the per-vertex normal is always the
    /// declared +Y up-vector (`tg_compute_normal`'s finite difference has no
    /// height variation to react to) — the emitted triangle WINDING must
    /// agree with that, i.e. `cross(v1-v0, v2-v0)` for every emitted
    /// triangle must also point +Y, not -Y. Before the fix this primitive
    /// wound every triangle CW-from-above while declaring +Y vertex normals
    /// — exactly the disagreement scatter_on_mesh's align_to_normal had to
    /// work around at the consumer (BUG-120's original finding). Checked on
    /// the hand kernel; the parity test above already proves the generated
    /// kernel matches it vertex-for-vertex.
    #[test]
    fn flat_grid_triangle_winding_agrees_with_vertex_normal() {
        const COLS: u32 = 3;
        const ROWS: u32 = 3;
        let mut grid = Vec::new();
        for row in 0..ROWS {
            for col in 0..COLS {
                let x = col as f32;
                let z = row as f32;
                grid.push(grid_vertex([x, 0.0, z], [x, z]));
            }
        }
        const DST_CAP: u32 = 24; // (3-1)*(3-1)*6, no padding needed.

        let mut hand = Vec::new();
        hand.extend_from_slice(&COLS.to_le_bytes());
        hand.extend_from_slice(&ROWS.to_le_bytes());
        hand.extend_from_slice(&DST_CAP.to_le_bytes());
        hand.extend_from_slice(&0u32.to_le_bytes());

        let hand_wgsl = include_str!("shaders/triangulate_grid.wgsl");
        let verts = dispatch_tri(hand_wgsl, &grid, DST_CAP, &hand);

        fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
            [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
        }
        fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
            [
                a[1] * b[2] - a[2] * b[1],
                a[2] * b[0] - a[0] * b[2],
                a[0] * b[1] - a[1] * b[0],
            ]
        }
        fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
            a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
        }
        fn normalize(a: [f32; 3]) -> [f32; 3] {
            let len = dot(a, a).sqrt();
            [a[0] / len, a[1] / len, a[2] / len]
        }

        for tri in verts.chunks_exact(3) {
            let face_normal = normalize(cross(
                sub(tri[1].position, tri[0].position),
                sub(tri[2].position, tri[0].position),
            ));
            let vertex_normal = tri[0].normal;
            let positions = [tri[0].position, tri[1].position, tri[2].position];
            assert!(
                dot(face_normal, vertex_normal) > 0.99,
                "winding-derived face normal {face_normal:?} disagrees with \
                 declared vertex normal {vertex_normal:?} for triangle {positions:?}"
            );
        }
    }
}
