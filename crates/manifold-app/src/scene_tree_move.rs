//! A scene item dragged to a new place in the Scene tab's tree
//! (SCENE_DOCUMENT_DESIGN.md section 8.1): one undo step that reparents or
//! reorders it. The reparent command keeps it where it is in the world, like
//! Blender's Keep Transform.

use manifold_core::LayerId;
use manifold_core::project::Project;
use manifold_core::scene_doc::ItemId;
use manifold_editing::commands::scene_doc::ReparentSceneItemCommand;

use crate::scene_edit::SceneEdit;

/// Move `item` under `parent` (the top level when `None`), before `before`
/// (at the end when `None`). `None` when it lands where it is.
pub(crate) fn tree_move(
    project: &Project,
    layer: &LayerId,
    item: &ItemId,
    parent: Option<&ItemId>,
    before: Option<&ItemId>,
) -> Result<Option<SceneEdit>, String> {
    let inst = project
        .timeline
        .find_layer_by_id(layer.as_str())
        .and_then(|(_, found)| found.gen_params())
        .filter(|inst| inst.scene.is_some())
        .ok_or("That layer has no scene")?;
    let doc = inst.scene.as_ref().expect("a scene layer");
    let (old_parent, old_index) = doc.locate(item).ok_or("That item is no longer in the scene")?;
    // Counted without the item itself, as the move takes it out first.
    let siblings: Vec<&ItemId> = match parent {
        Some(id) => doc.find(id).ok_or("That place is no longer in the scene")?.children.iter(),
        None => doc.items.iter(),
    }
    .map(|sibling| &sibling.id)
    .filter(|sibling| *sibling != item)
    .collect();
    let index = before.and_then(|before| siblings.iter().position(|sibling| *sibling == before)).unwrap_or(siblings.len());
    if old_parent.as_ref() == parent && old_index == index {
        return Ok(None);
    }
    let command = ReparentSceneItemCommand::new(layer.clone(), item.clone(), parent.cloned(), index);
    Ok(Some(SceneEdit { command: Box::new(command), select: None }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_core::PresetTypeId;
    use manifold_core::scene_doc::{BASE_CUBE, GroupFields, Item, ItemFields, SceneDoc, param_id};
    use manifold_core::types::LayerType;
    use manifold_editing::undo::UndoRedoManager;
    use manifold_node_engine::scene::transform::{IDENTITY_MAT4, Mat4, trs_matrix};
    use manifold_nodes_scene::node_graph::scene_vm::frame_of;

    const TRS: [&str; 9] = ["pos_x", "pos_y", "pos_z", "rot_x", "rot_y", "rot_z", "scale_x", "scale_y", "scale_z"];

    /// The items `id` sits in, outermost first.
    fn ancestors<'d>(doc: &'d SceneDoc, id: &ItemId) -> Vec<&'d Item> {
        let mut found = Vec::new();
        doc.walk(|item, chain| {
            if &item.id == id {
                found = chain.to_vec();
            }
        });
        found
    }

    fn mul(a: Mat4, b: Mat4) -> Mat4 {
        std::array::from_fn(|col| std::array::from_fn(|row| (0..4).map(|k| a[k][row] * b[col][k]).sum()))
    }

    /// A scene layer with a Group moved, turned and scaled, beside the cube.
    fn scene_with_group() -> (Project, LayerId) {
        let mut project = Project::default();
        let index = project.timeline.add_layer("Scene", LayerType::Generator, PresetTypeId::SCENE);
        let layer = project.timeline.layers[index].layer_id.clone();
        let inst = project.timeline.layers[index].gen_params_mut().unwrap();
        let mut doc = inst.scene.clone().unwrap();
        doc.items.push(Item::new(ItemId::new("rig"), "Rig", ItemFields::Group(GroupFields::default())));
        inst.install_scene_document(doc).unwrap();
        for (property, value) in [("pos_x", 3.0), ("pos_y", -1.0), ("rot_y", 0.7), ("rot_z", 0.3), ("scale_x", 2.0), ("scale_y", 2.0), ("scale_z", 2.0)] {
            assert!(inst.set_base_param(&param_id(&ItemId::new("rig"), property), value));
        }
        for (property, value) in [("pos_x", 1.0), ("pos_z", 0.5), ("rot_x", 0.2)] {
            assert!(inst.set_base_param(&param_id(&ItemId::new(BASE_CUBE), property), value));
        }
        (project, layer)
    }

    fn world_of(project: &Project, item: &ItemId) -> Mat4 {
        let inst = project.timeline.layers[0].gen_params().unwrap();
        let doc = inst.scene.as_ref().unwrap();
        let values = |id: &str| inst.params.contains(id).then(|| inst.get_base_param(id));
        let v = TRS.map(|property| inst.get_base_param(&param_id(item, property)));
        let frame = frame_of(&values, &ancestors(doc, item)).unwrap_or(IDENTITY_MAT4);
        mul(frame, trs_matrix([v[0], v[1], v[2]], [v[3], v[4], v[5]], [v[6], v[7], v[8]]))
    }

    fn assert_near(a: Mat4, b: Mat4) {
        for col in 0..4 {
            for row in 0..4 {
                assert!((a[col][row] - b[col][row]).abs() < 1e-4, "{a:?} != {b:?}");
            }
        }
    }

    /// The cube dragged into a turned, scaled Group and back out stays where
    /// it is in the world, each move one undo step.
    #[test]
    fn a_dragged_item_keeps_its_place_in_the_world() {
        let (mut project, layer) = scene_with_group();
        let cube = ItemId::new(BASE_CUBE);
        let before = world_of(&project, &cube);
        let mut undo = UndoRedoManager::new();
        let into = tree_move(&project, &layer, &cube, Some(&ItemId::new("rig")), None).unwrap().unwrap();
        assert!(undo.execute(into.command, &mut project));
        let doc = project.timeline.layers[0].gen_params().unwrap().scene.clone().unwrap();
        assert_eq!(doc.locate(&cube).unwrap().0, Some(ItemId::new("rig")));
        assert_near(world_of(&project, &cube), before);

        let out = tree_move(&project, &layer, &cube, None, None).unwrap().unwrap();
        assert!(undo.execute(out.command, &mut project));
        assert_near(world_of(&project, &cube), before);

        assert!(undo.undo(&mut project));
        assert!(undo.undo(&mut project));
        let doc = project.timeline.layers[0].gen_params().unwrap().scene.clone().unwrap();
        assert_eq!(doc.locate(&cube).unwrap().0, None, "two undos put it back");
        assert_near(world_of(&project, &cube), before);
    }

    /// A drop on its own place is no edit; before a sibling reorders.
    #[test]
    fn a_drop_in_place_is_no_edit_and_a_sibling_drop_reorders() {
        let (project, layer) = scene_with_group();
        let cube = ItemId::new(BASE_CUBE);
        let doc = project.timeline.layers[0].gen_params().unwrap().scene.clone().unwrap();
        let (_, at) = doc.locate(&cube).unwrap();
        let next = doc.items.get(at + 1).map(|item| item.id.clone());
        assert!(tree_move(&project, &layer, &cube, None, next.as_ref()).unwrap().is_none());
        let first = doc.items[0].id.clone();
        assert!(tree_move(&project, &layer, &cube, None, Some(&first)).unwrap().is_some());
    }
}
