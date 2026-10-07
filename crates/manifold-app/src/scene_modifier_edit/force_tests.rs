//! Force authoring uses the same admitted edit and stable binding contracts.
use super::*;
use manifold_core::scene_modifier_preset::{SceneTargetSelection, is_force_recipe};

#[test]
fn scene_force_add_without_physics_keeps_controls_and_roundtrips_undo() {
    let (mut project, layer) = project_with_mushroom();
    let before = host_graph(&project, &layer).clone();
    let mut editing = EditingService::new();
    let id = apply_stock(&mut editing, &mut project, &layer, "RadialForce");
    let added = host_graph(&project, &layer).clone();
    let instance = added.scene_modifiers.iter().find(|m| m.id == id).unwrap();
    assert!(is_force_recipe(&instance.graph));
    assert!(
        manifold_node_engine::load::expand::force_objects_for_authoring(
            &added,
            &instance.scene,
        )
        .unwrap()
        .is_empty()
    );
    let enabled = enabled_binding(&project, &layer, &id);
    assert!(
        project
            .timeline
            .find_layer_by_id(&layer)
            .unwrap()
            .1
            .gen_params()
            .unwrap()
            .params
            .get(&enabled)
            .is_some()
    );
    let saved = serde_json::to_vec(&added).unwrap();
    let loaded: EffectGraphDef = serde_json::from_slice(&saved).unwrap();
    assert_eq!(loaded, added);
    manifold_renderer::node_graph::scene_modifier_authoring::validate_new_scene_modifier(
        &loaded,
        &loaded.scene_modifiers[0],
    )
    .unwrap();
    assert!(editing.undo(&mut project));
    assert_eq!(host_graph(&project, &layer), &before);
    assert!(editing.redo(&mut project));
    assert_eq!(host_graph(&project, &layer), &added);
}

#[test]
fn scene_force_target_changes_and_duplicate_keep_independent_bindings() {
    let (mut project, layer) = project_with_mushroom();
    let mut editing = EditingService::new();
    let first = apply_stock(&mut editing, &mut project, &layer, "UniformForce");
    let second = apply_stock(&mut editing, &mut project, &layer, "VortexForce");
    let graph = host_graph(&project, &layer);
    let scene = graph.scene_modifiers[0].scene.clone();
    let target = manifold_renderer::node_graph::scene_modifier_authoring::scene_modifier_objects(
        graph, &scene,
    )
    .unwrap()
    .remove(0);
    let command = build_action(
        &project,
        SceneModifierAction::Retarget(
            layer.clone(),
            first.clone(),
            SceneTargetSelection::Explicit {
                objects: vec![target.clone()],
            },
        ),
    )
    .unwrap();
    editing.execute(with_admission(command), &mut project);
    assert!(editing.take_rejection().is_none());
    assert_eq!(
        host_graph(&project, &layer).scene_modifiers[0].targets,
        SceneTargetSelection::Explicit {
            objects: vec![target]
        }
    );
    assert_eq!(
        host_graph(&project, &layer).scene_modifiers[1].targets,
        SceneTargetSelection::AllObjects
    );
    let first_binding = enabled_binding(&project, &layer, &first);
    assert_ne!(first_binding, enabled_binding(&project, &layer, &second));
    let command = build_action(
        &project,
        SceneModifierAction::Duplicate(layer.clone(), vec![first.clone()]),
    )
    .unwrap();
    editing.execute(with_admission(command), &mut project);
    assert!(editing.take_rejection().is_none());
    let duplicate = modifier_ids(&project, &layer)
        .into_iter()
        .find(|id| id != &first && id != &second)
        .unwrap();
    assert_ne!(first_binding, enabled_binding(&project, &layer, &duplicate));
    let command = build_action(
        &project,
        SceneModifierAction::Retarget(
            layer.clone(),
            duplicate.clone(),
            SceneTargetSelection::Explicit {
                objects: Vec::new(),
            },
        ),
    )
    .unwrap();
    editing.execute(with_admission(command), &mut project);
    assert!(
        editing.take_rejection().is_none(),
        "No targets is an editable inactive force"
    );
    assert!(editing.undo(&mut project));
    assert!(editing.redo(&mut project));
    let loaded: EffectGraphDef =
        serde_json::from_slice(&serde_json::to_vec(host_graph(&project, &layer)).unwrap()).unwrap();
    assert!(
        matches!(&loaded.scene_modifiers.iter().find(|m| m.id == duplicate).unwrap().targets,
        SceneTargetSelection::Explicit { objects } if objects.is_empty())
    );
}
