

#[test]
fn fluid_bricks_still_pool_installs_dense_clamp() {
    use manifold_node_engine::persistence::PrimitiveRegistry;
    use manifold_node_engine::freeze::install::fuse_generator_view;
    use manifold_core::effect_graph_def::EffectGraphDef;

    let json = manifold_nodes::bundled_presets::bundled_preset_json(
        &manifold_core::PresetTypeId::new("WaterStillPoolMatter"),
    )
    .expect("Still Pool bundled");
    let canonical: EffectGraphDef = serde_json::from_str(&json).expect("Still Pool parses");
    let flat = manifold_core::flatten::flatten_groups(&canonical).expect("Still Pool flattens");
    let clamp = flat
        .nodes
        .iter()
        .find(|node| node.type_id == "node.clamp_liquid_to_solids")
        .expect("Still Pool contains the clamp");
    let smooth_id = flat
        .wires
        .iter()
        .find(|wire| wire.to_node == clamp.id && wire.to_port == "levelset")
        .expect("clamp reads the final smoothing pass")
        .from_node;
    let smooth = flat
        .nodes
        .iter()
        .find(|node| node.id == smooth_id)
        .expect("final smoothing node exists");
    assert_eq!(smooth.type_id, "node.smooth_lattice");
    let fused = fuse_generator_view(&canonical, &PrimitiveRegistry::with_builtin())
        .expect("Still Pool fuses");
    let region_id = fused.node_retarget.get(&clamp.node_id).expect("clamp is fused");
    assert_eq!(
        fused.node_retarget.get(&smooth.node_id),
        Some(region_id),
        "clamp shares the final smoothing region"
    );
    let region = fused
        .def
        .nodes
        .iter()
        .find(|node| node.node_id == *region_id)
        .expect("clamp retarget names an installed node");
    assert_eq!(region.type_id, "node.wgsl_compute");
    assert!(region.wgsl_source.is_some(), "region has generated WGSL");
    assert!(
        !fused.def.nodes.iter().any(|node| node.type_id == "node.clamp_liquid_to_solids")
    );
}
