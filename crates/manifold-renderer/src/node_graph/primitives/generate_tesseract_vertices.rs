//! `node.generate_tesseract_vertices` — emit the 16 corner vertices of
//! a 4D hypercube (tesseract) scaled to magnitude 0.25, plus its
//! 32-edge wireframe topology, as paired `Array<Vec4Vertex>` +
//! `Array<EdgePair>` outputs.
//!
//! The 4D-side counterpart of [`crate::node_graph::primitives::WireframeShape`].
//! Vertex positions are `(±1, ±1, ±1, ±1) * 0.125` — the sign pattern
//! follows `(sign(i&1), sign(i&2), sign(i&4), sign(i&8))` and the
//! 0.125 = 0.25 / 2 scaling normalises the corner magnitude (sqrt(4)
//! = 2) to 0.25, matching the legacy `generator_math::PROJ_SCALE`
//! screen-fit factor. Edges connect every pair `(i, i^bit)` for
//! `bit ∈ {1, 2, 4, 8}` where `j > i` (the canonical hypercube bit-
//! flip wireframe).
//!
//! The 0.25 magnitude lives inside this primitive (not as a graph-
//! side math node) so downstream `project_4d.proj_scale` defaults to
//! 1.0 — outer-card Scale binds to it directly and gives "Scale 1.0
//! = default zoom" UX without a multiplier node in the graph. Same
//! pattern as `wireframe_shape`. 4D perspective is non-linear in w,
//! so this bake does not reproduce the legacy generator's projected
//! pixels bit-exactly — accepted trade-off.
//!
//! Edges live in a CPU-written shared MTLBuffer alongside vertices to
//! match the [`WireframeShape`] pattern — the downstream consumer
//! (`node.render_lines`) reads the edges buffer CPU-side to build its
//! per-instance EdgeInstance buffer, and a same-frame GPU write would
//! not be visible to that CPU read without a fence.

use manifold_gpu::GpuBinding;

use crate::generators::mesh_common::{EdgePair, Vec4Vertex};
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::primitive::Primitive;

pub const TESSERACT_VERTEX_COUNT: u32 = 16;
pub const TESSERACT_EDGE_COUNT: u32 = 32;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniforms {
    capacity: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

crate::primitive! {
    name: GenerateTesseractVertices,
    type_id: "node.generate_tesseract_vertices",
    purpose: "Emit the 16 corner vertices of a 4D hypercube (tesseract) scaled to magnitude 0.25 plus its 32-edge wireframe topology as paired Array<Vec4Vertex> + Array<EdgePair>. Feed both into node.rotate_4d → node.project_4d → node.render_lines (with the `edges` input wired) to reproduce the legacy Tesseract generator's pipeline. The 4D counterpart of node.wireframe_shape.",
    inputs: {},
    outputs: {
        vertices: Array(Vec4Vertex),
        edges: Array(EdgePair),
    },
    params: [],
    composition_notes: "Vertex coordinates are (sign(i&1), sign(i&2), sign(i&4), sign(i&8)) * 0.125 — the 0.125 = 0.25 / 2 scaling normalises the corner magnitude (sqrt(4) = 2) to 0.25, matching the legacy PROJ_SCALE screen-fit factor. Edges connect (i, i^bit) for bit ∈ {1, 2, 4, 8} where j > i — 32 edges total, the canonical hypercube wireframe. Both outputs are pre-sized to fit exactly: vertices=16, edges=32. The 4D-shape vertex-set primitive — pipe through rotate_4d / project_4d / render_lines (with proj_scale defaulted to 1.0) to render.",
    examples: [],
    picker: { label: "Generate Tesseract Vertices", category: Atom },
}

/// Compute the canonical hypercube wireframe topology — 32 edges connecting
/// each vertex `i` to `i^bit` for `bit ∈ {1, 2, 4, 8}` where `j > i`. Inlined
/// here as a `const fn` so the table lives in `.rodata` and any future
/// `wgsl_compute_*` lift of this primitive still has the legacy reference.
const fn tesseract_edges() -> [EdgePair; TESSERACT_EDGE_COUNT as usize] {
    let mut edges = [EdgePair { a: 0, b: 0 }; TESSERACT_EDGE_COUNT as usize];
    let mut k = 0;
    let mut i = 0u32;
    while i < TESSERACT_VERTEX_COUNT {
        let mut bit_idx = 0;
        while bit_idx < 4 {
            let bit = 1u32 << bit_idx;
            let j = i ^ bit;
            if j > i {
                edges[k] = EdgePair { a: i, b: j };
                k += 1;
            }
            bit_idx += 1;
        }
        i += 1;
    }
    edges
}

const TESSERACT_EDGES: [EdgePair; TESSERACT_EDGE_COUNT as usize] = tesseract_edges();

impl Primitive for GenerateTesseractVertices {
    /// Both outputs are pre-sized to fit exactly:
    /// - `vertices`: 16 (one per tesseract corner)
    /// - `edges`:    32 (one per wireframe edge)
    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &crate::node_graph::effect_node::ParamValues,
        _input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        match port_name {
            "vertices" => Some(TESSERACT_VERTEX_COUNT),
            "edges" => Some(TESSERACT_EDGE_COUNT),
            _ => None,
        }
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(vert_dst) = ctx.outputs.array("vertices") else {
            return;
        };
        let Some(edge_dst) = ctx.outputs.array("edges") else {
            return;
        };
        let vertex_size = std::mem::size_of::<Vec4Vertex>() as u64;
        let edge_size = std::mem::size_of::<EdgePair>() as u64;
        let vert_capacity = (vert_dst.size / vertex_size) as u32;
        let edge_capacity = (edge_dst.size / edge_size) as u32;
        if vert_capacity == 0 || edge_capacity == 0 {
            return;
        }

        // ── CPU-write edges (same pattern as wireframe_shape) ──
        // Topology is constant per shape; consumer (render_lines) reads
        // CPU-side. Sentinel-pad the tail in case downstream pre-allocated
        // a larger buffer.
        let mut edges_scratch = [EdgePair::SENTINEL; TESSERACT_EDGE_COUNT as usize];
        edges_scratch.copy_from_slice(&TESSERACT_EDGES);
        let write_count = (edge_capacity as usize).min(edges_scratch.len());
        unsafe {
            edge_dst.write(0, bytemuck::cast_slice(&edges_scratch[..write_count]));
        }

        // ── Dispatch the vertex-write compute shader ──
        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                include_str!("shaders/generate_tesseract_vertices.wgsl"),
                "cs_main",
                "node.generate_tesseract_vertices",
            )
        });

        let uniforms = Uniforms {
            capacity: vert_capacity,
            _pad0: 0,
            _pad1: 0,
            _pad2: 0,
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
                    buffer: vert_dst,
                    offset: 0,
                },
            ],
            [vert_capacity.div_ceil(64), 1, 1],
            "node.generate_tesseract_vertices",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn declares_zero_inputs_and_vec4_plus_edge_array_outputs() {
        use crate::node_graph::ports::{ArrayType, PortType};
        let vec4_layout = ArrayType {
            item_size: std::mem::size_of::<Vec4Vertex>() as u32,
            item_align: std::mem::align_of::<Vec4Vertex>() as u32,
        };
        let edge_layout = ArrayType {
            item_size: std::mem::size_of::<EdgePair>() as u32,
            item_align: std::mem::align_of::<EdgePair>() as u32,
        };
        assert_eq!(
            GenerateTesseractVertices::TYPE_ID,
            "node.generate_tesseract_vertices"
        );
        assert!(GenerateTesseractVertices::INPUTS.is_empty());
        assert_eq!(GenerateTesseractVertices::OUTPUTS.len(), 2);
        assert_eq!(GenerateTesseractVertices::OUTPUTS[0].name, "vertices");
        assert_eq!(
            GenerateTesseractVertices::OUTPUTS[0].ty,
            PortType::Array(vec4_layout)
        );
        assert_eq!(GenerateTesseractVertices::OUTPUTS[1].name, "edges");
        assert_eq!(
            GenerateTesseractVertices::OUTPUTS[1].ty,
            PortType::Array(edge_layout)
        );
    }

    #[test]
    fn tesseract_edge_table_has_32_unique_pairs_with_legacy_bit_flip_topology() {
        assert_eq!(TESSERACT_EDGES.len() as u32, TESSERACT_EDGE_COUNT);

        // Legacy reference: for each vertex i, connect to i^1, i^2, i^4,
        // i^8 where j > i. Replicate the exact iteration order so the
        // const fn output is bit-identical to what the deleted Rust
        // generator produced.
        let mut expected: Vec<(u32, u32)> = Vec::with_capacity(32);
        for i in 0..TESSERACT_VERTEX_COUNT {
            for bit_idx in 0..4 {
                let bit = 1u32 << bit_idx;
                let j = i ^ bit;
                if j > i {
                    expected.push((i, j));
                }
            }
        }
        assert_eq!(expected.len(), TESSERACT_EDGE_COUNT as usize);
        for (i, &(a, b)) in expected.iter().enumerate() {
            assert_eq!(TESSERACT_EDGES[i].a, a, "edge {i}.a");
            assert_eq!(TESSERACT_EDGES[i].b, b, "edge {i}.b");
        }
    }

    #[test]
    fn array_output_capacities_match_constants() {
        let prim = GenerateTesseractVertices::new();
        let params = crate::node_graph::effect_node::ParamValues::default();
        assert_eq!(
            Primitive::array_output_capacity(&prim, "vertices", &params, &[]),
            Some(TESSERACT_VERTEX_COUNT)
        );
        assert_eq!(
            Primitive::array_output_capacity(&prim, "edges", &params, &[]),
            Some(TESSERACT_EDGE_COUNT)
        );
        assert_eq!(
            Primitive::array_output_capacity(&prim, "bogus", &params, &[]),
            None
        );
    }

    #[test]
    fn registers_with_palette() {
        let prim = GenerateTesseractVertices::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.generate_tesseract_vertices");
    }
}
