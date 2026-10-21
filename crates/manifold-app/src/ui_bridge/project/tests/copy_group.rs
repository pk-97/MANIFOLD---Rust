//! Make Copies from the object card (copy_group_design.md sections 3 and 5):
//! one action turns an added object into a copy group whose row reads as one
//! object, whose card carries Count, Layout and Spacing as manifest rows, and
//! whose gizmo sits at the group transform and scales as one number. One undo
//! takes it all back.

use super::swarm_add::dispatched;
use super::*;
use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_editing::service::EditingService;
use manifold_nodes_scene::node_graph::scene_vm::{SceneObjectKnownRow, SceneObjectVm, SceneVm, ShapeOwner};
use manifold_nodes_scene::node_graph::viewport_gizmo::{GizmoAxis, GizmoMode, GizmoTargetId, HandleWrite, gizmo_target_for};

fn insert_object(project: &mut Project, layer: &LayerId) -> EditingService {
    let (_, state, mut ui, mut selection, mut active, mut prefs) = dispatch_harness();
    let (tx, rx) = crossbeam_channel::unbounded();
    dispatch_project(
        &crate::ui_bridge::project::insert_template_action(project, layer.clone(), manifold_core::scene_surface::SceneTemplateKind::Object),
        &mut crate::ui_bridge::ProjectRef::new(project, &mut Default::default(), &tx),
        &tx,
        &state,
        &mut ui,
        &mut selection,
        &mut active,
        &mut prefs,
        Default::default(),
    );
    let ContentCommand::ExecuteSelecting(command, _) = rx.try_recv().expect("queued insertion") else { panic!("Add selects what it adds") };
    let mut editing = EditingService::new();
    editing.execute(command, project);
    assert_eq!(editing.take_rejection(), None);
    editing
}

/// The last object row added, by address.
fn added_row(def: &EffectGraphDef) -> SceneObjectKnownRow {
    SceneVm::from_def(def)
        .unwrap()
        .objects
        .into_iter()
        .filter_map(|object| match object {
            SceneObjectVm::Known(row) => Some(*row),
            _ => None,
        })
        .last()
        .expect("an object row")
}

#[test]
fn make_copies_turns_an_added_object_into_a_copy_group_on_the_card_and_gizmo() {
    let (mut project, layer, _) = scene_layer_project();
    let mut editing = insert_object(&mut project, &layer);
    let before = effective_def(&project, &layer);
    let object = added_row(&before);
    assert!(object.copy_group().is_none());

    let ContentCommand::SceneItem(action) = dispatched(&mut project, ProjectAction::SceneSetupMakeCopies(layer.clone(), object.address.clone()))
    else { panic!("Make Copies is a content-owned scene item edit") };
    let command = crate::scene_item_transfer::build_action(&project, action).expect("Make Copies builds");
    editing.execute(command, &mut project);
    assert_eq!(editing.take_rejection(), None);

    let made = effective_def(&project, &layer);
    let vm = SceneVm::from_def(&made).unwrap();
    let row = added_row(&made);
    let body = row.copy_group().expect("the row is a copy group").clone();
    assert!(row.controls.contains(&body), "Count, Layout and Spacing ride the object's card");

    // The card rows are manifest rows on the body, Count first.
    let meta = made.preset_metadata.as_ref().unwrap();
    for name in ["copy_count", "copy_layout", "copy_size_spacing"] {
        assert!(
            meta.bindings.iter().any(|binding| matches!(&binding.target,
                manifold_core::effect_graph_def::BindingTarget::Node { node_id, param } if *node_id == body.node && param == name)),
            "{name} is a card row"
        );
    }

    // The gizmo is the group's transform with one uniform scale handle.
    let id = GizmoTargetId { owner: ShapeOwner::Object(row.address.clone()), shape: 0 };
    let target = gizmo_target_for(&vm, &id).expect("the copy group has a gizmo");
    let scale = target.handle(GizmoMode::Scale, GizmoAxis::Uniform).expect("a uniform scale handle");
    assert!(matches!(scale.write, HandleWrite::Uniform { .. }));
    assert!(target.handle(GizmoMode::Scale, GizmoAxis::X).is_none(), "no per-axis scale reaches the world");

    // One undo takes the group back to the single object.
    assert!(editing.undo(&mut project));
    let undone = effective_def(&project, &layer);
    assert!(added_row(&undone).copy_group().is_none());
    assert_eq!(undone.wires.len(), before.wires.len());
}
