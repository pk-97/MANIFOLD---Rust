//! Group (Cmd+G) and Ungroup (Cmd+Shift+G): composites of the five commands,
//! sent as one `ExecuteBatch` so each is one undo step.

use manifold_core::LayerId;
use manifold_core::scene_doc::{ItemId, ItemKind, SceneDoc};

use super::commands::{AddSceneItemCommand, RemoveSceneItemCommand, ReparentSceneItemCommand};
use crate::command::Command;

/// Add a Group with id `group` where the first selected item sits, then move
/// the selection into it in tree order. A selected item inside another
/// selected item moves with it.
pub fn group_commands(layer: &LayerId, doc: &SceneDoc, selection: &[ItemId], group: ItemId) -> Vec<Box<dyn Command>> {
    let order = doc.ids();
    let mut chosen: Vec<&ItemId> = order.iter().filter(|id| selection.contains(id)).collect();
    chosen.retain(|id| {
        let mut inside_another = false;
        doc.walk(|item, ancestors| {
            if &&item.id == id {
                inside_another = ancestors.iter().any(|a| selection.contains(&a.id));
            }
        });
        !inside_another
    });
    let Some((parent, index)) = chosen.first().and_then(|first| doc.locate(first)) else {
        return Vec::new();
    };
    let mut commands: Vec<Box<dyn Command>> = vec![Box::new(AddSceneItemCommand::new(
        layer.clone(),
        ItemKind::Group,
        parent,
        index,
        None,
        group.clone(),
    ))];
    for (k, id) in chosen.into_iter().enumerate() {
        commands.push(Box::new(ReparentSceneItemCommand::new(layer.clone(), id.clone(), Some(group.clone()), k)));
    }
    commands
}

/// Move a group's children to where the group sits, in order, then remove
/// the group.
pub fn ungroup_commands(layer: &LayerId, doc: &SceneDoc, group: &ItemId) -> Vec<Box<dyn Command>> {
    let (Some(item), Some((parent, index))) = (doc.find(group), doc.locate(group)) else {
        return Vec::new();
    };
    if item.kind() != ItemKind::Group {
        return Vec::new();
    }
    let mut commands: Vec<Box<dyn Command>> = item
        .children
        .iter()
        .enumerate()
        .map(|(k, child)| {
            Box::new(ReparentSceneItemCommand::new(layer.clone(), child.id.clone(), parent.clone(), index + 1 + k))
                as Box<dyn Command>
        })
        .collect();
    commands.push(Box::new(RemoveSceneItemCommand::new(layer.clone(), group.clone())));
    commands
}
