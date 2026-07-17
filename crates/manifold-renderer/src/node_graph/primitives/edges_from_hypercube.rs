//! `node.hypercube_edges` — emit the wireframe edge topology of a
//! 4D hypercube as `Array<EdgePair>`. The 4D-side counterpart of
//! [`super::polytope_edges`], paired with [`super::hypercube_vertices`].
//!
//! CPU-only: the 32-edge bit-flip table is constant (a hypercube has
//! one topology, unlike the five Platonic solids), so there is no
//! `shape` selector and no param. The morph in `hypercube_vertices`
//! lives entirely in the vertex positions — collapsed axes make some of
//! these 32 edges zero-length, which `node.draw_lines` draws as
//! nothing / dots. The topology stays the full 32 edges at every
//! `dimension`, which is exactly the "ramp the 4th coord from 0" reveal.
//!
//! CPU-write lands the data in the shared MTLBuffer in time for the
//! downstream CPU reader (`node.draw_lines`'s
//! `build_instances_from_edges`) to consume it same-frame without a
//! GPU→CPU fence — same pattern as `polytope_edges`.

use crate::generators::mesh_common::EdgePair;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::primitive::Primitive;

pub const HYPERCUBE_EDGE_COUNT: u32 = 32;

/// The canonical hypercube wireframe topology — 32 edges connecting each
/// vertex `i` to `i ^ bit` for `bit ∈ {1, 2, 4, 8}` where `j > i`.
/// `const fn` so the table lives in `.rodata`.
const fn hypercube_edges() -> [EdgePair; HYPERCUBE_EDGE_COUNT as usize] {
    let mut edges = [EdgePair { a: 0, b: 0 }; HYPERCUBE_EDGE_COUNT as usize];
    let mut k = 0;
    let mut i = 0u32;
    while i < 16 {
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

pub const HYPERCUBE_EDGES: [EdgePair; HYPERCUBE_EDGE_COUNT as usize] = hypercube_edges();

crate::primitive! {
    name: EdgesFromHypercube,
    type_id: "node.hypercube_edges",
    purpose: "Emit the wireframe edge topology of a 4D hypercube as Array<EdgePair> — the constant 32-edge bit-flip table. The 4D counterpart of node.platonic_solid_edges. Pair with node.hypercube_points and feed both into node.draw_lines (vertices → points, edges → edges) for a 4D wireframe. No params: a hypercube has one topology; the dimension-morph lives in the vertex positions.",
    inputs: {},
    outputs: {
        edges: Array(EdgePair),
    },
    params: [],
    depth_rule: Terminal,
    composition_notes: "Output capacity is fixed at 32 (the hypercube edge count). Edges connect (i, i^bit) for bit ∈ {1,2,4,8} where j > i. The table is constant across `dimension` — when node.hypercube_points collapses an axis, the affected edges become zero-length and node.draw_lines skips/dots them. Drive the paired node.hypercube_points for the matching corners.",
    examples: [],
    picker: { label: "Hypercube Edges (4D)", category: Atom },
    summary: "Builds the wireframe edges of a hypercube — which corners connect. Feed it with the matching hypercube points to draw the 4D cube.",
    category: Geometry3D,
    role: Source,
    aliases: ["tesseract", "hypercube", "edges from hypercube", "4d cube", "edges", "wireframe"],
    boundary_reason: NonGpu,
}

impl Primitive for EdgesFromHypercube {
    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &crate::node_graph::effect_node::ParamValues,
        _input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        if port_name == "edges" {
            Some(HYPERCUBE_EDGE_COUNT)
        } else {
            None
        }
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(edge_dst) = ctx.outputs.array("edges") else {
            log::warn!(
                "node.hypercube_edges: no GpuBuffer bound to output port `edges` — \
                 the chain build did not pre-allocate the Array<EdgePair> output.",
            );
            return;
        };

        let edge_size = std::mem::size_of::<EdgePair>() as u64;
        let edge_capacity = (edge_dst.size / edge_size) as u32;
        if edge_capacity == 0 {
            return;
        }

        // Sentinel-pad the tail in case downstream pre-allocated a larger
        // buffer; render_lines skips SENTINEL slots.
        let mut scratch = [EdgePair::SENTINEL; HYPERCUBE_EDGE_COUNT as usize];
        scratch.copy_from_slice(&HYPERCUBE_EDGES);
        let write_count = (edge_capacity as usize).min(scratch.len());
        // Safety: shared-memory MTLBuffer (chain build pre-allocates),
        // write_count clamped to the buffer capacity, sequential executor
        // on the content thread means no GPU race.
        unsafe {
            edge_dst.write(0, bytemuck::cast_slice(&scratch[..write_count]));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn declares_zero_inputs_and_edge_pair_output() {
        use crate::node_graph::ports::{ArrayType, PortType};
        let edge_layout = ArrayType::of_known::<EdgePair>();
        assert_eq!(EdgesFromHypercube::TYPE_ID, "node.hypercube_edges");
        assert!(EdgesFromHypercube::INPUTS.is_empty());
        assert_eq!(EdgesFromHypercube::OUTPUTS.len(), 1);
        assert_eq!(EdgesFromHypercube::OUTPUTS[0].name, "edges");
        assert_eq!(
            EdgesFromHypercube::OUTPUTS[0].ty,
            PortType::Array(edge_layout)
        );
    }

    #[test]
    fn output_capacity_is_thirty_two() {
        let prim = EdgesFromHypercube::new();
        let params = crate::node_graph::effect_node::ParamValues::default();
        assert_eq!(
            Primitive::array_output_capacity(&prim, "edges", &params, &[]),
            Some(HYPERCUBE_EDGE_COUNT)
        );
        assert!(Primitive::array_output_capacity(&prim, "bogus", &params, &[]).is_none());
    }

    /// Pin the topology: 32 unique pairs, each `(i, i^bit)` with `j > i`,
    /// in the canonical iteration order — a transcription error would
    /// render the wrong hypercube wireframe.
    #[test]
    fn edge_table_has_32_pairs_with_canonical_bit_flip_topology() {
        assert_eq!(HYPERCUBE_EDGES.len() as u32, HYPERCUBE_EDGE_COUNT);
        let mut expected: Vec<(u32, u32)> = Vec::with_capacity(32);
        for i in 0..16u32 {
            for bit_idx in 0..4 {
                let bit = 1u32 << bit_idx;
                let j = i ^ bit;
                if j > i {
                    expected.push((i, j));
                }
            }
        }
        assert_eq!(expected.len(), HYPERCUBE_EDGE_COUNT as usize);
        for (i, &(a, b)) in expected.iter().enumerate() {
            assert_eq!(HYPERCUBE_EDGES[i].a, a, "edge {i}.a");
            assert_eq!(HYPERCUBE_EDGES[i].b, b, "edge {i}.b");
        }
    }

    #[test]
    fn registers_as_palette_atom() {
        let prim = EdgesFromHypercube::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.hypercube_edges");
    }
}
