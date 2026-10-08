//! Area-weighted normals, matching manifold-fluids decode_surface.
use manifold_node_engine::water::primitives::count_surface_triangles::MARCHING_CUBES_COMMON;
use manifold_node_engine::water::primitives::liquid_bricks;
use manifold_node_engine::water::primitives::relax_surface_mesh::SurfaceMeshPass;
use manifold_node_engine::float_param;
use manifold_node_engine::mesh::MeshVertex;
use manifold_node_engine::exec::effect_node::{EffectNodeContext, ParamValues};
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::primitive::Primitive;
use std::borrow::Cow;
manifold_node_engine::primitive! {
    name: SurfaceMeshNormals,
    type_id: "node.surface_mesh_normals",
    purpose: "Rebuild area-weighted smooth vertex normals from the incident triangles of a marching-cubes mesh. Sum unnormalized face cross products, then normalize, matching manifold-fluids decode_surface after FLIP mesh smoothing. Positions and attributes pass through; degenerate fans yield zero normals.",
    inputs: {
        vertices: Array(MeshVertex) required,
        levelset: Array(f32) required,
        scan: Array(u32) required,
        extent: Array(u32) optional,
        bricks: Array(u32) optional,
        edge_scan: Array(u32) optional,
        nodes_x: ScalarF32 optional, nodes_y: ScalarF32 optional, nodes_z: ScalarF32 optional,
    },
    outputs: {
        out: Array(MeshVertex),
    },
    params: [
        float_param!("nodes_x", "Nodes X", 2.0, 2.0, 4096.0),
        float_param!("nodes_y", "Nodes Y", 2.0, 2.0, 4096.0),
        float_param!("nodes_z", "Nodes Z", 2.0, 2.0, 4096.0),
    ],
    depth_rule: Terminal,
    composition_notes: "Wire after smooth_surface_mesh, with the mesh topology levelset, scan, edge_scan and lattice dimensions. Both indexed and triangle-list layouts use the same shared-edge incident triangles. Keep the original index buffer. This is a smooth normal gather; facet_normals remains the flat triangle-list operation.",
    examples: [],
    picker: { label: "Surface Mesh Normals", category: Atom },
    summary: "Rebuilds smooth normals from the final surface triangles so lighting follows the smoothed water.",
    category: Geometry3D,
    role: Filter,
    aliases: ["smooth normals", "area weighted normals", "water normals"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/surface_mesh_normals_body.wgsl"),
    input_access: [BufferGather, BufferGather, BufferGather, BufferGather, BufferGather, BufferGather],
    derived_uniforms: ["strength:f32", "max_capacity:u32", "brick_pass:u32", "indexed:u32"],
    wgsl_includes: [MARCHING_CUBES_COMMON, liquid_bricks::COMMON, manifold_node_engine::water::primitives::relax_surface_mesh::SURFACE_EDGE_OWNERSHIP_WGSL, manifold_node_engine::water::primitives::relax_surface_mesh::SURFACE_EDGE_INDEX_WGSL, manifold_node_engine::water::primitives::relax_surface_mesh::SURFACE_MESH_ADJACENCY_WGSL],
    owned_outputs: ["out"],
    buffer_index: "liquid_cell_brick_index",
    extra_fields: {
        pass: SurfaceMeshPass = SurfaceMeshPass::default(),
    },
}

impl Primitive for SurfaceMeshNormals {
    fn array_output_capacity(
        &self,
        port: &str,
        _params: &ParamValues,
        inputs: &[(&str, u32)],
    ) -> Option<u32> {
        (port == "out")
            .then(|| {
                inputs
                    .iter()
                    .find(|(name, _)| *name == "vertices")
                    .map(|(_, n)| *n)
            })
            .flatten()
    }
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        self.pass.run::<Self>(ctx, 0.0, 1, "out");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn generated_normal_gather_validates() {
        let source =
            manifold_node_engine::freeze::codegen::standalone_for_spec::<SurfaceMeshNormals>()
                .unwrap();
        let module = naga::front::wgsl::parse_str(&source).unwrap();
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .unwrap();
        assert!(source.contains("normal = normal + cross("));
        assert!(source.contains("buf_out[slot]"));
        assert!(!source.contains("workgroupBarrier"));
    }

}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    use super::*;
    use manifold_node_engine::mesh::MeshVertex;
    use manifold_node_engine::testkit::liquid_surface::{Harness, params, read};
    use crate::node_graph::primitives::smooth_surface_mesh::SmoothSurfaceMesh;
    use manifold_node_engine::water::primitives::surface_mesh_parity::{fixture, flip_normals, flip_smooth};
    use glam::DVec3;

    #[test]
    fn fluid_indexed_smoothing_and_normals_match_flip_reference() {
        let mut h = Harness::new();
        let mut smoother = SmoothSurfaceMesh::new();
        let mut normals = SurfaceMeshNormals::new();
        for kind in 0..3 {
            let f = fixture(kind);
            let capacity = f.triangles.len() * 3 + 19;
            let vertices: Vec<MeshVertex> = f
                .points
                .iter()
                .zip(&f.gradients)
                .map(|(p, n)| MeshVertex {
                    position: p.as_vec3().to_array(),
                    normal: n.as_vec3().to_array(),
                    color: [0.2, 0.4, 0.6, 1.0],
                    ..bytemuck::Zeroable::zeroed()
                })
                .collect();
            let triangle_scan = f.triangle_scan();
            let edge_scan = f.edge_scan();
            let (input, _) = h.array(&vertices, capacity);
            let (field, _) = h.array(&f.field, f.field.len());
            let (scan, _) = h.array(&triangle_scan, triangle_scan.len());
            let (edges, _) = h.array(&edge_scan, edge_scan.len());
            let (smoothed, smooth_buffer) = h.array::<MeshVertex>(&[], capacity);
            let (output, normal_buffer) = h.array::<MeshVertex>(&[], capacity);
            for (strength, iterations) in
                [(0.5, 0), (0.5, 1), (0.5, 2), (0.5, 5), (0.5, 11), (11.0, 1)]
            {
                let settings = params(&[
                    ("nodes_x", f.nodes as f32),
                    ("nodes_y", f.nodes as f32),
                    ("nodes_z", f.nodes as f32),
                    ("strength", strength),
                    ("iterations", iterations as f32),
                ]);
                let mut inputs = vec![
                    ("vertices", input),
                    ("levelset", field),
                    ("scan", scan),
                    ("edge_scan", edges),
                ];
                if iterations > 10 || strength > 10.0 {
                    inputs.push(("iterations", h.scalar_input(iterations as f32)));
                    inputs.push(("strength", h.scalar_input(strength)));
                }
                let (_, errors) =
                    h.run(&mut smoother, &inputs, &[("relaxed", smoothed)], &settings);
                assert!(errors.is_empty(), "{errors:?}");
                let (_, errors) = h.run(
                    &mut normals,
                    &[
                        ("vertices", smoothed),
                        ("levelset", field),
                        ("scan", scan),
                        ("edge_scan", edges),
                    ],
                    &[("out", output)],
                    &settings,
                );
                assert!(errors.is_empty(), "{errors:?}");
                let expected =
                    flip_smooth(&f.points, &f.triangles, f64::from(strength), iterations);
                let expected_normals = flip_normals(&expected, &f.triangles);
                let moved = read::<MeshVertex>(&smooth_buffer, capacity);
                let got = read::<MeshVertex>(&normal_buffer, capacity);
                for (i, v) in got.iter().take(expected.len()).enumerate() {
                    let p = DVec3::from_array(v.position.map(f64::from));
                    let n = DVec3::from_array(v.normal.map(f64::from));
                    assert!(
                        p.distance(expected[i]) < 3e-6 * expected[i].length().max(1.0),
                        "fixture {kind}, iteration {iterations}, position {i}"
                    );
                    assert!(
                        n.distance(expected_normals[i]) < 3e-5 * f64::from(strength.abs().max(1.0)),
                        "fixture {kind}, iteration {iterations}, normal {i}: {n:?} vs {:?}",
                        expected_normals[i]
                    );
                    assert_eq!(
                        v.position, moved[i].position,
                        "normals cannot move the surface"
                    );
                    assert_eq!(v.color, vertices[i].color);
                }
                assert!(
                    got[expected.len()..]
                        .iter()
                        .all(|v| v.position == [0.0; 3] && v.normal == [0.0; 3])
                );
            }
        }
    }
}

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
mod extent;
