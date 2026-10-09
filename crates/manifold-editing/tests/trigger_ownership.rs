use manifold_core::layer::Layer;
use manifold_core::project::Project;
use manifold_core::effect_graph_def::ParamSpecDef;
use manifold_core::effects::PresetInstance;
use manifold_core::params::{ClipTriggerSource, Param, ParamManifest};
use manifold_core::session::{ClipSequence, Scene, SessionSlot};
use manifold_core::types::LayerType;
use manifold_core::{LayerId, PresetTypeId, SceneId};
use manifold_editing::command::Command;
use manifold_editing::commands::layer::{DeleteLayerCommand, UngroupLayersCommand};

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
