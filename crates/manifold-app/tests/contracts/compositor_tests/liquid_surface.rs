/// On stage the clamp's dense form fuses into the last smoothing pass; the
/// editor and thumbnail use its standalone scheduled kernel. The Still Pool,
/// where it holds the water face at the front glass, must render the same both ways.
#[test]
fn fluid_clamp_scheduled_boundary_renders_like_unfrozen() {
    use manifold_core::effect_graph_def::EffectGraphDef;
    use manifold_core::preset_def::PresetKind;

    let device = manifold_gpu::testkit::test_device();
    let registry = manifold_node_engine::persistence::PrimitiveRegistry::with_builtin();
    let json = manifold_renderer::node_graph::bundled_presets::bundled_preset_json(&manifold_core::PresetTypeId::new(
        "WaterStillPoolMatter",
    ))
    .expect("Still Pool bundled");
    let canonical: EffectGraphDef = serde_json::from_str(&json).expect("Still Pool parses");
    let fused = manifold_node_engine::freeze::install::fuse_generator_view(&canonical, &registry)
        .expect("the Still Pool fuses and builds");
    assert!(
        !fused.def.nodes.iter().any(|n| n.type_id == "node.clamp_liquid_to_solids"),
        "the clamp's declared dense form fuses into the final smoothing pass"
    );
    let arc = device.arc();
    let render = |def: &EffectGraphDef| {
        manifold_compositor::preset_thumbnail::render_preset_thumbnail(&arc, PresetKind::Generator, def, 256, 144, false)
            .expect("Still Pool renders")
    };
    let unfused = render(&canonical);
    let fused = render(&fused.def);
    assert!(unfused == fused, "the fused clamp must render bit for bit like the unfused one");
}
