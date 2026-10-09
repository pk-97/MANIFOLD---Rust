use manifold_core::layer::Layer;
use manifold_core::project::Project;
use manifold_core::effect_graph_def::ParamSpecDef;
use manifold_core::effects::PresetInstance;
use manifold_core::params::{ClipTriggerSource, Param, ParamManifest};
use manifold_core::session::{ClipSequence, Scene, SessionSlot};
use manifold_core::types::LayerType;
use manifold_core::{LayerId, PresetTypeId, SceneId};
use manifold_editing::command::Command;
use manifold_editing::commands::layer::{
    AddLayerCommand, DeleteLayerCommand, GroupLayersCommand, ReorderLayerCommand,
    UngroupLayersCommand,
};

#[test]
fn creating_trigger_children_accepts_all_non_trigger_owner_types() {
    for owner_kind in [
        LayerType::Video,
        LayerType::Generator,
        LayerType::Audio,
        LayerType::Dmx,
        LayerType::Group,
    ] {
        let mut project = Project::default();
        let owner = Layer::new("Owner".into(), owner_kind, 0);
        let owner_id = owner.layer_id.clone();
        project.timeline.layers.push(owner);

        let prepared = Layer::new_trigger("Trigger".into(), owner_id.clone(), 1);
        let trigger_id = prepared.layer_id.clone();
        let mut command = AddLayerCommand::from_layer(prepared, 1);
        command.execute(&mut project);

        let trigger = project
            .timeline
            .layers
            .iter()
            .find(|layer| layer.layer_id == trigger_id)
            .expect("accepted trigger child");
        assert_eq!(trigger.parent_layer_id, Some(owner_id.clone()));
        assert!(command.was_applied());
        assert!(command.rejection_reason().is_none());

        command.undo(&mut project);
        assert!(!project.timeline.layers.iter().any(|layer| layer.layer_id == trigger_id));
        command.execute(&mut project);
        assert!(project.timeline.layers.iter().any(|layer| layer.layer_id == trigger_id));
    }
}

#[test]
fn prepared_layer_insertion_preserves_id_parent_and_generator_instance() {
    let mut project = Project::default();
    let group = Layer::new("Group".into(), LayerType::Group, 0);
    let group_id = group.layer_id.clone();
    project.timeline.layers.push(group);

    let mut prepared = Layer::new_generator("Prepared".into(), PresetTypeId::new("prepared"), 1);
    prepared.parent_layer_id = Some(group_id.clone());
    let layer_id = prepared.layer_id.clone();
    let generator = serde_json::to_value(prepared.gen_params()).unwrap();
    let mut command = AddLayerCommand::from_layer(prepared, 1);

    command.execute(&mut project);
    let inserted = project
        .timeline
        .layers
        .iter()
        .find(|layer| layer.layer_id == layer_id)
        .expect("prepared layer inserted");
    assert_eq!(inserted.parent_layer_id, Some(group_id));
    assert_eq!(serde_json::to_value(inserted.gen_params()).unwrap(), generator);
    command.undo(&mut project);
    command.execute(&mut project);
    let redone = project
        .timeline
        .layers
        .iter()
        .find(|layer| layer.layer_id == layer_id)
        .expect("prepared layer redone");
    assert_eq!(serde_json::to_value(redone.gen_params()).unwrap(), generator);
}

#[test]
fn trigger_creation_rejects_root_missing_and_trigger_parents() {
    let mut project = Project::default();
    let mut root = Layer::new_trigger("Root trigger".into(), LayerId::new("unused"), 0);
    root.parent_layer_id = None;
    let mut root_command = AddLayerCommand::from_layer(root, 0);
    root_command.execute(&mut project);
    assert!(!root_command.was_applied());
    assert!(root_command.rejection_reason().is_some());
    root_command.undo(&mut project);
    assert!(project.timeline.layers.is_empty());

    let missing = Layer::new_trigger("Missing parent".into(), LayerId::new("ghost"), 0);
    let mut missing_command = AddLayerCommand::from_layer(missing, 0);
    missing_command.execute(&mut project);
    assert!(!missing_command.was_applied());
    assert!(missing_command.rejection_reason().is_some());
    assert!(project.timeline.layers.is_empty());

    let trigger_parent = Layer::new_trigger("Trigger parent".into(), LayerId::new("owner"), 0);
    let trigger_parent_id = trigger_parent.layer_id.clone();
    project.timeline.layers.push(trigger_parent);
    let child = Layer::new_trigger("Child".into(), trigger_parent_id, 1);
    let mut child_command = AddLayerCommand::from_layer(child, 1);
    child_command.execute(&mut project);
    assert!(!child_command.was_applied());
    assert!(child_command.rejection_reason().is_some());
    assert_eq!(project.timeline.layers.len(), 1);
}

#[test]
fn ordinary_root_and_group_children_remain_accepted() {
    let mut project = Project::default();
    let mut root_command = AddLayerCommand::new(
        "Root".into(),
        LayerType::Video,
        PresetTypeId::NONE,
        0,
        None,
    );
    root_command.execute(&mut project);
    assert!(root_command.was_applied());

    let group = Layer::new("Group".into(), LayerType::Group, 1);
    let group_id = group.layer_id.clone();
    project.timeline.layers.push(group);
    let mut child_command = AddLayerCommand::new(
        "Child".into(),
        LayerType::Audio,
        PresetTypeId::NONE,
        2,
        Some(group_id),
    );
    child_command.execute(&mut project);
    assert!(child_command.was_applied());
    assert!(child_command.rejection_reason().is_none());
}

#[test]
fn duplicate_identity_and_self_parent_creation_leave_the_project_unchanged() {
    let mut project = Project::default();
    let owner = Layer::new_video("Owner".into(), 0);
    project.timeline.layers.push(owner.clone());
    let mut self_parent = Layer::new_trigger("Invalid".into(), owner.layer_id.clone(), 1);
    self_parent.parent_layer_id = Some(self_parent.layer_id.clone());
    let before = serde_json::to_value(&project).unwrap();
    for candidate in [owner, self_parent] {
        let mut command = AddLayerCommand::from_layer(candidate, 1);
        command.execute(&mut project);
        assert!(!command.was_applied());
        assert!(command.rejection_reason().is_some());
        command.undo(&mut project);
        assert_eq!(serde_json::to_value(&project).unwrap(), before);
    }
}

fn add_session_slot(project: &mut Project, layer_id: LayerId, scene_id: &SceneId) {
    project.session.slots.push(SessionSlot {
        layer_id,
        scene_id: scene_id.clone(),
        sequence: ClipSequence::default(),
        name: String::new(),
        color: None,
    });
}

fn slot_layer_ids(project: &Project) -> Vec<LayerId> {
    project
        .session
        .slots
        .iter()
        .map(|slot| slot.layer_id.clone())
        .collect()
}

fn layer_ids(project: &Project) -> Vec<LayerId> {
    project
        .timeline
        .layers
        .iter()
        .map(|layer| layer.layer_id.clone())
        .collect()
}

#[test]
fn deleting_each_non_trigger_owner_removes_direct_trigger_and_slots() {
    for owner_kind in [
        LayerType::Video,
        LayerType::Generator,
        LayerType::Audio,
        LayerType::Dmx,
        LayerType::Group,
    ] {
        let mut project = Project::default();
        let owner = Layer::new("Owner".into(), owner_kind, 0);
        let owner_id = owner.layer_id.clone();
        let trigger = Layer::new_trigger("Trigger".into(), owner_id.clone(), 1);
        let trigger_id = trigger.layer_id.clone();
        let unrelated = Layer::new("Unrelated".into(), LayerType::Video, 2);
        let unrelated_id = unrelated.layer_id.clone();
        let scene_id = SceneId::new("scene");
        project.session.scenes.push(Scene {
            id: scene_id.clone(),
            name: "Scene".into(),
            color: None,
        });
        project.timeline.layers.extend([owner.clone(), trigger, unrelated]);
        add_session_slot(&mut project, owner_id.clone(), &scene_id);
        add_session_slot(&mut project, unrelated_id.clone(), &scene_id);
        add_session_slot(&mut project, trigger_id.clone(), &scene_id);
        let original_layers = layer_ids(&project);
        let original_slots = slot_layer_ids(&project);

        let mut command = DeleteLayerCommand::new(owner);
        command.execute(&mut project);
        assert!(!project.timeline.layers.iter().any(|layer| layer.layer_id == owner_id));
        assert!(!project.timeline.layers.iter().any(|layer| layer.layer_id == trigger_id));
        assert_eq!(slot_layer_ids(&project), vec![unrelated_id.clone()]);

        command.undo(&mut project);
        assert_eq!(layer_ids(&project), original_layers);
        assert_eq!(slot_layer_ids(&project), original_slots);
        assert!(project.timeline.layers.iter().any(|layer| layer.layer_id == owner_id));
        assert!(project.timeline.layers.iter().any(|layer| layer.layer_id == trigger_id));

        command.execute(&mut project);
        assert_eq!(slot_layer_ids(&project), vec![unrelated_id.clone()]);
        command.undo(&mut project);
        assert_eq!(layer_ids(&project), original_layers);
        assert_eq!(slot_layer_ids(&project), original_slots);
    }
}

fn grouped_project() -> (Project, LayerId, LayerId, LayerId, LayerId, LayerId, LayerId, SceneId) {
    let mut project = Project::default();
    let group = Layer::new("Group".into(), LayerType::Group, 0);
    let group_id = group.layer_id.clone();
    let mut content = Layer::new("Content".into(), LayerType::Video, 1);
    content.parent_layer_id = Some(group_id.clone());
    let content_id = content.layer_id.clone();
    let nested_trigger = Layer::new_trigger("Nested trigger".into(), content_id.clone(), 2);
    let nested_trigger_id = nested_trigger.layer_id.clone();
    let direct_trigger = Layer::new_trigger("Direct trigger".into(), group_id.clone(), 3);
    let direct_trigger_id = direct_trigger.layer_id.clone();
    let unrelated = Layer::new("Unrelated".into(), LayerType::Video, 4);
    let unrelated_id = unrelated.layer_id.clone();
    let direct_trigger_b = Layer::new_trigger("Direct trigger B".into(), group_id.clone(), 5);
    let direct_trigger_b_id = direct_trigger_b.layer_id.clone();

    let mut effect = PresetInstance::new(PresetTypeId::BLOOM);
    let mut param = Param::bundled(ParamSpecDef {
        id: "force".into(),
        name: "Force".into(),
        ..ParamSpecDef::default()
    });
    param.clip_trigger_source = ClipTriggerSource::Lane {
        layer_id: direct_trigger_id.clone(),
    };
    effect.params = ParamManifest::from_params(vec![param]);
    content.effects = Some(vec![effect]);

    let scene_id = SceneId::new("scene");
    project.session.scenes.push(Scene {
        id: scene_id.clone(),
        name: "Scene".into(),
        color: None,
    });
    project
        .timeline
        .layers
        .extend([
            group,
            content,
            nested_trigger,
            direct_trigger,
            direct_trigger_b,
            unrelated,
        ]);
    project.timeline.enforce_tree_order();
    for id in [
        group_id.clone(),
        content_id.clone(),
        nested_trigger_id.clone(),
        direct_trigger_id.clone(),
        unrelated_id.clone(),
        direct_trigger_b_id.clone(),
    ] {
        add_session_slot(&mut project, id, &scene_id);
    }
    (
        project,
        group_id,
        direct_trigger_id,
        direct_trigger_b_id,
        content_id,
        nested_trigger_id,
        unrelated_id,
        scene_id,
    )
}

fn content_trigger_source(project: &Project, content_id: &LayerId) -> ClipTriggerSource {
    project
        .timeline
        .layers
        .iter()
        .find(|layer| layer.layer_id == *content_id)
        .and_then(|layer| layer.effects.as_ref())
        .and_then(|effects| effects.first())
        .and_then(|effect| effect.params.get("force"))
        .map(|param| param.clip_trigger_source.clone())
        .expect("content trigger source")
}

#[test]
fn deleting_group_preserves_content_children_and_their_triggers() {
    let (
        mut project,
        group_id,
        direct_trigger_id,
        direct_trigger_b_id,
        content_id,
        nested_trigger_id,
        unrelated_id,
        _,
    ) = grouped_project();
    let original_layers = layer_ids(&project);
    let original_slots = slot_layer_ids(&project);
    let group = project
        .timeline
        .layers
        .iter()
        .find(|layer| layer.layer_id == group_id)
        .cloned()
        .unwrap();

    let mut command = DeleteLayerCommand::new(group);
    command.execute(&mut project);
    assert!(!project.timeline.layers.iter().any(|layer| layer.layer_id == group_id));
    assert!(!project.timeline.layers.iter().any(|layer| layer.layer_id == direct_trigger_id));
    assert!(!project.timeline.layers.iter().any(|layer| layer.layer_id == direct_trigger_b_id));
    let content = project.timeline.layers.iter().find(|layer| layer.layer_id == content_id).unwrap();
    assert!(content.parent_layer_id.is_none());
    assert_eq!(
        content_trigger_source(&project, &content_id),
        ClipTriggerSource::Lane {
            layer_id: direct_trigger_id.clone(),
        }
    );
    assert!(project.timeline.layers.iter().any(|layer| layer.layer_id == nested_trigger_id));
    assert!(project.timeline.layers.iter().any(|layer| layer.layer_id == unrelated_id));
    assert_eq!(
        slot_layer_ids(&project),
        vec![content_id.clone(), nested_trigger_id.clone(), unrelated_id.clone()]
    );

    command.undo(&mut project);
    assert_eq!(layer_ids(&project), original_layers);
    assert_eq!(slot_layer_ids(&project), original_slots);
    assert_eq!(
        project.timeline.layers.iter().find(|layer| layer.layer_id == content_id).unwrap().parent_layer_id,
        Some(group_id.clone())
    );
    assert_eq!(
        content_trigger_source(&project, &content_id),
        ClipTriggerSource::Lane {
            layer_id: direct_trigger_id.clone(),
        }
    );
    command.execute(&mut project);
    assert_eq!(
        slot_layer_ids(&project),
        vec![content_id.clone(), nested_trigger_id, unrelated_id]
    );
    assert_eq!(
        content_trigger_source(&project, &content_id),
        ClipTriggerSource::Lane {
            layer_id: direct_trigger_id,
        }
    );
}

#[test]
fn grouping_owner_with_trigger_keeps_trigger_parent_and_roundtrips() {
    let mut project = Project::default();
    let owner = Layer::new_video("Owner".into(), 0);
    let owner_id = owner.layer_id.clone();
    let trigger = Layer::new_trigger("Lane".into(), owner_id.clone(), 1);
    let trigger_id = trigger.layer_id.clone();
    project.timeline.layers.extend([owner, trigger]);
    let original = project.timeline.layers.clone();
    let before = serde_json::to_value(&project).unwrap();
    let mut command = GroupLayersCommand::new(vec![owner_id.clone(), trigger_id.clone()], original.clone());

    command.execute(&mut project);
    assert!(command.was_applied());
    let group_id = project
        .timeline
        .layers
        .iter()
        .find(|layer| layer.is_group())
        .unwrap()
        .layer_id
        .clone();
    assert_eq!(project.timeline.find_layer_by_id(&owner_id).unwrap().1.parent_layer_id, Some(group_id));
    assert_eq!(project.timeline.find_layer_by_id(&trigger_id).unwrap().1.parent_layer_id, Some(owner_id.clone()));
    let after = serde_json::to_value(&project).unwrap();
    command.undo(&mut project);
    assert_eq!(serde_json::to_value(&project).unwrap(), before);
    command.execute(&mut project);
    assert!(command.was_applied());
    assert_eq!(project.timeline.find_layer_by_id(&trigger_id).unwrap().1.parent_layer_id, Some(owner_id));
    assert_eq!(serde_json::to_value(&project).unwrap(), after);
}

#[test]
fn grouping_selected_ancestors_keeps_the_existing_subtree_intact() {
    let mut project = Project::default();
    let group = Layer::new("Existing group".into(), LayerType::Group, 0);
    let mut owner = Layer::new_video("Owner".into(), 1);
    owner.parent_layer_id = Some(group.layer_id.clone());
    let trigger = Layer::new_trigger("Pattern".into(), owner.layer_id.clone(), 2);
    project.timeline.layers.extend([group, owner, trigger]);
    let ids = layer_ids(&project);
    let before = serde_json::to_value(&project).unwrap();
    let mut command = GroupLayersCommand::new(ids.clone(), project.timeline.layers.clone());
    command.execute(&mut project);
    assert!(command.was_applied());
    assert_eq!(project.timeline.layers[2].parent_layer_id.as_ref(), Some(&ids[0]));
    assert_eq!(project.timeline.layers[3].parent_layer_id.as_ref(), Some(&ids[1]));
    let after = serde_json::to_value(&project).unwrap();
    command.undo(&mut project);
    assert_eq!(serde_json::to_value(&project).unwrap(), before);
    command.execute(&mut project);
    assert_eq!(serde_json::to_value(&project).unwrap(), after);
}

#[test]
fn trigger_only_or_missing_group_selection_is_rejected_without_mutation() {
    let mut project = Project::default();
    let owner = Layer::new_video("Owner".into(), 0);
    let owner_id = owner.layer_id.clone();
    let trigger = Layer::new_trigger("Lane".into(), owner_id.clone(), 1);
    let trigger_id = trigger.layer_id.clone();
    project.timeline.layers.extend([owner, trigger]);
    let original = layer_ids(&project);
    let mut trigger_only = GroupLayersCommand::new(vec![trigger_id], project.timeline.layers.clone());
    trigger_only.execute(&mut project);
    assert!(!trigger_only.was_applied());
    assert!(trigger_only.rejection_reason().is_some());
    trigger_only.undo(&mut project);
    assert_eq!(layer_ids(&project), original);

    let mut missing = GroupLayersCommand::new(vec![LayerId::new("missing")], project.timeline.layers.clone());
    missing.execute(&mut project);
    assert!(!missing.was_applied());
    assert!(missing.rejection_reason().is_some());
    assert_eq!(layer_ids(&project), original);
}

#[test]
fn reorder_preserves_owner_when_candidate_trigger_parent_is_overridden() {
    let mut project = Project::default();
    let owner = Layer::new_video("Owner".into(), 0);
    let owner_id = owner.layer_id.clone();
    let trigger = Layer::new_trigger("Lane".into(), owner_id.clone(), 1);
    let trigger_id = trigger.layer_id.clone();
    let other = Layer::new_video("Other".into(), 2);
    let other_id = other.layer_id.clone();
    project.timeline.layers.extend([owner, trigger, other]);
    let old = project.timeline.layers.clone();
    let new = vec![
        project.timeline.layers[2].clone(),
        project.timeline.layers[0].clone(),
        project.timeline.layers[1].clone(),
    ];
    let old_parents = old.iter().map(|layer| (layer.layer_id.clone(), layer.parent_layer_id.clone())).collect::<std::collections::HashMap<_, _>>();
    let mut new_parents = old_parents.clone();
    new_parents.insert(trigger_id.clone(), Some(other_id));
    let mut command = ReorderLayerCommand::new(old.clone(), new, old_parents, new_parents);
    command.execute(&mut project);
    assert!(command.was_applied());
    assert_eq!(project.timeline.find_layer_by_id(&trigger_id).unwrap().1.parent_layer_id, Some(owner_id));
    command.undo(&mut project);
    assert_eq!(layer_ids(&project), old.iter().map(|layer| layer.layer_id.clone()).collect::<Vec<_>>());
}

#[test]
fn reorder_rejects_invalid_changed_parent_and_membership_without_mutation() {
    let mut project = Project::default();
    let first = Layer::new_video("First".into(), 0);
    let second = Layer::new_video("Second".into(), 1);
    project.timeline.layers.extend([first, second]);
    let old = project.timeline.layers.clone();
    let old_ids = layer_ids(&project);
    let old_parents = old.iter().map(|layer| (layer.layer_id.clone(), layer.parent_layer_id.clone())).collect::<std::collections::HashMap<_, _>>();

    let missing_parent = old.clone();
    let first_id = missing_parent[0].layer_id.clone();
    let mut parents = old_parents.clone();
    parents.insert(first_id, Some(LayerId::new("missing")));
    let mut command = ReorderLayerCommand::new(old.clone(), missing_parent, old_parents.clone(), parents);
    command.execute(&mut project);
    assert!(!command.was_applied());
    assert!(command.rejection_reason().is_some());
    assert_eq!(layer_ids(&project), old_ids);

    project
        .timeline
        .layers
        .push(Layer::new_video("Added after snapshot".into(), 2));
    let current_ids = layer_ids(&project);
    let mut stale = ReorderLayerCommand::new(
        old.clone(),
        old.clone(),
        old_parents.clone(),
        old_parents.clone(),
    );
    stale.execute(&mut project);
    assert!(!stale.was_applied());
    assert!(stale.rejection_reason().is_some());
    assert_eq!(layer_ids(&project), current_ids);

    let duplicate = vec![old[0].clone(), old[0].clone()];
    let mut command = ReorderLayerCommand::new(duplicate.clone(), duplicate, old_parents.clone(), old_parents);
    command.execute(&mut project);
    assert!(!command.was_applied());
    assert!(command.rejection_reason().is_some());
    assert_eq!(layer_ids(&project), current_ids);

    let mut cycle_project = Project::default();
    let group_a = Layer::new("Group A".into(), LayerType::Group, 0);
    let group_b = Layer::new("Group B".into(), LayerType::Group, 1);
    let group_a_id = group_a.layer_id.clone();
    let group_b_id = group_b.layer_id.clone();
    cycle_project.timeline.layers.extend([group_a, group_b]);
    let cycle_old = cycle_project.timeline.layers.clone();
    let cycle_parents = cycle_old
        .iter()
        .map(|layer| (layer.layer_id.clone(), layer.parent_layer_id.clone()))
        .collect::<std::collections::HashMap<_, _>>();
    let mut cycle_new = cycle_old.clone();
    cycle_new[0].parent_layer_id = Some(group_b_id);
    cycle_new[1].parent_layer_id = Some(group_a_id);
    let mut cycle_parent_map = cycle_parents.clone();
    cycle_parent_map.insert(cycle_new[0].layer_id.clone(), cycle_new[0].parent_layer_id.clone());
    cycle_parent_map.insert(cycle_new[1].layer_id.clone(), cycle_new[1].parent_layer_id.clone());
    let mut cycle_command = ReorderLayerCommand::new(
        cycle_old.clone(),
        cycle_new,
        cycle_parents,
        cycle_parent_map,
    );
    cycle_command.execute(&mut cycle_project);
    assert!(!cycle_command.was_applied());
    assert!(cycle_command.rejection_reason().is_some());
    assert_eq!(
        layer_ids(&cycle_project),
        cycle_old
            .iter()
            .map(|layer| layer.layer_id.clone())
            .collect::<Vec<_>>()
    );
}

#[test]
fn deleting_trigger_source_leaves_parameter_reference_inert_and_restores_it() {
    let (
        mut project,
        group_id,
        direct_trigger_id,
        direct_trigger_b_id,
        content_id,
        nested_trigger_id,
        unrelated_id,
        _,
    ) = grouped_project();
    let source = project
        .timeline
        .layers
        .iter()
        .find(|layer| layer.layer_id == direct_trigger_id)
        .cloned()
        .expect("direct trigger source");
    let original_layers = layer_ids(&project);
    let original_slots = slot_layer_ids(&project);
    let mut command = DeleteLayerCommand::new(source);

    command.execute(&mut project);
    assert_eq!(
        layer_ids(&project),
        vec![
            group_id.clone(),
            content_id.clone(),
            nested_trigger_id.clone(),
            direct_trigger_b_id.clone(),
            unrelated_id.clone(),
        ]
    );
    assert_eq!(
        slot_layer_ids(&project),
        vec![
            group_id.clone(),
            content_id.clone(),
            nested_trigger_id.clone(),
            unrelated_id.clone(),
            direct_trigger_b_id.clone(),
        ]
    );
    assert_eq!(
        content_trigger_source(&project, &content_id),
        ClipTriggerSource::Lane {
            layer_id: direct_trigger_id.clone(),
        }
    );

    command.undo(&mut project);
    assert_eq!(layer_ids(&project), original_layers);
    assert_eq!(slot_layer_ids(&project), original_slots);
    assert_eq!(
        content_trigger_source(&project, &content_id),
        ClipTriggerSource::Lane {
            layer_id: direct_trigger_id.clone(),
        }
    );
    command.execute(&mut project);
    assert_eq!(
        content_trigger_source(&project, &content_id),
        ClipTriggerSource::Lane {
            layer_id: direct_trigger_id,
        }
    );
}

#[test]
fn ungroup_reuses_owner_deletion_and_restores_exactly_on_repeat() {
    let (
        mut project,
        group_id,
        direct_trigger_id,
        direct_trigger_b_id,
        content_id,
        nested_trigger_id,
        unrelated_id,
        _,
    ) = grouped_project();
    let original_layers = project.timeline.layers.clone();
    let original_slots = slot_layer_ids(&project);
    let group = original_layers
        .iter()
        .find(|layer| layer.layer_id == group_id)
        .cloned()
        .unwrap();
    let original_order = original_layers.clone();
    let mut command = UngroupLayersCommand::new(
        group,
        vec![
            content_id.clone(),
            direct_trigger_id.clone(),
            direct_trigger_b_id.clone(),
        ],
        original_order,
    );

    command.execute(&mut project);
    assert!(!project.timeline.layers.iter().any(|layer| layer.layer_id == group_id));
    assert!(!project.timeline.layers.iter().any(|layer| layer.layer_id == direct_trigger_id));
    assert!(!project.timeline.layers.iter().any(|layer| layer.layer_id == direct_trigger_b_id));
    assert!(project.timeline.layers.iter().any(|layer| layer.layer_id == nested_trigger_id));
    assert!(project.timeline.layers.iter().any(|layer| layer.layer_id == unrelated_id));
    assert_eq!(
        slot_layer_ids(&project),
        vec![content_id.clone(), nested_trigger_id.clone(), unrelated_id.clone()]
    );
    assert!(project.timeline.layers.iter().find(|layer| layer.layer_id == content_id).unwrap().parent_layer_id.is_none());
    assert_eq!(
        content_trigger_source(&project, &content_id),
        ClipTriggerSource::Lane {
            layer_id: direct_trigger_id.clone(),
        }
    );

    command.undo(&mut project);
    assert_eq!(layer_ids(&project), original_layers.iter().map(|layer| layer.layer_id.clone()).collect::<Vec<_>>());
    assert_eq!(slot_layer_ids(&project), original_slots);
    assert_eq!(
        content_trigger_source(&project, &content_id),
        ClipTriggerSource::Lane {
            layer_id: direct_trigger_id.clone(),
        }
    );
    command.execute(&mut project);
    assert_eq!(
        slot_layer_ids(&project),
        vec![content_id, nested_trigger_id, unrelated_id]
    );
}
