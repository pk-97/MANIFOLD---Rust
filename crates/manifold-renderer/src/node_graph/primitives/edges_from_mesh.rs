//! `node.mesh_edges` — emit the per-triangle wireframe edge topology of
//! a flat triangle-list `Array<MeshVertex>` as `Array<EdgePair>`.
//!
//! glTF meshes arrive unindexed (`gltf_load` expands indices to a flat
//! triangle list), so triangle `t` owns vertices `(3t, 3t+1, 3t+2)` and
//! emits its three edges from index arithmetic alone — no vertex data
//! is read. Pair with `node.flatten_3d` (same mesh → projected points)
//! into `node.draw_lines` for a true mesh wireframe:
//! `gltf_mesh_source → flatten_3d → draw_lines.points`,
//! `gltf_mesh_source → mesh_edges → draw_lines.edges`.
//!
//! CPU-write into shared MTLBuffer, sentinel-padded inactive tail —
//! same family as `node.grid_edges` / `node.platonic_solid_edges`.

use crate::generators::mesh_common::{EdgePair, MeshVertex};
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::primitive::Primitive;

crate::primitive! {
    name: EdgesFromMesh,
    type_id: "node.mesh_edges",
    purpose: "Emit the per-triangle wireframe edge topology of a flat triangle-list Array<MeshVertex> as Array<EdgePair>: triangle t contributes (3t, 3t+1), (3t+1, 3t+2), (3t+2, 3t). Pure index arithmetic — glTF meshes are unindexed flat triangle lists (gltf_load expands indices), so no vertex data is read and edge count equals vertex count. Pair with node.flatten_3d (same mesh) into node.draw_lines for a true mesh wireframe of any imported or procedural triangle mesh. Interior edges shared by two triangles are emitted twice (flat verts carry no adjacency) — under draw_lines' additive blend shared edges draw ~2x brighter; dedup would need adjacency reconstruction (future work if renders demand it). Edge count tracks the BUFFER capacity, not a runtime active count — size the source's max_capacity to the asset (a zero-filled tail draws degenerate dot edges at vertex 0).",
    inputs: {
        vertices: Array(MeshVertex) required,
    },
    outputs: {
        edges: Array(EdgePair),
    },
    params: [],
    composition_notes: "Output capacity = input vertex capacity (one edge per vertex: 3 edges per 3-vertex triangle) at plan time. Runtime fills floor(buffer_verts / 3) * 3 edges and sentinel-pads the tail. Size gltf_mesh_source.max_capacity to the asset's exact vertex count or the zero tail contributes (0,0) dot edges. CPU-write topology, same content-thread pattern as node.grid_edges — draw_lines consumes it same-frame.",
    examples: [],
    picker: { label: "Mesh Edges", category: Atom },
    summary: "Outputs the wireframe edges of a triangle mesh, so any imported model can be drawn as lines. The mesh counterpart of Grid Edges.",
    category: Geometry3D,
    role: Filter,
    aliases: ["mesh edges", "edges from mesh", "wireframe", "triangle edges", "topology"],
    boundary_reason: NonGpu,
    extra_fields: {
        scratch: Vec<EdgePair> = Vec::new(),
    },
}

impl Primitive for EdgesFromMesh {
    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &crate::node_graph::effect_node::ParamValues,
        input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        if port_name != "edges" {
            return None;
        }
        input_capacities
            .iter()
            .find(|(name, _)| *name == "vertices")
            .map(|(_, cap)| *cap)
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(src) = ctx.inputs.array("vertices") else {
            return;
        };
        let Some(edge_dst) = ctx.outputs.array("edges") else {
            log::warn!(
                "node.mesh_edges: no GpuBuffer bound to output port `edges` — \
                 the chain build did not pre-allocate this Array<EdgePair>.",
            );
            return;
        };

        let vert_size = std::mem::size_of::<MeshVertex>() as u64;
        let edge_size = std::mem::size_of::<EdgePair>() as u64;
        let vert_count = (src.size / vert_size) as u32;
        let edge_capacity = (edge_dst.size / edge_size) as u32;
        if edge_capacity == 0 {
            return;
        }

        let tri_count = vert_count / 3;
        let active_edges = (tri_count * 3) as usize;
        self.scratch.clear();
        self.scratch.reserve(active_edges.min(edge_capacity as usize));
        for t in 0..tri_count {
            let base = t * 3;
            self.scratch.push(EdgePair { a: base, b: base + 1 });
            self.scratch.push(EdgePair { a: base + 1, b: base + 2 });
            self.scratch.push(EdgePair { a: base + 2, b: base });
        }

        let write_count = (edge_capacity as usize).min(active_edges);
        // Safety: shared-memory MTLBuffer pre-bound by the chain build;
        // write count clamped to the buffer capacity; sequential
        // executor means no concurrent writer.
        unsafe {
            edge_dst.write(0, bytemuck::cast_slice(&self.scratch[..write_count]));
        }

        if write_count < edge_capacity as usize {
            const PAD_CHUNK: usize = 64;
            const TAIL: [EdgePair; PAD_CHUNK] = [EdgePair::SENTINEL; PAD_CHUNK];
            let mut offset = write_count;
            while offset < edge_capacity as usize {
                let chunk = (edge_capacity as usize - offset).min(PAD_CHUNK);
                unsafe {
                    edge_dst.write(
                        (offset as u64) * edge_size,
                        bytemuck::cast_slice(&TAIL[..chunk]),
                    );
                }
                offset += chunk;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn declares_mesh_input_and_edge_pair_output() {
        use crate::node_graph::ports::{ArrayType, PortType};

        assert_eq!(EdgesFromMesh::TYPE_ID, "node.mesh_edges");
        assert_eq!(EdgesFromMesh::INPUTS.len(), 1);
        assert_eq!(EdgesFromMesh::INPUTS[0].name, "vertices");
        assert!(EdgesFromMesh::INPUTS[0].required);
        assert_eq!(
            EdgesFromMesh::INPUTS[0].ty,
            PortType::Array(ArrayType::of_known::<MeshVertex>()),
        );

        assert_eq!(EdgesFromMesh::OUTPUTS.len(), 1);
        assert_eq!(EdgesFromMesh::OUTPUTS[0].name, "edges");
        assert_eq!(
            EdgesFromMesh::OUTPUTS[0].ty,
            PortType::Array(ArrayType::of_known::<EdgePair>()),
        );
    }

    #[test]
    fn output_capacity_matches_vertex_capacity() {
        use crate::node_graph::effect_node::ParamValues;
        let prim = EdgesFromMesh::new();
        let params = ParamValues::default();

        assert_eq!(
            Primitive::array_output_capacity(&prim, "edges", &params, &[("vertices", 9210)]),
            Some(9210),
        );
        assert_eq!(
            Primitive::array_output_capacity(&prim, "edges", &params, &[]),
            None,
        );
        assert_eq!(
            Primitive::array_output_capacity(&prim, "bogus", &params, &[("vertices", 9210)]),
            None,
        );
    }

    #[test]
    fn registers_as_palette_atom() {
        let prim = EdgesFromMesh::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.mesh_edges");
    }
}
