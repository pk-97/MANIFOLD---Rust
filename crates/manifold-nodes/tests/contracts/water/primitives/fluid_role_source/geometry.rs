pub(super) mod tests {
    use std::fs;
    use manifold_water_liquid::primitives::fluid_role_source::{CompoundPreparation, WiredPreparation};
    use manifold_water_liquid::primitives::fluid_role_source::geometry::{GeometryMode, prepare_geometry, prepare_wired_geometry};
    use manifold_water_liquid::testkit::fluid_role_source::cube_triangle_list;
    use manifold_node_engine::scene::mesh_source::MeshSource;
    use manifold_node_engine::scene::mesh_selection::MeshSelection;
    use manifold_node_engine::scene::transform::Transform;

    pub(crate) fn write_two_material_cube_fixture() -> (std::path::PathBuf, CompoundPreparation) {
        let vertices = cube_triangle_list();
        let material_zero = vertices[..18].to_vec();
        let mut material_one = vertices[18..].to_vec();
        for vertex in &mut material_one {
            vertex.position[0] -= 0.5;
        }

        let mut bin = Vec::new();
        for vertex in material_zero.iter().chain(&material_one) {
            for value in vertex.position {
                bin.extend_from_slice(&value.to_le_bytes());
            }
        }
        let doc = serde_json::json!({
            "asset": { "version": "2.0" },
            "scene": 0,
            "scenes": [{ "nodes": [0] }],
            "nodes": [{ "mesh": 0 }],
            "meshes": [{ "primitives": [
                { "attributes": { "POSITION": 0 }, "material": 0 },
                { "attributes": { "POSITION": 1 }, "material": 1 }
            ]}],
            "materials": [{}, {}],
            "buffers": [{ "uri": "fixture.bin", "byteLength": bin.len() }],
            "bufferViews": [
                { "buffer": 0, "byteOffset": 0, "byteLength": material_zero.len() * 12 },
                { "buffer": 0, "byteOffset": material_zero.len() * 12, "byteLength": material_one.len() * 12 }
            ],
            "accessors": [
                { "bufferView": 0, "componentType": 5126, "count": material_zero.len(), "type": "VEC3", "min": [-0.5, -0.5, -0.5], "max": [0.5, 0.5, 0.5] },
                { "bufferView": 1, "componentType": 5126, "count": material_one.len(), "type": "VEC3", "min": [-1.0, -0.5, -0.5], "max": [0.0, 0.5, 0.5] }
            ]
        });
        let dir = std::env::temp_dir().join(format!(
            "manifold-fluid-compound-{}",
            manifold_core::short_id()
        ));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("fixture.bin"), bin).unwrap();
        fs::write(dir.join("fixture.gltf"), serde_json::to_vec(&doc).unwrap()).unwrap();

        let mut part_transforms = [Transform::default(); 64];
        part_transforms[1].pos[0] = 0.5;
        let mut materials = [None; 64];
        materials[0] = Some(0);
        materials[1] = Some(1);
        (
            dir.join("fixture.gltf"),
            CompoundPreparation {
                materials,
                part_transforms,
            },
        )
    }

    #[test]
    fn scene_physics_wired_mesh_preserves_each_asset_selector_and_part_transform() {
        let (path, compound) = write_two_material_cube_fixture();
        let second_path = path.with_file_name("second.gltf");
        fs::copy(&path, &second_path).unwrap();
        let mut wired = WiredPreparation {
            sources: std::array::from_fn(|_| None),
            part_transforms: compound.part_transforms,
        };
        for (slot, source_path) in [&path, &second_path].into_iter().enumerate() {
            wired.sources[slot] = Some(MeshSource::Gltf {
                path: std::sync::Arc::from(source_path.to_str().unwrap()),
                selection: manifold_water_liquid::primitives::fluid_role_source::default_selection(32).with_material(slot as i32),
            });
        }
        let transform = Transform { pos: [0.0, 2.0, 0.0], ..Transform::default() };
        let first = prepare_wired_geometry(&wired, transform, GeometryMode::ClosedMesh, 32).unwrap();
        assert_eq!(first[0].vertices.len(), 8);
        assert_eq!(first[0].triangles.len(), 12);
        assert!(first[0].vertices.iter().all(|v| v[1] >= 1.5 && v[1] <= 2.5));

        // A selector offset and its matching local transform cancel exactly.
        // The first part has different selectors and must stay untouched.
        if let Some(MeshSource::Gltf { selection, .. }) = &mut wired.sources[1] {
            selection.translate[0] = 0.25;
        }
        wired.part_transforms[1].pos[0] -= 0.25;
        let moved = prepare_wired_geometry(&wired, transform, GeometryMode::ClosedMesh, 32).unwrap();
        assert_eq!(first[0].vertices, moved[0].vertices);
        assert_eq!(first[0].triangles, moved[0].triangles);

        if let Some(MeshSource::Gltf { selection, .. }) = &mut wired.sources[1] {
            selection.material = 0;
        }
        assert!(prepare_wired_geometry(&wired, transform, GeometryMode::ClosedMesh, 32).is_err(),
            "two copies of one half must not be accepted as the original closed cube");
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn scene_physics_compound_fluid_materials_assemble_before_validation() {
        let (path, compound) = write_two_material_cube_fixture();
        let selection = MeshSelection {
            mesh: -1,
            primitive: -1,
            material: -1,
            fit: false,
            recenter: false,
            translate: [0.0; 3],
            fragment_count: 1,
            fragment_index: 0,
            collider_parts: 1,
        };
        let meshes = prepare_geometry(
            &path,
            selection,
            1,
            1.0,
            Transform::default(),
            GeometryMode::ClosedMesh,
            Some(&compound),
        )
        .unwrap();
        assert_eq!(meshes[0].vertices.len(), 8);
        assert_eq!(meshes[0].triangles.len(), 12);
        manifold_fluids::validate_mesh(&meshes[0]).unwrap();
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }
}
