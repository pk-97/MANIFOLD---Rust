//! A scene item dragged to a new place in the Scene tab's tree
//! (SCENE_DOCUMENT_DESIGN.md section 8.1): one undo step that reparents or
//! reorders it and, like Blender's Keep Transform, keeps it where it is in
//! the world. Its Position, Rotation and Scale are rewritten in the new
//! parent's frame; a shear a non-uniformly scaled parent gave it cannot be
//! kept by those rows and is dropped, as Blender drops it.

use manifold_core::effects::PresetInstance;
use manifold_core::project::Project;
use manifold_core::scene_doc::{Item, ItemId, SceneDoc, param_id};
use manifold_core::{GraphTarget, LayerId};
use manifold_editing::command::{Command, CompositeCommand};
use manifold_editing::commands::effects::ChangeGraphParamCommand;
use manifold_editing::commands::scene_doc::ReparentSceneItemCommand;
use manifold_node_engine::scene::transform::{IDENTITY_MAT4, Mat4, rotation_to_render_scene_euler, trs_matrix};
use manifold_nodes_scene::node_graph::scene_vm::frame_of;

use crate::scene_edit::SceneEdit;

const TRS: [&str; 9] = ["pos_x", "pos_y", "pos_z", "rot_x", "rot_y", "rot_z", "scale_x", "scale_y", "scale_z"];

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
    let mut commands: Vec<Box<dyn Command>> =
        vec![Box::new(ReparentSceneItemCommand::new(layer.clone(), item.clone(), parent.cloned(), index))];
    if old_parent.as_ref() != parent {
        commands.extend(keep_in_place(layer, doc, inst, item, parent));
    }
    Ok(Some(SceneEdit { command: Box::new(CompositeCommand::new(commands, "Move".to_owned())), select: None }))
}

/// The writes that keep `item` where it is in the world under `parent`.
fn keep_in_place(layer: &LayerId, doc: &SceneDoc, inst: &PresetInstance, item: &ItemId, parent: Option<&ItemId>) -> Vec<Box<dyn Command>> {
    let ids = TRS.map(|property| param_id(item, property));
    if !ids.iter().all(|id| inst.params.contains(id)) {
        return Vec::new();
    }
    let values = |id: &str| inst.params.contains(id).then(|| inst.get_base_param(id));
    let old: [f32; 9] = std::array::from_fn(|k| inst.get_base_param(&ids[k]));
    let old_frame = frame_of(&values, &ancestors(doc, item)).unwrap_or(IDENTITY_MAT4);
    let new_chain = match parent {
        Some(parent) => {
            let mut chain = ancestors(doc, parent);
            chain.extend(doc.find(parent));
            chain
        }
        None => Vec::new(),
    };
    let new_frame = frame_of(&values, &new_chain).unwrap_or(IDENTITY_MAT4);
    let world = mul(old_frame, trs_matrix([old[0], old[1], old[2]], [old[3], old[4], old[5]], [old[6], old[7], old[8]]));
    let Some(new) = affine_inverse(new_frame).map(|inverse| decompose(mul(inverse, world))) else {
        return Vec::new();
    };
    (0..9)
        .filter(|&k| (new[k] - old[k]).abs() > 1e-6)
        .map(|k| Box::new(ChangeGraphParamCommand::new(GraphTarget::Generator(layer.clone()), ids[k].clone(), old[k], new[k])) as Box<dyn Command>)
        .collect()
}

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

/// The inverse of an affine matrix (columns, last row `0 0 0 1`); `None`
/// when it is flat.
fn affine_inverse(m: Mat4) -> Option<Mat4> {
    let a = |row: usize, col: usize| m[col][row];
    let cof = |r0: usize, r1: usize, c0: usize, c1: usize| a(r0, c0) * a(r1, c1) - a(r0, c1) * a(r1, c0);
    let det = a(0, 0) * cof(1, 2, 1, 2) - a(0, 1) * cof(1, 2, 0, 2) + a(0, 2) * cof(1, 2, 0, 1);
    if det.abs() < 1e-12 {
        return None;
    }
    // The 3x3 inverse by cofactors, row-major.
    let inv = [
        [cof(1, 2, 1, 2), -cof(0, 2, 1, 2), cof(0, 1, 1, 2)],
        [-cof(1, 2, 0, 2), cof(0, 2, 0, 2), -cof(0, 1, 0, 2)],
        [cof(1, 2, 0, 1), -cof(0, 2, 0, 1), cof(0, 1, 0, 1)],
    ]
    .map(|row| row.map(|v| v / det));
    let t = [m[3][0], m[3][1], m[3][2]];
    let mut out = IDENTITY_MAT4;
    for col in 0..3 {
        for row in 0..3 {
            out[col][row] = inv[row][col];
        }
    }
    for row in 0..3 {
        out[3][row] = -(0..3).map(|k| inv[row][k] * t[k]).sum::<f32>();
    }
    Some(out)
}

/// Position, rotation and scale (the `TRS` order) of an affine matrix.
fn decompose(m: Mat4) -> [f32; 9] {
    let column = |c: usize| [m[c][0], m[c][1], m[c][2]];
    let length = |v: [f32; 3]| (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    let (x, y, z) = (column(0), column(1), column(2));
    let det = x[0] * (y[1] * z[2] - y[2] * z[1]) - y[0] * (x[1] * z[2] - x[2] * z[1]) + z[0] * (x[1] * y[2] - x[2] * y[1]);
    let mut scale = [length(x), length(y), length(z)];
    if det < 0.0 {
        scale[0] = -scale[0];
    }
    let unit = |v: [f32; 3], s: f32| if s.abs() > 1e-12 { v.map(|c| c / s) } else { v };
    let (x, y, z) = (unit(x, scale[0]), unit(y, scale[1]), unit(z, scale[2]));
    let rot = rotation_to_render_scene_euler([[x[0], y[0], z[0]], [x[1], y[1], z[1]], [x[2], y[2], z[2]]]);
    [m[3][0], m[3][1], m[3][2], rot[0], rot[1], rot[2], scale[0], scale[1], scale[2]]
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_core::PresetTypeId;
    use manifold_core::scene_doc::{BASE_CUBE, GroupFields, ItemFields};
    use manifold_core::types::LayerType;
    use manifold_editing::undo::UndoRedoManager;

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
