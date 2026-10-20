//! The Add menu's Swarm goes in as one template: its object row owns the
//! swarm's forces node and box, the box is a gizmo shape, and the whole
//! insertion is one undo step.

use super::*;
use manifold_editing::service::EditingService;
use manifold_nodes_scene::node_graph::scene_vm::{ShapeOwner, SceneObjectVm, SceneVm};

#[test]
fn add_swarm_inserts_one_row_with_its_forces_and_box_and_undoes_in_one_step() {
    let (mut project, layer, _) = scene_layer_project();
    let before = effective_def(&project, &layer);
    let objects_before = SceneVm::from_def(&before).unwrap().objects.len();

    let (_, state, mut ui, mut selection, mut active, mut prefs) = dispatch_harness();
    let (tx, rx) = crossbeam_channel::unbounded();
    dispatch_project(
        &crate::ui_bridge::project::insert_template_action(&project, layer.clone(), manifold_core::scene_surface::SceneTemplateKind::Swarm),
        &mut crate::ui_bridge::ProjectRef::new(&mut project, &mut Default::default(), &tx),
        &tx,
        &state,
        &mut ui,
        &mut selection,
        &mut active,
        &mut prefs,
        Default::default(),
    );
    let ContentCommand::ExecuteSelecting(command, _) = rx.try_recv().expect("queued swarm insertion")
    else { panic!("Add Swarm goes through content-owned editing and selection"); };
    let mut editing = EditingService::new();
    editing.execute(command, &mut project);
    assert_eq!(editing.take_rejection(), None);

    let added = effective_def(&project, &layer);
    let vm = SceneVm::from_def(&added).unwrap();
    assert_eq!(vm.objects.len(), objects_before + 1, "one new object row");
    let forces = vm
        .objects
        .iter()
        .find_map(|object| match object {
            SceneObjectVm::Known(row) => row.swarm.as_ref().map(|swarm| (row.object_node_id, swarm.node.clone())),
            _ => None,
        })
        .expect("the new row owns a swarm");
    let node = added
        .nodes
        .iter()
        .chain(added.nodes.iter().filter_map(|node| node.group.as_deref()).flat_map(|group| group.nodes.iter()))
        .find(|node| node.node_id == forces.1)
        .expect("the swarm's forces node is in the graph");
    assert_eq!(node.type_id, "node.swarm_forces");
    assert!(
        vm.shapes.iter().any(|shape| shape.owner == ShapeOwner::Swarm(forces.1.clone())),
        "the swarm's box is a gizmo shape from the moment it is inserted"
    );

    assert!(editing.undo(&mut project));
    let undone = SceneVm::from_def(&effective_def(&project, &layer)).unwrap();
    assert_eq!(undone.objects.len(), objects_before, "one undo removes the swarm");
    assert!(
        undone.objects.iter().all(|object| !matches!(object, SceneObjectVm::Known(row) if row.swarm.is_some())),
        "no swarm row is left"
    );
}
