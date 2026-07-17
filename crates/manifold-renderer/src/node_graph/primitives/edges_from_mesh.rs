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
    purpose: "Emit the per-triangle wireframe edge topology of a flat triangle-list Array<MeshVertex> as Array<EdgePair>: triangle t contributes (3t, 3t+1), (3t+1, 3t+2), (3t+2, 3t). Pure index arithmetic — glTF meshes are unindexed flat triangle lists (gltf_load expands indices), so no vertex data is read and edge count equals vertex count. Pair with node.flatten_3d (same mesh) into node.draw_lines for a true mesh wireframe of any imported or procedural triangle mesh. Interior edges shared by two triangles are emitted twice (flat verts carry no adjacency) — under draw_lines' additive blend shared edges draw ~2x brighter; dedup would need adjacency reconstruction (future work if renders demand it). `active_count` (input-only) overrides the vertex count used for topology when the source buffer's capacity exceeds the asset's real loaded vertex count — when unwired, edge count tracks the BUFFER capacity, which produces degenerate (0,0)-style dot edges from the zero-filled tail if max_capacity was sized larger than the asset.",
    inputs: {
        vertices: Array(MeshVertex) required,
        active_count: ScalarF32 optional,
    },
    outputs: {
        edges: Array(EdgePair),
    },
    params: [],
    depth_rule: Terminal,
    composition_notes: "Output capacity = input vertex capacity (one edge per vertex: 3 edges per 3-vertex triangle) at plan time. Runtime fills floor(min(buffer_verts, active_count) / 3) * 3 edges and sentinel-pads the tail. `active_count` (input-only port-shadow, mirrors node.range) lets a source with a larger max_capacity than the loaded asset still emit only real topology — when unwired the count is the buffer's own vertex capacity, so old graphs (fully-sized sources) dispatch identically. CPU-write topology, same content-thread pattern as node.grid_edges — draw_lines consumes it same-frame.",
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

/// Resolve the vertex count to build topology from: the source buffer's
/// own capacity, clamped down by an optional `active_count` port-shadow
/// override (BUG-123). Pure so it can be unit-tested without a real
/// `GpuBuffer`/Metal device — mirrors `node.range`'s `active_count`
/// clamp-to-`[0, capacity]` convention.
fn resolve_vert_count(buffer_vert_count: u32, active_count: Option<f32>) -> u32 {
    match active_count {
        Some(n) => (n.round().max(0.0) as u32).min(buffer_vert_count),
        None => buffer_vert_count,
    }
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
        let buffer_vert_count = (src.size / vert_size) as u32;
        // Port-shadow: when `active_count` is wired, it overrides the
        // buffer-capacity-derived vertex count so a source sized larger
        // than the loaded asset (max_capacity > real vertex count)
        // doesn't emit degenerate edges from its zero-filled tail.
        let active_count_input = ctx.inputs.scalar("active_count").and_then(|v| v.as_scalar());
        let vert_count = resolve_vert_count(buffer_vert_count, active_count_input);
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
        use crate::node_graph::ports::{ArrayType, PortType, ScalarType};

        assert_eq!(EdgesFromMesh::TYPE_ID, "node.mesh_edges");
        assert_eq!(EdgesFromMesh::INPUTS.len(), 2);
        assert_eq!(EdgesFromMesh::INPUTS[0].name, "vertices");
        assert!(EdgesFromMesh::INPUTS[0].required);
        assert_eq!(
            EdgesFromMesh::INPUTS[0].ty,
            PortType::Array(ArrayType::of_known::<MeshVertex>()),
        );
        assert_eq!(EdgesFromMesh::INPUTS[1].name, "active_count");
        assert!(
            !EdgesFromMesh::INPUTS[1].required,
            "active_count must be optional (port-shadow)"
        );
        assert_eq!(
            EdgesFromMesh::INPUTS[1].ty,
            PortType::Scalar(ScalarType::F32),
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

    // BUG-123: a source buffer sized larger than the asset's real loaded
    // vertex count (max_capacity > actual vertices) must not let the
    // zero-filled tail contribute degenerate (0,0)-style edges. Without
    // `active_count` wired, behavior is unchanged (buffer capacity IS the
    // count) — old graphs still dispatch bit-identically.
    #[test]
    fn active_count_unwired_falls_back_to_buffer_capacity() {
        assert_eq!(resolve_vert_count(9210, None), 9210);
        assert_eq!(resolve_vert_count(0, None), 0);
    }

    #[test]
    fn active_count_clamps_a_buffer_sized_larger_than_the_real_asset() {
        // max_capacity was sized for a bigger asset (e.g. reused across a
        // preset swap) than the currently-loaded mesh's real vertex count.
        let buffer_capacity = 12_000; // sized for a larger asset
        let real_asset_verts = 300.0; // the actually-loaded glTF's vertex count
        assert_eq!(
            resolve_vert_count(buffer_capacity, Some(real_asset_verts)),
            300,
        );
        // Topology built from this count is a whole number of triangles —
        // no degenerate zero-vertex edges from the buffer's zero-filled tail.
        assert_eq!(300 % 3, 0);
    }

    #[test]
    fn active_count_cannot_exceed_buffer_capacity() {
        // A stray/over-large active_count is clamped down, mirroring
        // node.range's `active_count.min(max_capacity)` convention — it can
        // shrink the active set but never read past the real buffer.
        assert_eq!(resolve_vert_count(300, Some(50_000.0)), 300);
    }

    #[test]
    fn active_count_rounds_and_floors_at_zero() {
        assert_eq!(resolve_vert_count(300, Some(150.4)), 150);
        assert_eq!(resolve_vert_count(300, Some(150.6)), 151);
        assert_eq!(resolve_vert_count(300, Some(-5.0)), 0);
    }
}
