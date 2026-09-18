use std::path::Path;

use manifold_core::effect_graph_def::{EffectGraphDef, EffectGraphNode, SerializedParamValue};
use manifold_core::layer::Layer;
use manifold_core::project::Project;
use manifold_core::scene_modifier_preset::SceneMeshReferenceFrame;
use manifold_core::{GraphTarget, LayerId, NodeId, PresetTypeId};
use manifold_editing::commands::graph::{
    AddSceneObjectCommand, DuplicateSceneObjectCommand, RemoveSceneObjectCommand,
};
use manifold_editing::service::EditingService;
use manifold_renderer::node_graph::gltf_import::assemble_import_graph;

use super::{SceneModifierAction, build_action, with_admission};

const MUSHROOM_FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/gltf/cc0___mushroom.glb"
);

fn project_with_mushroom() -> (Project, LayerId) {
    let (graph, _) = assemble_import_graph(Path::new(MUSHROOM_FIXTURE))
        .expect("production mushroom import must assemble");
    let mut layer =
        Layer::new_generator("Mushroom".into(), PresetTypeId::new("PhotoscanBaseline"), 0);
    let layer_id = LayerId::new("frame-admission-layer");
    layer.layer_id = layer_id.clone();
    let host = layer.gen_params_or_init();
    host.graph = Some(graph);
    host.refresh_manifest_from_graph();

    let mut project = Project::default();
    project.timeline.layers.push(layer);
    (project, layer_id)
}

fn host_graph<'a>(project: &'a Project, layer_id: &LayerId) -> &'a EffectGraphDef {
    project
        .graph_target_owner(&GraphTarget::Generator(layer_id.clone()))
        .and_then(|instance| instance.graph_def().as_ref())
        .expect("fixture generator graph")
}

fn host_graph_mut<'a>(project: &'a mut Project, layer_id: &LayerId) -> &'a mut EffectGraphDef {
    project
        .graph_target_owner_mut(&GraphTarget::Generator(layer_id.clone()))
        .and_then(|instance| instance.graph.as_mut())
        .expect("fixture generator graph")
}

fn modifier_id(project: &Project, layer_id: &LayerId) -> NodeId {
    host_graph(project, layer_id)
        .scene_modifiers
        .first()
        .expect("modifier inserted")
        .id
        .clone()
}

fn frames(
    project: &Project,
    layer_id: &LayerId,
    modifier_id: &NodeId,
) -> Vec<SceneMeshReferenceFrame> {
    host_graph(project, layer_id)
        .scene_modifiers
        .iter()
        .find(|modifier| &modifier.id == modifier_id)
        .expect("modifier exists")
        .mesh_frames
        .clone()
}

fn find_render_scene(nodes: &[EffectGraphNode], scope: &mut Vec<u32>) -> Option<(Vec<u32>, u32)> {
    for node in nodes {
        if node.type_id == "node.render_scene" {
            return Some((scope.clone(), node.id));
        }
        if let Some(group) = node.group.as_ref() {
            scope.push(node.id);
            let found = find_render_scene(&group.nodes, scope);
            scope.pop();
            if found.is_some() {
                return found;
            }
        }
    }
    None
}

fn nodes_at_scope<'a>(
    nodes: &'a [EffectGraphNode],
    scope: &[u32],
) -> Option<&'a [EffectGraphNode]> {
    let Some(group_id) = scope.first() else {
        return Some(nodes);
    };
    let group = nodes
        .iter()
        .find(|node| node.id == *group_id)?
        .group
        .as_ref()?;
    nodes_at_scope(&group.nodes, &scope[1..])
}

fn render_scene(project: &Project, layer_id: &LayerId) -> (Vec<u32>, u32, u32) {
    let graph = host_graph(project, layer_id);
    let (scope, id) = find_render_scene(&graph.nodes, &mut Vec::new())
        .expect("mushroom fixture has one render scene");
    let objects = nodes_at_scope(&graph.nodes, &scope)
        .expect("render scene scope exists")
        .iter()
        .find(|node| node.id == id)
        .and_then(|node| node.params.get("objects"))
        .and_then(manifold_core::effects::serialized_value_as_f32)
        .expect("render scene has object count")
        .max(0.0) as u32;
    (scope, id, objects)
}

fn apply_peel(project: &mut Project, layer_id: &LayerId, service: &mut EditingService) -> NodeId {
    let command = build_action(
        project,
        SceneModifierAction::Add(layer_id.clone(), "SurfacePeel".into()),
    )
    .expect("elastic modifier action builds");
    service.execute(with_admission(command), project);
    assert!(service.take_rejection().is_none());
    modifier_id(project, layer_id)
}

fn add_cube_object(project: &mut Project, layer_id: &LayerId, service: &mut EditingService) {
    let (scope, render_id, index) = render_scene(project, layer_id);
    let target = GraphTarget::Generator(layer_id.clone());
    let command = AddSceneObjectCommand::new(
        target,
        scope,
        render_id,
        index,
        (900.0, 200.0 + 40.0 * index as f32),
        manifold_renderer::node_graph::scene_exposure::metadata_for_node_type(
            "node.phong_material",
        ),
        manifold_renderer::node_graph::scene_exposure::metadata_for_node_type("node.transform_3d"),
        manifold_renderer::node_graph::scene_exposure::metadata_for_node_type("node.scene_object"),
        host_graph(project, layer_id).clone(),
    );
    service.execute(with_admission(Box::new(command)), project);
}

fn duplicate_imported_object(
    project: &mut Project,
    layer_id: &LayerId,
    service: &mut EditingService,
) {
    let (scope, render_id, _) = render_scene(project, layer_id);
    let command = DuplicateSceneObjectCommand::new(
        GraphTarget::Generator(layer_id.clone()),
        scope,
        render_id,
        0,
        host_graph(project, layer_id).clone(),
    );
    service.execute(with_admission(Box::new(command)), project);
}

fn remove_object(
    project: &mut Project,
    layer_id: &LayerId,
    service: &mut EditingService,
    index: u32,
) {
    let (scope, render_id, _) = render_scene(project, layer_id);
    let command = RemoveSceneObjectCommand::new(
        GraphTarget::Generator(layer_id.clone()),
        scope,
        render_id,
        index,
        host_graph(project, layer_id).clone(),
    );
    service.execute(with_admission(Box::new(command)), project);
}

fn mutate_source(project: &mut Project, layer_id: &LayerId) -> String {
    fn visit(nodes: &mut [EffectGraphNode]) -> Option<String> {
        for node in nodes {
            if node.type_id == "node.gltf_mesh_source" {
                let old = node.params.insert(
                    "translate_x".into(),
                    SerializedParamValue::Float { value: 1.0 },
                );
                return Some(
                    old.map_or_else(String::new, |value| serde_json::to_string(&value).unwrap()),
                );
            }
            if let Some(group) = node.group.as_mut()
                && let Some(old) = visit(&mut group.nodes)
            {
                return Some(old);
            }
        }
        None
    }
    visit(&mut host_graph_mut(project, layer_id).nodes).expect("mushroom has a mesh source")
}

fn restore_source(project: &mut Project, layer_id: &LayerId, old: &str) {
    fn visit(nodes: &mut [EffectGraphNode], old: &str) -> bool {
        for node in nodes {
            if node.type_id == "node.gltf_mesh_source" {
                if old.is_empty() {
                    node.params.remove("translate_x");
                } else {
                    node.params.insert(
                        "translate_x".into(),
                        serde_json::from_str(old).expect("saved source parameter"),
                    );
                }
                return true;
            }
            if let Some(group) = node.group.as_mut()
                && visit(&mut group.nodes, old)
            {
                return true;
            }
        }
        false
    }
    assert!(visit(&mut host_graph_mut(project, layer_id).nodes, old));
}

#[test]
fn all_objects_add_object_captures_only_new_frame_and_round_trips() {
    let (mut project, layer_id) = project_with_mushroom();
    let mut service = EditingService::new();
    let modifier = apply_peel(&mut project, &layer_id, &mut service);
    let before_frames = frames(&project, &layer_id, &modifier);
    let before_graph = serde_json::to_value(&project).expect("project serializes");

    duplicate_imported_object(&mut project, &layer_id, &mut service);
    assert!(service.take_rejection().is_none());
    let after_frames = frames(&project, &layer_id, &modifier);
    assert_eq!(after_frames.len(), before_frames.len() + 1);
    for old in &before_frames {
        assert!(
            after_frames
                .iter()
                .any(|frame| frame.target == old.target && frame == old)
        );
    }
    let after_graph = serde_json::to_value(&project).expect("project serializes");

    assert!(service.undo(&mut project));
    assert_eq!(serde_json::to_value(&project).unwrap(), before_graph);
    assert_eq!(frames(&project, &layer_id, &modifier), before_frames);
    assert!(service.redo(&mut project));
    assert_eq!(serde_json::to_value(&project).unwrap(), after_graph);
    assert_eq!(frames(&project, &layer_id, &modifier), after_frames);
}

#[test]
fn all_objects_remove_prunes_one_frame_and_undo_restores_it() {
    let (mut project, layer_id) = project_with_mushroom();
    let mut service = EditingService::new();
    let modifier = apply_peel(&mut project, &layer_id, &mut service);
    duplicate_imported_object(&mut project, &layer_id, &mut service);
    assert!(service.take_rejection().is_none());
    let added_graph = serde_json::to_value(&project).unwrap();
    let added_frames = frames(&project, &layer_id, &modifier);
    let (_, _, index) = render_scene(&project, &layer_id);

    remove_object(&mut project, &layer_id, &mut service, index - 1);
    assert!(service.take_rejection().is_none());
    let removed_frames = frames(&project, &layer_id, &modifier);
    assert_eq!(removed_frames.len() + 1, added_frames.len());
    assert!(
        added_frames
            .iter()
            .any(|frame| !removed_frames.contains(frame))
    );

    assert!(service.undo(&mut project));
    assert_eq!(serde_json::to_value(&project).unwrap(), added_graph);
    assert_eq!(frames(&project, &layer_id, &modifier), added_frames);
}

#[test]
fn changed_surviving_source_rejects_redo_without_losing_captured_frames() {
    let (mut project, layer_id) = project_with_mushroom();
    let mut service = EditingService::new();
    let modifier = apply_peel(&mut project, &layer_id, &mut service);
    duplicate_imported_object(&mut project, &layer_id, &mut service);
    assert!(service.take_rejection().is_none());
    let captured = frames(&project, &layer_id, &modifier);
    assert!(service.undo(&mut project));
    let old_source_param = mutate_source(&mut project, &layer_id);
    let unchanged = serde_json::to_value(&project).unwrap();
    let version = service.data_version();

    assert!(!service.redo(&mut project));
    assert!(service.can_redo());
    assert!(service.take_rejection().is_some());
    assert_eq!(service.data_version(), version);
    assert_eq!(serde_json::to_value(&project).unwrap(), unchanged);

    restore_source(&mut project, &layer_id, &old_source_param);
    assert!(service.redo(&mut project));
    assert_eq!(frames(&project, &layer_id, &modifier), captured);
}

#[test]
fn add_cube_object_is_rejected_atomically_for_coordinate_context() {
    let (mut project, layer_id) = project_with_mushroom();
    let mut service = EditingService::new();
    apply_peel(&mut project, &layer_id, &mut service);
    let before = serde_json::to_value(&project).unwrap();
    let version = service.data_version();
    let had_history = service.can_undo();

    add_cube_object(&mut project, &layer_id, &mut service);

    let rejection = service
        .take_rejection()
        .expect("a fresh cube has no qualified mesh coordinate frame");
    assert!(rejection.contains("direct static glTF") || rejection.contains("coordinate"));
    assert_eq!(serde_json::to_value(&project).unwrap(), before);
    assert_eq!(service.data_version(), version);
    assert_eq!(service.can_undo(), had_history);
}

#[test]
fn math_view_add_remove_undo_redo_round_trips() {
    let (mut project, layer_id) = project_with_mushroom();
    let mut service = EditingService::new();

    let add = build_action(
        &project,
        SceneModifierAction::Add(layer_id.clone(), "MathView".into()),
    )
    .expect("math view action builds");
    service.execute(with_admission(add), &mut project);
    assert!(service.take_rejection().is_none());
    let id = modifier_id(&project, &layer_id);
    assert!(
        host_graph(&project, &layer_id)
            .scene_modifiers
            .iter()
            .find(|modifier| modifier.id == id)
            .is_some_and(|modifier| {
                manifold_core::scene_modifier_math_view::is_math_view_recipe(&modifier.graph)
            }),
        "added modifier is the standalone Math View recipe"
    );

    // Singleton authoring gate: a second Math View in the same scene is
    // rejected at build time (load migration is exempt from this rule).
    assert!(
        build_action(&project, SceneModifierAction::Add(layer_id.clone(), "MathView".into()))
            .is_err(),
        "second Math View add is a singleton collision"
    );

    let added_graph = serde_json::to_value(&project).expect("project serializes");
    assert!(service.undo(&mut project));
    assert!(host_graph(&project, &layer_id).scene_modifiers.is_empty());
    assert!(service.redo(&mut project));
    assert_eq!(serde_json::to_value(&project).unwrap(), added_graph);
    assert_eq!(modifier_id(&project, &layer_id), id);

    let remove = build_action(&project, SceneModifierAction::Remove(layer_id.clone(), id.clone()))
        .expect("remove action builds");
    service.execute(with_admission(remove), &mut project);
    assert!(service.take_rejection().is_none());
    assert!(host_graph(&project, &layer_id).scene_modifiers.is_empty());
    assert!(service.undo(&mut project));
    assert_eq!(modifier_id(&project, &layer_id), id);
    assert!(service.redo(&mut project));
    assert!(host_graph(&project, &layer_id).scene_modifiers.is_empty());
}
