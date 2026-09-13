use super::*;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use manifold_core::effect_graph_def::BindingTarget;
use manifold_core::scene_modifier_preset::{SceneMeshReferenceFrame, SceneNodeRef};

const SOURCE_KEY: &str = "calibrated_source";
const SOURCE_NODE: &str = "scene-source";

fn unique_temp_root(label: &str) -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let root = std::env::temp_dir().join(format!(
        "manifold-{label}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    assert!(
        !root.exists(),
        "temporary test root already exists: {}",
        root.display()
    );
    root
}

fn calibrated_scene_project(old_path: &str, stale_hash: bool) -> (Project, LayerId, String) {
    let (mut project, layer_id, modifier_id) = scene_modifier_asset_project(false);
    let embedded = project
        .embedded_presets
        .first_mut()
        .expect("scene modifier host fixture");
    let graph = &mut embedded.def;
    graph.nodes.push(node(SOURCE_NODE, "node.gltf_mesh_source"));
    let metadata = graph.preset_metadata.as_mut().expect("host metadata");
    metadata.string_params.push(sp(SOURCE_KEY, old_path, true));
    metadata.string_bindings.push(StringBindingDef {
        id: SOURCE_KEY.into(),
        label: "Calibrated Source".into(),
        default_value: old_path.into(),
        target: BindingTarget::Node {
            node_id: NodeId::new(SOURCE_NODE),
            param: "path".into(),
        },
    });

    let source = graph
        .nodes
        .iter()
        .find(|candidate| candidate.node_id == NodeId::new(SOURCE_NODE))
        .expect("calibrated source node");
    let source_hash =
        manifold_core::scene_source_identity::scene_source_definition_hash(graph, source)
            .expect("source identity hash");
    graph.scene_modifiers[0]
        .mesh_frames
        .push(SceneMeshReferenceFrame {
            target: SceneNodeRef {
                scope: Vec::new(),
                node: NodeId::new("scene-object"),
            },
            source: SceneNodeRef {
                scope: Vec::new(),
                node: NodeId::new(SOURCE_NODE),
            },
            source_definition_hash: if stale_hash {
                "stale-source-hash".into()
            } else {
                source_hash
            },
            source_offset: [-0.75, 0.2, 0.1],
            scene_radius: 3.5,
        });
    (project, layer_id, modifier_id)
}

fn frame_snapshot(graph: &EffectGraphDef) -> (SceneNodeRef, SceneNodeRef, [f64; 3], f64, String) {
    let frame = &graph.scene_modifiers[0].mesh_frames[0];
    (
        frame.target.clone(),
        frame.source.clone(),
        frame.source_offset,
        frame.scene_radius,
        frame.source_definition_hash.clone(),
    )
}

#[test]
fn scene_modifier_source_relocation_collect_all_save_reload_preserves_calibration() {
    let root = unique_temp_root("scene-source-collect");
    let source = root.join("source").join("scan.glb");
    std::fs::create_dir_all(source.parent().expect("source parent")).expect("source directory");
    std::fs::write(&source, b"placeholder glb bytes").expect("source asset");

    let (mut project, layer_id, _) = calibrated_scene_project(&source.to_string_lossy(), false);
    let original_embedded = project.embedded_presets[0].def.clone();
    let original_frame = frame_snapshot(&original_embedded);
    let project_path = root.join("show").join("show.manifold");
    let expected = project_path
        .parent()
        .expect("project parent")
        .join("Media/Meshes/scan.glb");

    let report = collect_all_and_save(&mut project, &project_path).expect("collect and save");
    assert_eq!(
        report.copied, 1,
        "mesh source should be copied once: {report:?}"
    );
    assert_eq!(
        report.re_pointed, 1,
        "calibrated source should re-point once: {report:?}"
    );
    assert!(
        expected.is_file(),
        "collected source missing: {}",
        expected.display()
    );
    assert_eq!(project.embedded_presets[0].def, original_embedded);

    let layer = project
        .timeline
        .find_layer_by_id(layer_id.as_str())
        .unwrap()
        .1;
    let graph = layer
        .generator_graph()
        .expect("re-pointing clones host graph");
    let moved_frame = frame_snapshot(graph);
    assert_eq!(moved_frame.0, original_frame.0);
    assert_eq!(moved_frame.1, original_frame.1);
    assert_eq!(moved_frame.2, original_frame.2);
    assert_eq!(moved_frame.3, original_frame.3);
    let moved_source = graph
        .nodes
        .iter()
        .find(|node| node.node_id == NodeId::new(SOURCE_NODE))
        .expect("moved source node");
    assert_eq!(
        moved_frame.4,
        manifold_core::scene_source_identity::scene_source_definition_hash(graph, moved_source)
            .expect("moved source identity hash")
    );
    let clip_path = layer.clips[0]
        .string_params
        .as_ref()
        .and_then(|params| params.get(SOURCE_KEY))
        .expect("collect materializes a per-clip source path");
    assert_eq!(Path::new(clip_path), expected);

    let loaded = crate::loader::load_project(&project_path).expect("reload collected project");
    assert_eq!(loaded.embedded_presets[0].def, original_embedded);
    let loaded_layer = loaded
        .timeline
        .find_layer_by_id(layer_id.as_str())
        .unwrap()
        .1;
    let loaded_graph = loaded_layer
        .generator_graph()
        .expect("reloaded source graph");
    assert_eq!(frame_snapshot(loaded_graph), moved_frame);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn scene_modifier_source_relocation_path_resolver_refreshes_default_and_clip_override() {
    let root = unique_temp_root("scene-source-resolve");
    let moved = root.join("moved").join("scan.glb");
    std::fs::create_dir_all(moved.parent().expect("moved parent")).expect("moved directory");
    std::fs::write(&moved, b"placeholder glb bytes").expect("moved asset");
    let old = root.join("old").join("scan.glb");
    let (mut project, layer_id, _) = calibrated_scene_project(&old.to_string_lossy(), false);
    let original_embedded = project.embedded_presets[0].def.clone();
    let original_frame = frame_snapshot(&original_embedded);
    let project_path = root.join("show").join("show.manifold");

    let result = PathResolver::resolve_all(&mut project, &project_path.to_string_lossy());
    assert_eq!(
        result.resolved_count, 1,
        "source should resolve by filename: {result:?}"
    );
    assert_eq!(project.embedded_presets[0].def, original_embedded);

    let layer = project
        .timeline
        .find_layer_by_id(layer_id.as_str())
        .unwrap()
        .1;
    let graph = layer
        .generator_graph()
        .expect("path resolution clones host graph");
    let moved_frame = frame_snapshot(graph);
    assert_eq!(moved_frame.0, original_frame.0);
    assert_eq!(moved_frame.1, original_frame.1);
    assert_eq!(moved_frame.2, original_frame.2);
    assert_eq!(moved_frame.3, original_frame.3);
    assert_ne!(moved_frame.4, original_frame.4);
    let clip_path = layer.clips[0]
        .string_params
        .as_ref()
        .and_then(|params| params.get(SOURCE_KEY))
        .expect("default source becomes a per-clip override");
    assert_eq!(
        std::fs::canonicalize(clip_path).unwrap(),
        std::fs::canonicalize(&moved).unwrap()
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn scene_modifier_source_relocation_stale_hash_rejects_atomically() {
    let root = unique_temp_root("scene-source-stale");
    let old = root.join("old").join("scan.glb");
    let new = root.join("new").join("scan.glb");
    let (mut project, layer_id, _) = calibrated_scene_project(&old.to_string_lossy(), true);
    let before = serde_json::to_value(&project).expect("serialize before stale rejection");

    assert!(!re_point_string_param(
        &mut project,
        &layer_id,
        SOURCE_KEY,
        &old.to_string_lossy(),
        &new.to_string_lossy(),
    ));
    assert_eq!(
        serde_json::to_value(&project).expect("serialize after stale rejection"),
        before,
        "stale calibration must reject before graph or clip mutation"
    );
}
