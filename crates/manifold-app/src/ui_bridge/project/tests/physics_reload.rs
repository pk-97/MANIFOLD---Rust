//! PHYSICS_ENGINE_BOUNDARY_DESIGN.md P6a gate: a saved rigid body reloads
//! with its switch state and its card controls, which still take writes.
//! `enabled` itself is a Hidden switch (D6), so it has no card to modulate.

use super::*;
use manifold_nodes_scene::node_graph::scene_vm::{PhysicsVm, SceneObjectVm, SceneVm};

fn body(project: &Project, layer_id: &LayerId) -> PhysicsVm {
    SceneVm::from_def(&effective_def(project, layer_id))
        .unwrap()
        .objects
        .into_iter()
        .find_map(|object| match object {
            SceneObjectVm::Known(row) if row.index == 0 => row.physics,
            _ => None,
        })
        .expect("the Ground has a body")
}

fn switch(project: &mut Project, layer_id: &LayerId, scene: u32, enabled: bool) {
    let action = crate::scene_item_transfer::SceneItemAction::SetPhysics { layer: layer_id.clone(), scene, index: 0, enabled };
    let mut command = crate::scene_item_transfer::build_action(project, action).expect("the switch builds");
    command.execute(project);
    assert!(command.was_applied(), "{:?}", command.rejection_reason());
}

#[test]
fn physics_boundary_rigid_reload_keeps_the_switch_and_card_controls() {
    let (mut project, layer_id, scene) = physics_solids_layer_project();
    switch(&mut project, &layer_id, scene, false);
    let off = body(&project, &layer_id);
    assert!(!off.enabled);

    let saved = serde_json::to_string(&project).unwrap();
    let mut reloaded = manifold_io::loader::load_project_from_json(&saved).expect("real project load pipeline");
    let def = effective_def(&reloaded, &layer_id);
    assert_eq!(body(&reloaded, &layer_id), off, "the body and its off switch survive a reload");
    assert!(
        super::super::scene_write::scene_binding_id(&def, &off.body_scope_path, off.body_node_id, "enabled").is_none(),
        "the switch is Hidden, so no card or modulation reaches it"
    );

    // Friction is a card after reload, so its writes land on the card slot
    // that drivers and automation modulate.
    let card = super::super::scene_write::scene_binding_id(&def, &off.body_scope_path, off.body_node_id, "friction")
        .expect("Friction is a card");
    let mut write = apply_scene_param_write(&reloaded, &layer_id, off.body_scope_path.clone(), off.body_node_id, "friction", 0.73)
        .expect("a card write");
    write.execute(&mut reloaded);
    let host = reloaded.timeline.find_layer_by_id(&layer_id).unwrap().1.gen_params().unwrap();
    assert_eq!(host.get_base_param(&card), 0.73);

    switch(&mut reloaded, &layer_id, scene, true);
    assert!(body(&reloaded, &layer_id).enabled, "the reloaded switch turns back on");
}

/// A stale second click changes nothing, so it is refused before it reaches
/// the undo stack.
#[test]
fn physics_switch_refuses_a_click_that_changes_nothing() {
    let (mut project, layer_id, scene) = physics_solids_layer_project();
    let again = |project: &Project, enabled| {
        let action = crate::scene_item_transfer::SceneItemAction::SetPhysics { layer: layer_id.clone(), scene, index: 0, enabled };
        crate::scene_item_transfer::build_action(project, action).map(|_| ())
    };
    assert_eq!(again(&project, true), Err("Physics is already on".to_string()));
    switch(&mut project, &layer_id, scene, false);
    assert_eq!(again(&project, false), Err("Physics is already off".to_string()));
}
