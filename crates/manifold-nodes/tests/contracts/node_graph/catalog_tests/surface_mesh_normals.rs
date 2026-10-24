use manifold_node_engine::parameters::ParamValue;

#[test]
fn surface_stage_defaults_and_manifest_bindings() {
    use manifold_node_engine::primitive::PrimitiveSpec;
    use manifold_water_surface::primitives::smooth_surface_mesh::SmoothSurfaceMesh;
    let iterations = SmoothSurfaceMesh::PARAMS
        .iter()
        .find(|p| p.name == "iterations")
        .unwrap();
    assert_eq!(
        iterations.range,
        Some((0.0, 10.0)),
        "editable display span only"
    );
    assert_eq!(iterations.default, ParamValue::Float(2.0));
    let def = manifold_nodes_water::presets::gpu_flip::render_def(
        manifold_nodes_water::presets::gpu_flip::WaterScene::dam_break(64),
    );
    let metadata = def.preset_metadata.as_ref().unwrap();
    for (id, name, default) in [
        ("mesh_relaxation", "Smoothing Value", 0.5),
        ("surface_smoothing_iterations", "Smoothing Iterations", 2.0),
    ] {
        let p = metadata.params.iter().find(|p| p.id == id).unwrap();
        assert_eq!(p.name, name);
        assert_eq!(p.section.as_deref(), Some("Water Detail"));
        assert_eq!(p.default_value, default);
        assert_eq!(metadata.bindings.iter().filter(|b| b.id == id).count(), 1);
        assert_eq!((p.min, p.max), (0.0, 10.0));
    }
    let flat = manifold_core::flatten::flatten_groups(&def).unwrap();
    assert_eq!(
        flat.nodes
            .iter()
            .filter(|n| n.type_id == "node.smooth_surface_mesh")
            .count(),
        1
    );
    assert_eq!(
        flat.nodes
            .iter()
            .filter(|n| n.type_id == "node.surface_mesh_normals")
            .count(),
        1
    );
    assert!(
        !flat
            .nodes
            .iter()
            .any(|n| n.type_id == "node.relax_surface_mesh")
    );
    let registry = manifold_node_engine::persistence::PrimitiveRegistry::with_builtin();
    let view = manifold_node_engine::freeze::install::fuse_generator_view(&def, &registry).unwrap();
    for ty in ["node.smooth_surface_mesh", "node.surface_mesh_normals"] {
        assert_eq!(
            view.def.nodes.iter().filter(|n| n.type_id == ty).count(),
            1,
            "gather/stage must survive freezing: {ty}"
        );
    }
}
