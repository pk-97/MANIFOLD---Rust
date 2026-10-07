//! Collect/load portability for a published native FLIP source identity.

use manifold_node_engine::runtime::*;
use manifold_node_engine::testkit::physics_history::*;
use crate::node_graph::*;

use manifold_node_engine::scene::source_asset::SourceAssetIdentity;
use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_core::layer::Layer;
use manifold_core::preset_def::PresetKind;
use manifold_core::project::{EmbeddedOrigin, EmbeddedPreset, Project};
use manifold_core::{Beats, NodeId, PresetTypeId, Seconds};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const PRESET_ID: &str = "collected-flip";

fn time(seconds: f64) -> FrameTime {
    FrameTime {
        seconds: Seconds(seconds),
        beats: Beats(seconds * 2.0),
        delta: Seconds::ZERO,
        frame_count: (seconds * 60.0) as i64,
    }
}

fn source_status(runtime: &PresetRuntime) -> SourceAssetIdentity<'_> {
    let mesh = runtime
        .graph
        .instance_by_node_id(&NodeId::new("mesh"))
        .expect("mesh node");
    let node = runtime.graph.get_node(mesh).expect("mesh instance");
    node.node.source_asset_identity(&node.params)
}

fn published_identity(runtime: &PresetRuntime) -> Option<Result<[u8; 32], String>> {
    let fluid = runtime
        .graph
        .instance_by_node_id(&NodeId::new("fluid"))
        .expect("fluid node");
    manifold_node_engine::runtime::testkit::published_identity(runtime, fluid)
}

fn settle_source_at(runtime: &mut PresetRuntime, seconds: f64) -> [u8; 32] {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut ready_once = None;
    loop {
        runtime.execute_frame(time(seconds));
        let source = source_status(runtime);
        let published = published_identity(runtime);
        if let SourceAssetIdentity::Failed(error) = &source {
            panic!("Live source failed while publishing identity: {error}");
        }
        if let Some(Err(error)) = &published
            && error != "Source asset is still loading"
        {
            panic!("Live source identity publication failed: {error}");
        }
        if !runtime.warmup_pending()
            && matches!(&source, SourceAssetIdentity::Ready(_))
            && let Some(Ok(identity)) = published
        {
            if let Some(previous) = ready_once {
                assert_eq!(previous, identity, "held source identity changed");
                return identity;
            }
            ready_once = Some(identity);
        } else {
            ready_once = None;
        }
        assert!(
            Instant::now() < deadline,
            "Live source publication did not settle at {seconds}s; source={source:?}, published={published:?}, pending={}",
            runtime.warmup_pending(),
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn settle_source_failure(runtime: &mut PresetRuntime) -> (String, String) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        runtime.execute_frame(time(0.0));
        let source = source_status(runtime);
        let published = published_identity(runtime);
        if let (SourceAssetIdentity::Failed(source_error), Some(Err(published_error))) =
            (&source, &published)
            && *source_error == published_error
        {
            return (source_error.to_string(), published_error.clone());
        }
        assert!(
            Instant::now() < deadline,
            "missing source did not publish an explicit failure; source={source:?}, published={published:?}, pending={}",
            runtime.warmup_pending(),
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn assert_observed_time(expected: f64) {
    let observed = observed_fluid_time()
        .expect("fluid observer accepted a simulation time");
    assert!(
        (f64::from(observed) - expected).abs() < 1e-5,
        "observer saw simulation time {observed}, expected {expected}"
    );
}

fn strings(model: &Path) -> BTreeMap<String, String> {
    BTreeMap::from([(
        "model_file".to_string(),
        model.to_string_lossy().into_owned(),
    )])
}

fn definition(model: &Path) -> EffectGraphDef {
    let mut value = serde_json::to_value(runtime_definition()).unwrap();
    value["nodes"].as_array_mut().unwrap().extend([
        serde_json::json!({
            "id": 5,
            "nodeId": "mesh",
            "typeId": "node.gltf_mesh_source",
            "params": {"path": {"type":"String", "value": model.to_string_lossy()}}
        }),
        serde_json::json!({
            "id": 6,
            "nodeId": "transform",
            "typeId": "node.transform_3d"
        }),
        serde_json::json!({
            "id": 7,
            "nodeId": "role",
            "typeId": "node.fluid_role_source",
            "params": {
                "role": {"type":"Enum", "value":3},
                "geometry": {"type":"Enum", "value":0}
            }
        }),
    ]);
    value["wires"].as_array_mut().unwrap().extend([
        serde_json::json!({"fromNode":5,"fromPort":"source","toNode":7,"toPort":"mesh_0"}),
        serde_json::json!({"fromNode":6,"fromPort":"transform","toNode":7,"toPort":"transform"}),
        serde_json::json!({"fromNode":7,"fromPort":"role","toNode":0,"toPort":"role_0"}),
    ]);
    value["presetMetadata"] = serde_json::json!({
        "id": PRESET_ID,
        "displayName": "Collected FLIP",
        "category": "Diagnostic",
        "oscPrefix": "collected_flip",
        "params": [],
        "bindings": [],
        "stringParams": [
            {"id":"model_file","name":"Model","defaultValue":model.to_string_lossy(),"isFilePicker":true}
        ],
        "stringBindings": [
            {"id":"model_file","label":"Model","defaultValue":model.to_string_lossy(),"target":{"kind":"node","nodeId":"mesh","param":"path"}}
        ]
    });
    serde_json::from_value(value).unwrap()
}

fn project(def: EffectGraphDef, model: &Path) -> Project {
    let preset_id = PresetTypeId::new(PRESET_ID);
    let mut project = Project::default();
    project.upsert_embedded_preset(EmbeddedPreset {
        kind: PresetKind::Generator,
        def,
        origin: EmbeddedOrigin::Saved,
    });
    let mut layer = Layer::new_generator("Collected FLIP".into(), preset_id, 0);
    let mut clip = manifold_core::clip::TimelineClip::new_generator(Beats::ZERO, Beats(8.0));
    clip.string_params = Some(strings(model));
    layer.clips.push(clip);
    project.timeline.layers.push(layer);
    project
}

fn loaded_inputs(project: &Project) -> (EffectGraphDef, BTreeMap<String, String>) {
    let id = PresetTypeId::new(PRESET_ID);
    let def = project
        .embedded_preset(&id)
        .expect("embedded collected preset")
        .def
        .clone();
    let values = project.timeline.layers[0].clips[0]
        .string_params
        .clone()
        .expect("materialized string overrides");
    (def, values)
}

fn copy_tree(source: &Path, destination: &Path) {
    std::fs::create_dir_all(destination).unwrap();
    for entry in std::fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let target = destination.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

fn external_buffer_path(model: &Path) -> PathBuf {
    let document: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(model).unwrap()).unwrap();
    let uri = document["buffers"][0]["uri"]
        .as_str()
        .expect("external glTF buffer URI");
    model.parent().expect("model directory").join(uri)
}

fn write_cube(root: &Path) -> PathBuf {
    let positions = [
        [-1.0_f32, -1.0, -1.0],
        [1.0, -1.0, -1.0],
        [1.0, 1.0, -1.0],
        [-1.0, 1.0, -1.0],
        [-1.0, -1.0, 1.0],
        [1.0, -1.0, 1.0],
        [1.0, 1.0, 1.0],
        [-1.0, 1.0, 1.0],
    ];
    let indices: [u16; 36] = [
        0, 2, 1, 0, 3, 2, 4, 5, 6, 4, 6, 7, 0, 4, 7, 0, 7, 3, 1, 2, 6, 1, 6, 5, 0, 1, 5, 0, 5, 4,
        3, 7, 6, 3, 6, 2,
    ];
    let mut bytes = Vec::with_capacity(168);
    for position in positions {
        for value in position {
            bytes.extend(value.to_le_bytes());
        }
    }
    for index in indices {
        bytes.extend(index.to_le_bytes());
    }
    let binary = root.join("cube.bin");
    std::fs::write(&binary, bytes).unwrap();
    let document = serde_json::json!({
        "asset":{"version":"2.0"},"scene":0,
        "scenes":[{"nodes":[0]}],"nodes":[{"mesh":0}],
        "meshes":[{"primitives":[{"attributes":{"POSITION":0},"indices":1}]}],
        "buffers":[{"uri":"cube.bin","byteLength":168}],
        "bufferViews":[
            {"buffer":0,"byteOffset":0,"byteLength":96},
            {"buffer":0,"byteOffset":96,"byteLength":72}
        ],
        "accessors":[
            {"bufferView":0,"componentType":5126,"count":8,"type":"VEC3","min":[-1,-1,-1],"max":[1,1,1]},
            {"bufferView":1,"componentType":5123,"count":36,"type":"SCALAR"}
        ]
    });
    let model = root.join("cube.gltf");
    std::fs::write(&model, document.to_string()).unwrap();
    model
}

/// This proves Live source publication and collection portability. Scene
/// Record/Playback remains guarded by FluidRuntime::observe_coupled_scene_with_field.
#[test]
fn collected_flip_source_identity_survives_relocation_and_geometry_changes() {
    let root = std::env::temp_dir().join(format!(
        "manifold-flip-collection-{}",
        manifold_core::short_id()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let root = std::fs::canonicalize(&root).unwrap();
    let source_root = root.join("source");
    let show_root = root.join("Show");
    let moved_root = root.join("Moved");
    std::fs::create_dir_all(&source_root).unwrap();
    std::fs::create_dir_all(&show_root).unwrap();
    let model = write_cube(&source_root);
    let original_buffer = std::fs::read(external_buffer_path(&model)).unwrap();

    let mut original = runtime_from_definition(definition(&model));
    original.set_string_params(Some(&strings(&model)));
    set_observed_fluid_time(None);
    let original_identity = settle_source_at(&mut original, 0.0);
    assert_observed_time(0.0);
    let held_identity = settle_source_at(&mut original, 0.1);
    assert_observed_time(0.1);
    assert_eq!(held_identity, original_identity);
    drop(original);

    let project_path = show_root.join("Show.manifold");
    let mut project = project(definition(&model), &model);
    let report = manifold_io::collect::collect_all_and_save(&mut project, &project_path).unwrap();
    assert_eq!(report.missing, 0);

    let loaded = manifold_io::loader::load_project(&project_path).unwrap();
    let (loaded_def, loaded_values) = loaded_inputs(&loaded);
    let loaded_model = PathBuf::from(loaded_values.get("model_file").unwrap());
    assert!(loaded_model.starts_with(&show_root));
    assert_eq!(
        std::fs::read(external_buffer_path(&loaded_model)).unwrap(),
        original_buffer,
        "collected external buffer changed"
    );
    let mut loaded_runtime = runtime_from_definition(loaded_def);
    loaded_runtime.set_string_params(Some(&loaded_values));
    set_observed_fluid_time(None);
    let loaded_identity = settle_source_at(&mut loaded_runtime, 0.0);
    assert_observed_time(0.0);
    assert_eq!(loaded_identity, original_identity);
    let loaded_held_identity = settle_source_at(&mut loaded_runtime, 0.1);
    assert_observed_time(0.1);
    assert_eq!(loaded_held_identity, original_identity);
    drop(loaded_runtime);

    copy_tree(&show_root, &moved_root);
    let moved = manifold_io::loader::load_project(&moved_root.join("Show.manifold")).unwrap();
    let (moved_def, moved_values) = loaded_inputs(&moved);
    let moved_model = PathBuf::from(moved_values.get("model_file").unwrap());
    assert!(moved_model.starts_with(&moved_root));
    let mut moved_runtime = runtime_from_definition(moved_def);
    moved_runtime.set_string_params(Some(&moved_values));
    set_observed_fluid_time(None);
    assert_eq!(settle_source_at(&mut moved_runtime, 0.0), original_identity);
    assert_observed_time(0.0);
    assert_eq!(settle_source_at(&mut moved_runtime, 0.1), original_identity);
    assert_observed_time(0.1);
    drop(moved_runtime);

    std::fs::remove_dir_all(&source_root).unwrap();
    std::fs::remove_dir_all(&show_root).unwrap();
    let moved_again = manifold_io::loader::load_project(&moved_root.join("Show.manifold")).unwrap();
    let (moved_again_def, moved_again_values) = loaded_inputs(&moved_again);
    let mut moved_again_runtime = runtime_from_definition(moved_again_def);
    moved_again_runtime.set_string_params(Some(&moved_again_values));
    set_observed_fluid_time(None);
    assert_eq!(
        settle_source_at(&mut moved_again_runtime, 0.0),
        original_identity
    );
    assert_observed_time(0.0);
    assert_eq!(
        settle_source_at(&mut moved_again_runtime, 0.1),
        original_identity
    );
    assert_observed_time(0.1);
    drop(moved_again_runtime);

    let moved_buffer = external_buffer_path(&moved_model);
    let mut changed_bytes = std::fs::read(&moved_buffer).unwrap();
    assert_eq!(changed_bytes, original_buffer);
    changed_bytes[..4].copy_from_slice(&2.0_f32.to_le_bytes());
    std::fs::write(&moved_buffer, changed_bytes).unwrap();
    let changed_project =
        manifold_io::loader::load_project(&moved_root.join("Show.manifold")).unwrap();
    let (changed_def, changed_values) = loaded_inputs(&changed_project);
    let mut changed = runtime_from_definition(changed_def);
    changed.set_string_params(Some(&changed_values));
    let changed_identity = settle_source_at(&mut changed, 0.0);
    assert_ne!(changed_identity, original_identity);
    drop(changed);

    std::fs::remove_file(&moved_buffer).unwrap();
    let missing_project =
        manifold_io::loader::load_project(&moved_root.join("Show.manifold")).unwrap();
    let (missing_def, missing_values) = loaded_inputs(&missing_project);
    let mut missing = runtime_from_definition(missing_def);
    missing.set_string_params(Some(&missing_values));
    let (source_error, published_error) = settle_source_failure(&mut missing);
    assert!(!source_error.is_empty());
    assert!(!published_error.is_empty());
    drop(missing);
    std::fs::remove_dir_all(root).unwrap();
}
