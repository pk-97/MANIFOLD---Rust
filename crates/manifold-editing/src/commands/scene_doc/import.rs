//! Imports (SCENE_DOCUMENT_DESIGN.md section 7.7 (Imports)): a dropped model
//! is one Add of its whole subtree; a Model File swap is a Set of the Group's
//! Source plus the changes that reconcile its children with the new file.
//! Both are command lists for one `ExecuteBatch`, so each is one undo step.

use manifold_core::scene_doc::{
    ImportedModel, ImportedNode, Item, ItemFields, ItemId, ItemKind, SceneDoc, Shape, import_child_id,
    is_import_child, node_item, param_id,
};
use manifold_core::{GraphTarget, LayerId};

use super::commands::{AddSceneItemCommand, RemoveSceneItemCommand, ReparentSceneItemCommand, SetSceneItemFieldsCommand};
use crate::command::Command;
use crate::commands::effects::ChangeGraphParamCommand;

/// Add the model as `id` under `parent` at `index`, then place it so it
/// fills the view.
pub fn import_model_commands(
    layer: &LayerId,
    parent: Option<ItemId>,
    index: usize,
    id: ItemId,
    file: &str,
    model: &ImportedModel,
) -> Vec<Box<dyn Command>> {
    let mut commands: Vec<Box<dyn Command>> =
        vec![Box::new(AddSceneItemCommand::with_subtree(layer.clone(), parent, index, model.into_item(file, id.clone())))];
    if let Some(placement) = model.placement() {
        let target = GraphTarget::Generator(layer.clone());
        let writes = [
            ("scale_x", 1.0, placement.scale),
            ("scale_y", 1.0, placement.scale),
            ("scale_z", 1.0, placement.scale),
            ("pos_x", 0.0, placement.position[0]),
            ("pos_y", 0.0, placement.position[1]),
            ("pos_z", 0.0, placement.position[2]),
        ];
        for (property, from, to) in writes {
            commands.push(Box::new(ChangeGraphParamCommand::new(target.clone(), param_id(&id, property), from, to)));
        }
    }
    commands
}

/// What a Model File swap does to the scene.
pub struct SwapPlan {
    pub commands: Vec<Box<dyn Command>>,
    /// Objects the new file no longer has, removed with their edits.
    pub removed: usize,
}

struct Wanted<'a> {
    node: &'a ImportedNode,
    id: ItemId,
    parent: ItemId,
}

fn wanted<'a>(group: &ItemId, nodes: &'a [ImportedNode], parent: &ItemId, out: &mut Vec<Wanted<'a>>) {
    for node in nodes {
        let id = import_child_id(group, &node.key);
        out.push(Wanted { node, id: id.clone(), parent: parent.clone() });
        wanted(group, &node.children, &id, out);
    }
}

/// Point import Group `group` at `file`, parsed as `model`. A part whose file
/// node path is still in the file stays, with every edit made to it; a part
/// whose path is gone is removed; a new path is added. Items the user put in
/// the group by hand are left alone.
pub fn swap_model_file_commands(
    layer: &LayerId,
    doc: &SceneDoc,
    group: &ItemId,
    file: &str,
    model: &ImportedModel,
) -> Result<SwapPlan, String> {
    let item = doc.find(group).ok_or_else(|| format!("no item {group} in this scene"))?;
    let ItemFields::Group(fields) = &item.fields else {
        return Err("only an imported group has a Model File".to_owned());
    };
    if fields.source.is_none() {
        return Err("only an imported group has a Model File".to_owned());
    }
    let mut source = fields.clone();
    source.source = Some(file.to_owned());
    let mut commands: Vec<Box<dyn Command>> =
        vec![Box::new(SetSceneItemFieldsCommand::new(layer.clone(), group.clone(), ItemFields::Group(source)))];

    let mut wants = Vec::new();
    wanted(group, model.group_children(), group, &mut wants);
    let kept = |id: &ItemId| wants.iter().any(|want| &want.id == id);

    for want in &wants {
        let Some(existing) = doc.find(&want.id) else {
            let mut added = node_item(group, want.node, file);
            added.children.clear();
            commands.push(Box::new(AddSceneItemCommand::with_subtree(
                layer.clone(),
                Some(want.parent.clone()),
                usize::MAX,
                added,
            )));
            continue;
        };
        if let (ItemFields::Object(old), Some(path)) = (&existing.fields, &want.node.mesh_node) {
            let shape = Shape::Mesh { file: file.to_owned(), node_path: path.clone(), vertex_count: want.node.vertex_count };
            if old.shape != shape {
                let mut fields = old.clone();
                fields.shape = shape;
                commands.push(Box::new(SetSceneItemFieldsCommand::new(
                    layer.clone(),
                    want.id.clone(),
                    ItemFields::Object(fields),
                )));
            }
        }
        if doc.locate(&want.id).and_then(|(parent, _)| parent).as_ref() != Some(&want.parent) {
            commands.push(Box::new(ReparentSceneItemCommand::new(
                layer.clone(),
                want.id.clone(),
                Some(want.parent.clone()),
                usize::MAX,
            )));
        }
    }

    // Parts of the old file that are gone, children before their groups.
    let mut gone: Vec<&Item> = Vec::new();
    doc.walk(|item, ancestors| {
        if ancestors.iter().any(|a| &a.id == group) && is_import_child(group, &item.id) && !kept(&item.id) {
            gone.push(item);
        }
    });
    let removed = gone.iter().filter(|item| item.kind() == ItemKind::Object).count();
    for item in gone.into_iter().rev() {
        commands.push(Box::new(RemoveSceneItemCommand::new(layer.clone(), item.id.clone())));
    }
    Ok(SwapPlan { commands, removed })
}
