//! Group (Cmd+G) and Ungroup (Cmd+Shift+G): composites of the five commands,
//! sent as one `ExecuteBatch` so each is one undo step.

use manifold_core::LayerId;
use manifold_core::scene_doc::{
    ForceTargets, ItemFields, ItemId, ItemKind, ModifierTargets, SceneDoc, can_hold,
};

use super::commands::{
    AddSceneItemCommand, RemoveSceneItemCommand, ReparentSceneItemCommand,
    SetSceneItemFieldsCommand,
};
use crate::command::Command;

/// Add a Group with id `group` where the first selected item sits, then move
/// the selection into it in tree order. A selected item inside another
/// selected item moves with it.
pub fn group_commands(
    layer: &LayerId,
    doc: &SceneDoc,
    selection: &[ItemId],
    group: ItemId,
) -> Vec<Box<dyn Command>> {
    let order = doc.ids();
    // What a Group cannot hold stays where it is: Forces, Modifiers and the
    // World at the top of the scene, Water Sources in their Water.
    let mut chosen: Vec<&ItemId> = order
        .iter()
        .filter(|id| selection.contains(id))
        .filter(|id| {
            doc.find(id)
                .is_some_and(|item| can_hold(Some(ItemKind::Group), item.kind()))
        })
        .collect();
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
        commands.push(Box::new(ReparentSceneItemCommand::new(
            layer.clone(),
            id.clone(),
            Some(group.clone()),
            k,
        )));
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
            Box::new(ReparentSceneItemCommand::new(
                layer.clone(),
                child.id.clone(),
                parent.clone(),
                index + 1 + k,
            )) as Box<dyn Command>
        })
        .collect();
    // A card aimed at the Group keeps acting: it is aimed at the Group's
    // children instead, before the Group goes.
    let children = |kinds: &[ItemKind]| -> Vec<ItemId> {
        item.children
            .iter()
            .filter(|child| kinds.contains(&child.kind()))
            .map(|child| child.id.clone())
            .collect()
    };
    let retarget = |ids: &[ItemId], kinds: &[ItemKind]| -> Option<Vec<ItemId>> {
        ids.contains(group).then(|| {
            let mut aimed: Vec<ItemId> = ids.iter().filter(|id| *id != group).cloned().collect();
            aimed.extend(children(kinds).into_iter().filter(|id| !ids.contains(id)));
            aimed
        })
    };
    for card in &doc.items {
        let fields = match &card.fields {
            ItemFields::Modifier(m) => match &m.targets {
                ModifierTargets::Items(ids) => retarget(ids, &[ItemKind::Object, ItemKind::Group])
                    .map(|ids| {
                        ItemFields::Modifier(manifold_core::scene_doc::ModifierFields {
                            targets: ModifierTargets::Items(ids),
                            ..m.clone()
                        })
                    }),
                ModifierTargets::Scene => None,
            },
            ItemFields::Force(f) => match &f.targets {
                ForceTargets::Items(ids) => {
                    retarget(ids, &[ItemKind::Object, ItemKind::Group, ItemKind::Swarm]).map(
                        |ids| {
                            ItemFields::Force(manifold_core::scene_doc::ForceFields {
                                targets: ForceTargets::Items(ids),
                                ..f.clone()
                            })
                        },
                    )
                }
                _ => None,
            },
            _ => None,
        };
        if let Some(fields) = fields {
            commands.push(Box::new(SetSceneItemFieldsCommand::new(
                layer.clone(),
                card.id.clone(),
                fields,
            )));
        }
    }
    commands.push(Box::new(RemoveSceneItemCommand::new(
        layer.clone(),
        group.clone(),
    )));
    commands
}
