use std::path::Path;

use manifold_core::effect_graph_def::{BindingTarget, EffectGraphDef, SerializedParamValue};
use manifold_core::layer::Layer;
use manifold_core::project::Project;
use manifold_core::{GraphTarget, LayerId, NodeId, PresetTypeId};
use manifold_editing::command::Command;
use manifold_editing::commands::graph::AddGraphNodeCommand;
use manifold_editing::commands::graph::SetGraphNodeParamCommand;
use manifold_editing::service::EditingService;
use manifold_renderer::node_graph::gltf_import::assemble_import_graph;

use super::{SceneModifierAction, build_action, with_admission, with_admission_snapshot};

const MUSHROOM_FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/gltf/cc0___mushroom.glb"
);

fn project_with_mushroom() -> (Project, LayerId) {
    let (graph, _) = assemble_import_graph(Path::new(MUSHROOM_FIXTURE))
        .expect("production mushroom import must assemble");
    let mut layer =
        Layer::new_generator("Mushroom".into(), PresetTypeId::new("PhotoscanBaseline"), 0);
    let layer_id = LayerId::new("admission-layer");
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

fn modifier_ids(project: &Project, layer_id: &LayerId) -> Vec<NodeId> {
    host_graph(project, layer_id)
        .scene_modifiers
        .iter()
        .map(|modifier| modifier.id.clone())
        .collect()
}

fn apply_stock(
    service: &mut EditingService,
    project: &mut Project,
    layer_id: &LayerId,
    preset: &str,
) -> NodeId {
    let command = build_action(
        project,
        SceneModifierAction::Add(layer_id.clone(), preset.into()),
    )
    .expect("stock modifier action builds");
    service.execute(with_admission(command), project);
    assert!(service.take_rejection().is_none());
    modifier_ids(project, layer_id)
        .last()
        .cloned()
        .expect("stock modifier inserted")
}

fn append_invalid_node(project: &mut Project, layer_id: &LayerId) {
    let target = GraphTarget::Generator(layer_id.clone());
    let catalog = host_graph(project, layer_id).clone();
    let mut command =
        AddGraphNodeCommand::new(target, "node.not_a_real_primitive".into(), None, catalog);
    command.execute(project);
    assert!(
        command.was_applied(),
        "invalid fixture node must be inserted for the probe"
    );
}

fn enabled_binding(project: &Project, layer_id: &LayerId, modifier_id: &NodeId) -> String {
    let graph = host_graph(project, layer_id);
    let modifier = graph
        .scene_modifiers
        .iter()
        .find(|modifier| &modifier.id == modifier_id)
        .expect("modifier exists");
    let enabled_param = modifier
        .graph
        .preset_metadata
        .as_ref()
        .expect("modifier metadata")
        .scene_modifier
        .as_ref()
        .expect("scene modifier recipe metadata")
        .enabled_param
        .clone();
    graph
        .preset_metadata
        .as_ref()
        .expect("host metadata")
        .bindings
        .iter()
        .find_map(|binding| match &binding.target {
            BindingTarget::SceneModifier {
                modifier_id: id,
                param_id,
            } if id.as_str() == modifier_id.as_str() && param_id == &enabled_param => {
                Some(binding.id.clone())
            }
            _ => None,
        })
        .expect("enabled host binding")
}

#[test]
fn duplicate_peel_actions_keep_independent_instance_controls() {
    let (mut project, layer_id) = project_with_mushroom();
    let mut service = EditingService::new();
    let first = apply_stock(&mut service, &mut project, &layer_id, "SurfacePeel");
    let second = apply_stock(&mut service, &mut project, &layer_id, "SurfacePeel");
    assert_ne!(first, second, "each add receives a stable instance id");

    let graph = host_graph(&project, &layer_id);
    let controls: Vec<(NodeId, String)> = graph
        .preset_metadata
        .as_ref()
        .expect("host metadata")
        .bindings
        .iter()
        .filter_map(|binding| match &binding.target {
            BindingTarget::SceneModifier { modifier_id, .. }
                if modifier_id.as_str() == first.as_str()
                    || modifier_id.as_str() == second.as_str() =>
            {
                Some((modifier_id.clone(), binding.id.clone()))
            }
            _ => None,
        })
        .collect();
    assert!(controls.iter().any(|(id, _)| id.as_str() == first.as_str()));
    assert!(
        controls
            .iter()
            .any(|(id, _)| id.as_str() == second.as_str())
    );
    let first_ids: Vec<String> = controls
        .iter()
        .filter(|(id, _)| id.as_str() == first.as_str())
        .map(|(_, binding)| binding.clone())
        .collect();
    let second_ids: Vec<String> = controls
        .iter()
        .filter(|(id, _)| id.as_str() == second.as_str())
        .map(|(_, binding)| binding.clone())
        .collect();
    assert!(
        first_ids
            .iter()
            .all(|binding| !second_ids.contains(binding)),
        "duplicate instances must have independent macro controls"
    );
}

#[test]
fn move_action_preserves_instance_ids_through_undo_and_redo() {
    let (mut project, layer_id) = project_with_mushroom();
    let mut service = EditingService::new();
    let first = apply_stock(&mut service, &mut project, &layer_id, "SurfacePeel");
    let second = apply_stock(&mut service, &mut project, &layer_id, "SurfacePeel");
    let target = GraphTarget::Generator(layer_id.clone());

    let move_command = build_action(
        &project,
        SceneModifierAction::Move(layer_id.clone(), first.clone(), 1),
    )
    .expect("move action builds");
    service.execute(with_admission(move_command), &mut project);
    assert_eq!(
        modifier_ids(&project, &layer_id),
        vec![second.clone(), first.clone()]
    );
    assert!(service.undo(&mut project));
    assert_eq!(
        modifier_ids(&project, &layer_id),
        vec![first.clone(), second.clone()]
    );
    assert!(service.redo(&mut project));
    assert_eq!(modifier_ids(&project, &layer_id), vec![second, first]);
    assert!(project.graph_target_owner(&target).is_some());
}

#[test]
fn oversized_photoscan_stack_is_rejected_before_project_or_history_changes() {
    let (mut project, layer_id) = project_with_mushroom();
    let mut service = EditingService::new();
    apply_stock(&mut service, &mut project, &layer_id, "ElasticSculpture");
    let mut service = EditingService::new();
    let before = serde_json::to_value(&project).expect("project serializes");
    let version = service.data_version();
    let command = build_action(
        &project,
        SceneModifierAction::Add(layer_id.clone(), "ElasticSculpture".into()),
    )
    .expect("recipe construction succeeds before allocation admission");
    service.execute(
        with_admission_snapshot(
            command,
            Some(manifold_gpu::GpuMemorySnapshot {
                current_allocated_bytes: 0,
                recommended_max_working_set_bytes: 512 * 1024 * 1024,
            }),
        ),
        &mut project,
    );
    let rejection = service
        .take_rejection()
        .expect("oversized stack is diagnosed");
    assert!(
        rejection.contains("projected GPU memory peak"),
        "{rejection}"
    );
    assert!(rejection.contains("current 0"), "{rejection}");
    assert!(
        rejection.contains("available for this candidate"),
        "{rejection}"
    );
    assert_eq!(service.data_version(), version);
    assert!(!service.can_undo());
    assert!(!service.can_redo());
    assert_eq!(serde_json::to_value(&project).unwrap(), before);
}

#[test]
fn modifier_stack_over_256mib_is_admitted_with_device_room() {
    let (mut project, layer_id) = project_with_mushroom();
    let mut service = EditingService::new();
    apply_stock(&mut service, &mut project, &layer_id, "ElasticSculpture");
    let mut service = EditingService::new();
    let before = serde_json::to_value(&project).expect("project serializes");
    let command = build_action(
        &project,
        SceneModifierAction::Add(layer_id.clone(), "ElasticSculpture".into()),
    )
    .expect("recipe construction succeeds before allocation admission");
    service.execute(
        with_admission_snapshot(
            command,
            Some(manifold_gpu::GpuMemorySnapshot {
                current_allocated_bytes: 0,
                recommended_max_working_set_bytes: 4 * 1024 * 1024 * 1024,
            }),
        ),
        &mut project,
    );
    assert!(service.take_rejection().is_none());
    assert!(service.can_undo());
    assert_ne!(serde_json::to_value(&project).unwrap(), before);
}

#[test]
fn invalid_structural_edit_is_rejected_before_project_or_history_changes() {
    let (mut project, layer_id) = project_with_mushroom();
    let mut service = EditingService::new();
    apply_stock(&mut service, &mut project, &layer_id, "ElasticSculpture");
    let mut service = EditingService::new();
    let before = serde_json::to_value(&project).expect("project serializes");
    let version = service.data_version();
    let target = GraphTarget::Generator(layer_id.clone());
    let command = AddGraphNodeCommand::new(
        target,
        "node.not_a_real_primitive".into(),
        None,
        host_graph(&project, &layer_id).clone(),
    );

    service.execute(with_admission(Box::new(command)), &mut project);
    let rejection = service.take_rejection().expect("invalid edit is diagnosed");
    assert!(rejection.contains("rejected") || rejection.contains("unknown"));
    assert_eq!(
        service.data_version(),
        version,
        "rejection does not bump data version"
    );
    assert!(
        !service.can_undo(),
        "rejected edit is absent from undo history"
    );
    assert_eq!(
        serde_json::to_value(&project).expect("project serializes"),
        before
    );
}

#[test]
fn stale_redo_preserves_project_and_keeps_redo_entry() {
    let (mut project, layer_id) = project_with_mushroom();
    let mut service = EditingService::new();
    let first = apply_stock(&mut service, &mut project, &layer_id, "ElasticSculpture");
    assert!(service.undo(&mut project));
    assert!(service.can_redo());

    append_invalid_node(&mut project, &layer_id);
    let before = serde_json::to_value(&project).expect("project serializes");
    assert!(
        !service.redo(&mut project),
        "redo must revalidate the changed source graph"
    );
    assert!(
        service.can_redo(),
        "rejected redo remains available for retry"
    );
    assert!(service.take_rejection().is_some());
    assert_eq!(
        serde_json::to_value(&project).expect("project serializes"),
        before
    );
    assert!(!modifier_ids(&project, &layer_id).contains(&first));
}

#[test]
fn toggle_addresses_the_requested_modifier_instance() {
    let (mut project, layer_id) = project_with_mushroom();
    let mut service = EditingService::new();
    let first = apply_stock(&mut service, &mut project, &layer_id, "SurfacePeel");
    let second = apply_stock(&mut service, &mut project, &layer_id, "SurfacePeel");
    let first_binding = enabled_binding(&project, &layer_id, &first);
    let second_binding = enabled_binding(&project, &layer_id, &second);
    let before_first = project
        .graph_target_owner(&GraphTarget::Generator(layer_id.clone()))
        .and_then(|owner| owner.params.get(&first_binding))
        .expect("first enabled host param")
        .base;
    let before_second = project
        .graph_target_owner(&GraphTarget::Generator(layer_id.clone()))
        .and_then(|owner| owner.params.get(&second_binding))
        .expect("second enabled host param")
        .base;

    let command = build_action(
        &project,
        SceneModifierAction::Toggle(layer_id.clone(), first.clone()),
    )
    .expect("toggle action builds");
    service.execute(with_admission(command), &mut project);
    assert!(service.take_rejection().is_none());
    let owner = project
        .graph_target_owner(&GraphTarget::Generator(layer_id))
        .expect("generator owner");
    assert_ne!(
        owner.params.get(&first_binding).expect("first param").base,
        before_first
    );
    assert_eq!(
        owner
            .params
            .get(&second_binding)
            .expect("second param")
            .base,
        before_second
    );
}

#[test]
fn live_numeric_graph_write_bypasses_structural_admission() {
    let (mut project, layer_id) = project_with_mushroom();
    let mut service = EditingService::new();
    apply_stock(&mut service, &mut project, &layer_id, "ElasticSculpture");
    append_invalid_node(&mut project, &layer_id);

    let target = GraphTarget::Generator(layer_id.clone());
    let node = host_graph(&project, &layer_id)
        .nodes
        .iter()
        .find(|node| node.type_id == "node.orbit_camera")
        .expect("fixture camera node");
    let command = SetGraphNodeParamCommand::new(
        target,
        node.id,
        "live_numeric_probe".into(),
        SerializedParamValue::Float { value: 0.25 },
        host_graph(&project, &layer_id).clone(),
    );
    let version = service.data_version();
    service.execute(with_admission(Box::new(command)), &mut project);
    assert!(
        service.take_rejection().is_none(),
        "live numeric writes have no admission target"
    );
    assert!(service.data_version() > version);
}

#[test]
fn toggle_inverts_the_host_mapping_to_reach_the_local_enabled_value() {
    let (mut project, layer) = project_with_mushroom();
    let mut service = EditingService::new();
    let modifier = apply_stock(&mut service, &mut project, &layer, "ElasticSculpture");
    let id = enabled_binding(&project, &layer, &modifier);
    let target = GraphTarget::Generator(layer.clone());
    let host = project.graph_target_owner_mut(&target).unwrap();
    host.graph
        .as_mut()
        .unwrap()
        .preset_metadata
        .as_mut()
        .unwrap()
        .bindings
        .iter_mut()
        .find(|binding| binding.id == id)
        .unwrap()
        .scale = 0.5;
    let param = host.params.get_mut(&id).unwrap();
    param.spec.max = 2.0;
    param.base = 2.0;
    param.value = 2.0;
    let mut command = build_action(
        &project,
        SceneModifierAction::Toggle(layer.clone(), modifier.clone()),
    )
    .unwrap();
    command.execute(&mut project);
    assert_eq!(
        project
            .graph_target_owner(&target)
            .unwrap()
            .params
            .get(&id)
            .unwrap()
            .base,
        0.0
    );
    let mut command = build_action(&project, SceneModifierAction::Toggle(layer, modifier)).unwrap();
    command.execute(&mut project);
    assert_eq!(
        project
            .graph_target_owner(&target)
            .unwrap()
            .params
            .get(&id)
            .unwrap()
            .base,
        2.0
    );
}

#[test]
fn retarget_keeps_frames_and_undo_restores_all_objects_semantics() {
    use manifold_core::scene_modifier_preset::SceneTargetSelection;
    let (mut project, layer) = project_with_mushroom();
    let mut service = EditingService::new();
    let id = apply_stock(&mut service, &mut project, &layer, "ElasticSculpture");
    let instance = &host_graph(&project, &layer).scene_modifiers[0];
    let before = instance.clone();
    let objects = instance
        .mesh_frames
        .iter()
        .map(|frame| frame.target.clone())
        .collect();
    let command = build_action(
        &project,
        SceneModifierAction::Retarget(
            layer.clone(),
            id,
            SceneTargetSelection::Explicit { objects },
        ),
    )
    .unwrap();
    service.execute(with_admission(command), &mut project);
    assert!(service.take_rejection().is_none());
    assert_eq!(
        host_graph(&project, &layer).scene_modifiers[0].mesh_frames,
        before.mesh_frames
    );
    assert!(matches!(
        host_graph(&project, &layer).scene_modifiers[0].targets,
        SceneTargetSelection::Explicit { .. }
    ));
    assert!(service.undo(&mut project));
    assert_eq!(host_graph(&project, &layer).scene_modifiers[0], before);
}

#[test]
fn preparation_edit_rebuilds_one_local_snapshot_and_round_trips_with_undo() {
    use manifold_core::scene_modifier_preset::{SceneNodeRef, SceneTargetSelection};
    let (mut project, layer) = project_with_mushroom();
    let target = GraphTarget::Generator(layer.clone());
    let graph = host_graph(&project, &layer).clone();
    let scene = graph
        .nodes
        .iter()
        .find(|node| node.type_id == "node.render_scene")
        .unwrap();
    let mut recipe =
        manifold_renderer::node_graph::bundled_preset_def(&PresetTypeId::new("SurfacePeel"))
            .unwrap()
            .clone();
    recipe
        .preset_metadata
        .as_mut()
        .unwrap()
        .scene_modifier
        .as_mut()
        .unwrap()
        .preparation_params
        .push("detail".into());
    let instance =
        manifold_renderer::node_graph::scene_modifier_authoring::prepare_new_scene_modifier(
            &graph,
            &recipe,
            NodeId::new("preparation-test"),
            SceneNodeRef {
                scope: vec![],
                node: scene.node_id.clone(),
            },
            SceneTargetSelection::AllObjects,
        )
        .unwrap();
    let insert = manifold_editing::commands::graph::InsertSceneModifierCommand::new(
        &project, target, &graph, 0, instance,
    )
    .unwrap();
    let mut service = EditingService::new();
    service.execute(with_admission(Box::new(insert)), &mut project);
    assert!(service.take_rejection().is_none());
    let before = serde_json::to_value(&project).unwrap();
    let edit = build_action(
        &project,
        SceneModifierAction::Preparation(
            layer.clone(),
            NodeId::new("preparation-test"),
            "detail".into(),
            2.0,
        ),
    )
    .unwrap();
    service.execute(with_admission(edit), &mut project);
    assert!(service.take_rejection().is_none());
    let after = serde_json::to_value(&project).unwrap();
    assert_ne!(after, before);
    let owner = host_graph(&project, &layer);
    assert!(!owner.preset_metadata.as_ref().unwrap().bindings.iter().any(|binding|
        matches!(&binding.target, BindingTarget::SceneModifier { param_id, .. } if param_id == "detail")));
    assert!(service.undo(&mut project));
    assert_eq!(serde_json::to_value(&project).unwrap(), before);
    assert!(service.redo(&mut project));
    assert_eq!(serde_json::to_value(&project).unwrap(), after);
}
