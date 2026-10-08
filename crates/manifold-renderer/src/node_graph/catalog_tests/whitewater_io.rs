use manifold_node_engine::testkit::whitewater_fingerprints::run;
use manifold_node_engine::testkit::whitewater_scene::with_tick_probe;
use manifold_node_engine::water::primitives::gpu_flip_preset::WaterScene;
#[test]
fn whitewater_packed_preset_save_load_matches_fingerprints() {
    use manifold_core::{project::Project, types::LayerType, preset_type_id::PresetTypeId};
    use manifold_node_engine::testkit::whitewater_scene::with_whitewater_reports;
    let def = manifold_node_engine::water::primitives::gpu_flip_preset::render_def(WaterScene::dam_break(64));
    let mut project = Project::default();
    let index = project.timeline.add_layer("Dam Break", LayerType::Generator, PresetTypeId::new("WaterDamBreakGpuFlip"));
    project.timeline.layers[index].gen_params_or_init().graph = Some(def.clone());
    let path = std::env::temp_dir().join(format!("whitewater_round_trip_{}_{}.manifold", std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    manifold_io::saver::save_project(&mut project, &path, Some("Whitewater round trip"), false).expect("save actual project archive");
    let reloaded = manifold_io::loader::load_project(&path).expect("load actual project archive");
    let loaded = reloaded.timeline.layers[index].generator_graph().expect("saved generator graph").clone();
    std::fs::remove_file(&path).expect("remove round-trip archive");
    let mut before = Vec::new();
    let mut after = Vec::new();
    run("save_load", with_tick_probe(with_whitewater_reports(def)), &mut before);
    run("save_load", with_tick_probe(with_whitewater_reports(loaded)), &mut after);
    assert_eq!(before, after, "I11: I1 fingerprints after manifold-io save/load");
}