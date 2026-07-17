//! `node.platonic_solid_edges` — emit the wireframe edge topology of one
//! of the five Platonic solids as `Array<EdgePair>`. Curated-enum
//! atom paired with [`super::polytope_vertices`]; same `shape` scalar
//! drives both so vertices and edges agree per frame.
//!
//! CPU-only — the edge tables are tiny (≤30 entries × 8 bytes per
//! shape), pure adjacency lookups with no math. CPU-write also lands
//! the data in the shared MTLBuffer in time for downstream CPU readers
//! (`node.draw_lines`'s `build_instances_from_edges`) to consume it
//! same-frame without a GPU→CPU fence.

use std::borrow::Cow;

use crate::generators::mesh_common::{
    EdgePair, PLATONIC_MAX_EDGES, PLATONIC_SHAPES, platonic_edges,
};
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;
use crate::node_graph::primitives::polytope_vertices::read_shape;

crate::primitive! {
    name: PolytopeEdges,
    type_id: "node.platonic_solid_edges",
    purpose: "Emit the wireframe edge topology of one of the five Platonic solids as Array<EdgePair>. Curated-enum atom — one CPU write of a static per-shape adjacency table, sentinel-padded for the inactive tail. Pair with node.platonic_solid_points (driving both from the same shape scalar) and feed both into node.draw_lines (vertices → points, edges → edges) for a 3D wireframe.",
    inputs: {
        // Port-shadows the `shape` enum param. Wire the same scalar
        // here that drives `node.platonic_solid_points.shape` so the two
        // atoms stay in sync.
        shape: ScalarF32 optional,
    },
    outputs: {
        edges: Array(EdgePair),
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("shape"),
            label: "Shape",
            ty: ParamType::Enum,
            default: ParamValue::Enum(0),
            range: None,
            enum_values: PLATONIC_SHAPES,
        },
    ],
    depth_rule: Terminal,
    composition_notes: "Output capacity is fixed at PLATONIC_MAX_EDGES (30 — Icosa / Dodeca have the most edges); slots past the active shape's edge count are EdgePair::SENTINEL so node.draw_lines's `build_instances_from_edges` filter skips them without drawing a line. Indices are stable per shape and reference the paired `node.platonic_solid_points` slot ordering — drive both atoms' `shape` input from the same scalar so vertices and edges always agree.",
    examples: [],
    picker: { label: "Platonic Solid Edges", category: Atom },
    summary: "Builds the wireframe edges of one of the five Platonic solids, pairing up which corners connect. Feed it with the matching points to draw the wireframe.",
    category: Geometry3D,
    role: Source,
    aliases: ["platonic solid", "polytope edges", "polytope", "edges", "wireframe"],
    boundary_reason: NonGpu,
}

impl Primitive for PolytopeEdges {
    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &crate::node_graph::effect_node::ParamValues,
        _input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        if port_name == "edges" {
            Some(PLATONIC_MAX_EDGES)
        } else {
            None
        }
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        // See polytope_vertices::read_shape for why scalar_or_param
        // can't be used directly here (ParamValue::Enum fall-through).
        let shape = read_shape(ctx);

        let Some(edge_dst) = ctx.outputs.array("edges") else {
            log::warn!(
                "node.platonic_solid_edges: no GpuBuffer bound to output port `edges` — \
                 the chain build did not pre-allocate the Array<EdgePair> output.",
            );
            return;
        };

        let edge_size = std::mem::size_of::<EdgePair>() as u64;
        let edge_capacity = (edge_dst.size / edge_size) as u32;
        if edge_capacity == 0 {
            return;
        }

        let mut scratch = [EdgePair::SENTINEL; PLATONIC_MAX_EDGES as usize];
        let active = platonic_edges(shape);
        scratch[..active.len()].copy_from_slice(active);

        let write_count = (edge_capacity as usize).min(scratch.len());
        // Safety: shared-memory MTLBuffer (chain build pre-allocates),
        // write_count clamped to the buffer capacity, sequential
        // executor on the content thread means no GPU race.
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
    fn declares_shape_input_and_edge_pair_output() {
        use crate::node_graph::ports::{ArrayType, PortType, ScalarType};
        let edge_layout = ArrayType::of_known::<EdgePair>();
        assert_eq!(PolytopeEdges::TYPE_ID, "node.platonic_solid_edges");
        assert_eq!(PolytopeEdges::INPUTS.len(), 1);
        assert_eq!(PolytopeEdges::INPUTS[0].name, "shape");
        assert!(!PolytopeEdges::INPUTS[0].required);
        assert_eq!(
            PolytopeEdges::INPUTS[0].ty,
            PortType::Scalar(ScalarType::F32)
        );

        assert_eq!(PolytopeEdges::OUTPUTS.len(), 1);
        assert_eq!(PolytopeEdges::OUTPUTS[0].name, "edges");
        assert_eq!(PolytopeEdges::OUTPUTS[0].ty, PortType::Array(edge_layout));
    }

    #[test]
    fn shape_enum_lists_five_platonic_solids() {
        let shape = PolytopeEdges::PARAMS
            .iter()
            .find(|p| p.name == "shape")
            .unwrap();
        assert_eq!(shape.ty, ParamType::Enum);
        assert_eq!(shape.enum_values.len(), 5);
        assert_eq!(shape.enum_values, PLATONIC_SHAPES);
    }

    #[test]
    fn output_capacity_is_platonic_max_edges() {
        let prim = PolytopeEdges::new();
        let params = crate::node_graph::effect_node::ParamValues::default();
        assert_eq!(
            Primitive::array_output_capacity(&prim, "edges", &params, &[]),
            Some(PLATONIC_MAX_EDGES)
        );
        assert!(
            Primitive::array_output_capacity(&prim, "bogus", &params, &[]).is_none()
        );
    }

    /// Pin the per-shape table sizes — a single transcription error
    /// in `platonic_edges` (e.g. the wrong table on the wrong index)
    /// would silently render the wrong wireframe topology.
    #[test]
    fn platonic_edges_counts_match_each_shape() {
        let expected = [
            (0u32, 6),  // Tetra
            (1, 12),    // Cube
            (2, 12),    // Octa
            (3, 30),    // Icosa
            (4, 30),    // Dodeca
        ];
        for (shape, count) in expected {
            assert_eq!(
                platonic_edges(shape).len(),
                count,
                "shape {shape} edge count",
            );
        }
    }

    /// Pin the Tetrahedron edges by hand against the canonical
    /// topology — a swap with another shape's table would otherwise
    /// produce a tetrahedron with the wrong connections.
    #[test]
    fn tetra_edges_match_canonical_topology() {
        let expected: &[(u32, u32)] =
            &[(0, 1), (0, 2), (0, 3), (1, 2), (1, 3), (2, 3)];
        let edges = platonic_edges(0);
        assert_eq!(edges.len(), expected.len());
        for (i, &(a, b)) in expected.iter().enumerate() {
            assert_eq!(edges[i].a, a, "tetra edge {i}.a");
            assert_eq!(edges[i].b, b, "tetra edge {i}.b");
        }
    }

    #[test]
    fn registers_as_palette_atom() {
        let prim = PolytopeEdges::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.platonic_solid_edges");
    }
}
