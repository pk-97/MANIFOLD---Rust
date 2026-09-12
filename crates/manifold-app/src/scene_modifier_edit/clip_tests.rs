use std::path::Path;

use manifold_core::clip::TimelineClip;
use manifold_core::effect_graph_def::{
    BindingTarget, EffectGraphNode, StringBindingDef, StringParamSpecDef,
};
use manifold_core::layer::Layer;
use manifold_core::project::Project;
use manifold_core::units::Beats;
use manifold_core::{ClipId, LayerId, NodeId, PresetTypeId};
use manifold_editing::commands::clip::SetClipStringParamCommand;
use manifold_editing::service::EditingService;
use manifold_renderer::node_graph::gltf_import::assemble_import_graph;

use super::{SceneModifierAction, build_action, with_admission};

const MUSHROOM_FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/gltf/cc0___mushroom.glb"
);
const CLIP_SOURCE_PARAM: &str = "clip_model_file";

fn find_mesh_source(nodes: &[EffectGraphNode]) -> Option<NodeId> {
    for node in nodes {
        if node.type_id == "node.gltf_mesh_source" {
            return Some(node.node_id.clone());
        }
        if let Some(group) = node.group.as_ref()
            && let Some(found) = find_mesh_source(&group.nodes)
        {
            return Some(found);
        }
    }
    None
}

fn project_with_mushroom_clip() -> (Project, LayerId, ClipId, String) {
    let (mut graph, _) = assemble_import_graph(Path::new(MUSHROOM_FIXTURE))
        .expect("production mushroom import must assemble");
    let source_node_id =
        find_mesh_source(&graph.nodes).expect("mushroom has a nested gltf mesh source");
    let source_path = graph
        .preset_metadata
        .as_ref()
        .unwrap()
        .string_bindings
        .iter()
        .find(|binding| {
            matches!(&binding.target,
            BindingTarget::Node { node_id, param } if node_id == &source_node_id && param == "path")
        })
        .expect("importer binds the real source path")
        .default_value
        .clone();

    // Keep a dedicated outer string field in the fixture. It addresses the
    // real imported source node, so the clip command exercises the same
    // binding path as a user-authored file override.
    let metadata = graph
        .preset_metadata
        .as_mut()
        .expect("imported graph carries preset metadata");
    metadata.string_params.push(StringParamSpecDef {
        id: CLIP_SOURCE_PARAM.into(),
        name: "Clip Model File".into(),
        default_value: source_path.clone(),
        is_file_picker: true,
        use_dropdown: false,
        is_file_path: true,
    });
    metadata.string_bindings.push(StringBindingDef {
        id: CLIP_SOURCE_PARAM.into(),
        label: "Clip Model File".into(),
        default_value: source_path.clone(),
        target: BindingTarget::Node {
            node_id: source_node_id,
            param: "path".into(),
        },
    });

    let mut layer =
        Layer::new_generator("Mushroom".into(), PresetTypeId::new("PhotoscanBaseline"), 0);
    let layer_id = LayerId::new("clip-admission-layer");
    layer.layer_id = layer_id.clone();
    let host = layer.gen_params_or_init();
    host.graph = Some(graph);
    host.refresh_manifest_from_graph();

    let mut clip = TimelineClip::new_generator(Beats::ZERO, Beats::from_f32(4.0));
    clip.layer_id = layer_id.clone();
    let clip_id = clip.id.clone();
    layer.clips.push(clip);

    let mut project = Project::default();
    project.timeline.layers.push(layer);
    (project, layer_id, clip_id, source_path)
}

fn apply_elastic(project: &mut Project, layer_id: &LayerId, service: &mut EditingService) {
    let command = build_action(
        project,
        SceneModifierAction::Add(layer_id.clone(), "ElasticSculpture".into()),
    )
    .expect("elastic modifier action builds");
    service.execute(with_admission(command), project);
    assert!(service.take_rejection().is_none());
}

fn clip_string(project: &mut Project, clip_id: &ClipId) -> Option<String> {
    project
        .timeline
        .find_clip_by_id(clip_id)
        .and_then(|clip| clip.string_params.as_ref())
        .and_then(|params| params.get(CLIP_SOURCE_PARAM))
        .cloned()
}

#[test]
fn different_clip_source_is_rejected_atomically_after_real_calibration() {
    let (mut project, layer_id, clip_id, _source_path) = project_with_mushroom_clip();
    let mut service = EditingService::new();
    apply_elastic(&mut project, &layer_id, &mut service);

    let before = serde_json::to_value(&project).expect("project serializes");
    let version = service.data_version();
    let can_undo = service.can_undo();
    let can_redo = service.can_redo();
    let command = SetClipStringParamCommand::new(
        clip_id.clone(),
        CLIP_SOURCE_PARAM.into(),
        None,
        Some("/tmp/a-different-mushroom.glb".into()),
    );
    service.execute(with_admission(Box::new(command)), &mut project);

    let rejection = service
        .take_rejection()
        .expect("a different clip source must violate calibrated source identity");
    assert!(rejection.contains("calibrated source"), "{rejection}");
    assert_eq!(clip_string(&mut project, &clip_id), None);
    assert_eq!(serde_json::to_value(&project).unwrap(), before);
    assert_eq!(service.data_version(), version);
    assert_eq!(service.can_undo(), can_undo);
    assert_eq!(service.can_redo(), can_redo);
}

#[test]
fn same_clip_source_override_is_admissible_and_round_trips() {
    let (mut project, layer_id, clip_id, source_path) = project_with_mushroom_clip();
    let mut service = EditingService::new();
    apply_elastic(&mut project, &layer_id, &mut service);
    let before = serde_json::to_value(&project).unwrap();

    let command = SetClipStringParamCommand::new(
        clip_id.clone(),
        CLIP_SOURCE_PARAM.into(),
        None,
        Some(source_path.clone()),
    );
    service.execute(with_admission(Box::new(command)), &mut project);
    assert!(service.take_rejection().is_none());
    let changed = serde_json::to_value(&project).unwrap();
    assert_eq!(
        clip_string(&mut project, &clip_id),
        Some(source_path.clone())
    );
    assert_ne!(changed, before);

    assert!(service.undo(&mut project));
    assert_eq!(serde_json::to_value(&project).unwrap(), before);
    assert_eq!(clip_string(&mut project, &clip_id), None);
    assert!(service.redo(&mut project));
    assert_eq!(serde_json::to_value(&project).unwrap(), changed);
    assert_eq!(clip_string(&mut project, &clip_id), Some(source_path));
}
