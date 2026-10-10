use manifold_nodes_water::presets::gpu_flip::WaterScene;
#[test]
fn whitewater_preset_archive_preserves_graph() {
    use manifold_core::{project::Project, types::LayerType, preset_type_id::PresetTypeId};
    let def = manifold_nodes_water::presets::gpu_flip::render_def(WaterScene::dam_break(64));
    let mut project = Project::default();
    let index = project.timeline.add_layer("Dam Break", LayerType::Generator, PresetTypeId::new("WaterDamBreakGpuFlip"));
    let instance = project.timeline.layers[index].gen_params_or_init();
    instance.graph = Some(def.clone());
    instance.refresh_manifest_from_graph();
    let path = std::env::temp_dir().join(format!("whitewater_round_trip_{}_{}.manifold", std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    manifold_io::saver::save_project(&mut project, &path, Some("Whitewater round trip"), false).expect("save actual project archive");
    let reloaded = manifold_io::loader::load_project(&path).expect("load actual project archive");
    let loaded = reloaded.timeline.layers[index].generator_graph().expect("saved generator graph");
    std::fs::remove_file(&path).expect("remove round-trip archive");
    // I11 owns archive fidelity; I1's per-tick golden owns simulation values.
    // Compare the complete definition, including nested graphs and bindings.
    assert!(loaded == &def, "I11: manifold-io preserves the complete water preset");
}
