use manifold_nodes_water::presets::gpu_flip::WaterScene;
#[test]
fn whitewater_preset_archive_preserves_graph() {
    use manifold_core::{project::Project, types::LayerType, preset_type_id::PresetTypeId};
    let def = manifold_nodes_water::presets::gpu_flip::render_def(WaterScene::dam_break(64));
    let mut project = Project::default();
    let index = project.timeline.add_layer("Dam Break", LayerType::Generator, PresetTypeId::new("WaterDamBreakGpuFlip"));
    let instance = project.timeline.layers[index].gen_params_mut().unwrap();
    instance.graph = Some(def.clone());
    instance.refresh_manifest_from_graph();
    // The live graph: the authored def with each control's range and flags
    // read from the lookup (SCENE_SURFACES_DESIGN.md D2 (one live lookup)).
    let live = instance.graph.clone().expect("live generator graph");
    let path = std::env::temp_dir().join(format!("whitewater_round_trip_{}_{}.manifold", std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    manifold_io::saver::save_project(&mut project, &path, Some("Whitewater round trip"), false).expect("save actual project archive");
    let reloaded = manifold_io::loader::load_project(&path).expect("load actual project archive");
    let loaded = reloaded.timeline.layers[index].generator_graph().expect("saved generator graph");
    std::fs::remove_file(&path).expect("remove round-trip archive");
    // I11 owns archive fidelity; I1's per-tick golden owns simulation values.
    // Compare the complete definition, including nested graphs and bindings.
    assert!(loaded == &live, "I11: manifold-io preserves the complete water preset");
    // Everything the lookup does not own is the authored def's, word for word.
    assert_eq!((&loaded.nodes, &loaded.wires, &loaded.scene_modifiers), (&def.nodes, &def.wires, &def.scene_modifiers));
    let (got, want) = (loaded.preset_metadata.as_ref().unwrap(), def.preset_metadata.as_ref().unwrap());
    assert_eq!(got.bindings, want.bindings);
    let authored = |m: &manifold_core::effect_graph_def::PresetMetadata| -> Vec<_> {
        m.params.iter().map(|p| (p.id.clone(), p.name.clone(), p.default_value, p.section.clone(), p.format_string.clone())).collect()
    };
    assert_eq!(authored(got), authored(want), "control ids, order, names and defaults are the authored ones");
}
