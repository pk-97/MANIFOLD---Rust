use manifold_node_engine::parameters::ParamValue;

#[test]
fn surface_stage_defaults_and_manifest_bindings() {
    use crate::node_graph::primitives::smooth_surface_mesh::SmoothSurfaceMesh;
    use manifold_node_engine::primitive::PrimitiveSpec;
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
    let def = manifold_node_engine::water::primitives::gpu_flip_preset::render_def(
        manifold_node_engine::water::primitives::gpu_flip_preset::WaterScene::dam_break(64),
    );
    let metadata = def.preset_metadata.as_ref().unwrap();
    let scene = crate::node_graph::scene_vm::SceneVm::from_def(&def).unwrap();
    let water = scene
        .objects
        .iter()
        .find_map(|object| match object {
            crate::node_graph::scene_vm::SceneObjectVm::Known(row)
                if row.liquid_domain.is_some() =>
            {
                Some(row)
            }
            _ => None,
        })
        .expect("Water object");
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
        let binding = metadata.bindings.iter().find(|b| b.id == id).unwrap();
        let manifold_core::effect_graph_def::BindingTarget::Node { node_id, .. } =
            &binding.target
        else {
            panic!("node binding")
        };
        assert!(
            water.fluid_controls.contains(node_id),
            "Water must own {id}"
        );
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
    let view =
        manifold_node_engine::freeze::install::fuse_generator_view(&def, &registry).unwrap();
    for ty in ["node.smooth_surface_mesh", "node.surface_mesh_normals"] {
        assert_eq!(
            view.def.nodes.iter().filter(|n| n.type_id == ty).count(),
            1,
            "gather/stage must survive freezing: {ty}"
        );
    }
}
