//! Imports (SCENE_DOCUMENT_DESIGN.md section 7.7 (Imports)): a dropped model
//! is one Add of its whole subtree (or, for one mesh, of its Object and that
//! Object's Material) plus the writes that place it and start the file's
//! lights and cameras where the file puts them; a Model File swap is a Set of
//! the import's file plus the changes that reconcile it with the new file.
//! Both are command lists for one `ExecuteBatch`, so each is one undo step.

use manifold_core::scene_doc::{
    ImportedModel, ImportedNode, Item, ItemFields, ItemId, ItemKind, MeshPrimitives, SceneDoc, Shape, TextureSource,
    camera_item, import_child_id, is_import_child, light_item, material_item, node_item, param_id,
};
use manifold_core::{GraphTarget, LayerId};

use super::commands::{AddSceneItemCommand, RemoveSceneItemCommand, ReparentSceneItemCommand, SetSceneItemFieldsCommand};
use crate::command::Command;
use crate::commands::effects::ChangeGraphParamCommand;

/// Add the model as `id` under `parent` at `index`, place it so it fills the
/// view, and start its lights and cameras at the file's values.
pub fn import_model_commands(
    layer: &LayerId,
    parent: Option<ItemId>,
    index: usize,
    id: ItemId,
    file: &str,
    model: &ImportedModel,
) -> Vec<Box<dyn Command>> {
    let mut commands: Vec<Box<dyn Command>> = Vec::new();
    for (offset, item) in model.into_items(file, id.clone()).into_iter().enumerate() {
        let at = index.saturating_add(offset);
        commands.push(Box::new(AddSceneItemCommand::with_subtree(layer.clone(), parent.clone(), at, item)));
    }
    let target = GraphTarget::Generator(layer.clone());
    if let Some(placement) = model.placement() {
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
    starting_writes(&target, model.starting_values(&id), &mut commands);
    commands
}

/// The writes that start new lights and cameras at the file's values. They
/// are added in the same batch, so undo removes them with their values.
fn starting_writes(
    target: &GraphTarget,
    values: impl IntoIterator<Item = (ItemId, &'static str, f32)>,
    commands: &mut Vec<Box<dyn Command>>,
) {
    for (item, property, value) in values {
        commands.push(Box::new(ChangeGraphParamCommand::new(target.clone(), param_id(&item, property), value, value)));
    }
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

/// Point the import `item` at `file`, parsed as `model`. An import Group
/// reconciles its children with the new file; a single imported Object
/// keeps itself and its edits and draws the new file's mesh.
pub fn swap_model_file_commands(
    layer: &LayerId,
    doc: &SceneDoc,
    item: &ItemId,
    file: &str,
    model: &ImportedModel,
) -> Result<SwapPlan, String> {
    let found = doc.find(item).ok_or_else(|| format!("no item {item} in this scene"))?;
    match &found.fields {
        ItemFields::Group(fields) if fields.source.is_some() => swap_group(layer, doc, found, file, model),
        ItemFields::Object(fields) if matches!(fields.shape, Shape::Mesh { .. }) => {
            Ok(swap_object(layer, doc, found, file, model))
        }
        _ => Err("only an imported model has a Model File".to_owned()),
    }
}

/// A single-mesh import: a Set of its Shape to the new file's only mesh
/// part (or, when the new file has several, all of it as one mesh), and its
/// import Material pointed at that part's material.
fn swap_object(layer: &LayerId, doc: &SceneDoc, object: &Item, file: &str, model: &ImportedModel) -> SwapPlan {
    let ItemFields::Object(old) = &object.fields else {
        unreachable!("swap_model_file_commands checked the kind");
    };
    let only = model.only_part();
    let shape = match only {
        Some(part) => Shape::Mesh {
            file: file.to_owned(),
            node_path: part.mesh_node.clone().unwrap_or_default(),
            vertex_count: part.vertex_count,
            primitives: part.primitives,
        },
        None => Shape::Mesh {
            file: file.to_owned(),
            node_path: String::new(),
            vertex_count: model.vertex_count(),
            primitives: MeshPrimitives::All,
        },
    };
    let mut fields = old.clone();
    fields.shape = shape;
    let mut commands: Vec<Box<dyn Command>> =
        vec![Box::new(SetSceneItemFieldsCommand::new(layer.clone(), object.id.clone(), ItemFields::Object(fields)))];
    // Its own import Material follows the file; one the user picked stays.
    let material = old.material.as_ref().and_then(|id| doc.find(id));
    if let Some(material) = material
        && let ItemFields::Material(fields) = &material.fields
        && matches!(fields.texture, TextureSource::Model { .. })
    {
        let index = only
            .and_then(|part| part.material.as_ref())
            .and_then(|key| model.materials.iter().find(|m| &m.key == key))
            .and_then(|m| m.material);
        let mut fields = fields.clone();
        fields.texture = TextureSource::Model { file: file.to_owned(), material: index };
        commands.push(Box::new(SetSceneItemFieldsCommand::new(
            layer.clone(),
            material.id.clone(),
            ItemFields::Material(fields),
        )));
    }
    SwapPlan { commands, removed: 0 }
}

/// An import Group: a part whose file node path (and material) is still in
/// the file stays, with every edit made to it; a part whose path is gone is
/// removed; a new path is added. Materials, lights and cameras reconcile the
/// same way by their keys. Items the user put in the group by hand are left
/// alone, and a part keeps a material the user bound it to.
fn swap_group(layer: &LayerId, doc: &SceneDoc, item: &Item, file: &str, model: &ImportedModel) -> Result<SwapPlan, String> {
    let group = &item.id;
    let ItemFields::Group(fields) = &item.fields else {
        return Err("only an imported model has a Model File".to_owned());
    };
    let mut source = fields.clone();
    source.source = Some(file.to_owned());
    // The new file's clips; the pick stays when the file still has it.
    source.clip_names.clone_from(&model.clips);
    if source.clip as usize >= model.clips.len() {
        source.clip = 0;
    }
    let mut commands: Vec<Box<dyn Command>> =
        vec![Box::new(SetSceneItemFieldsCommand::new(layer.clone(), group.clone(), ItemFields::Group(source)))];

    let mut wants = Vec::new();
    wanted(group, model.group_children(), group, &mut wants);
    let mut kept: Vec<ItemId> = wants.iter().map(|want| want.id.clone()).collect();

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
            let mut fields = old.clone();
            fields.shape = Shape::Mesh {
                file: file.to_owned(),
                node_path: path.clone(),
                vertex_count: want.node.vertex_count,
                primitives: want.node.primitives,
            };
            let picked_by_hand = old.material.as_ref().is_some_and(|id| !is_import_child(group, id));
            if !picked_by_hand {
                fields.material = want.node.material.as_ref().map(|key| import_child_id(group, key));
            }
            if &fields != old {
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

    let add = |item: Item, commands: &mut Vec<Box<dyn Command>>| {
        commands.push(Box::new(AddSceneItemCommand::with_subtree(layer.clone(), Some(group.clone()), usize::MAX, item)));
    };
    for material in &model.materials {
        let id = import_child_id(group, &material.key);
        kept.push(id.clone());
        match doc.find(&id).map(|existing| &existing.fields) {
            None => add(material_item(group, material, file), &mut commands),
            Some(ItemFields::Material(old)) if matches!(old.texture, TextureSource::Model { .. }) => {
                let mut fields = old.clone();
                fields.texture = TextureSource::Model { file: file.to_owned(), material: material.material };
                if &fields != old {
                    commands.push(Box::new(SetSceneItemFieldsCommand::new(layer.clone(), id, ItemFields::Material(fields))));
                }
            }
            Some(_) => {}
        }
    }
    let target = GraphTarget::Generator(layer.clone());
    let mut starts = Vec::new();
    for light in &model.lights {
        let id = import_child_id(group, &light.key);
        kept.push(id.clone());
        if doc.find(&id).is_none() {
            add(light_item(group, light), &mut commands);
            starts.extend(light.values.iter().map(|(property, value)| (id.clone(), *property, *value)));
        }
    }
    for camera in &model.cameras {
        let id = import_child_id(group, &camera.key);
        kept.push(id.clone());
        if doc.find(&id).is_none() {
            add(camera_item(group, camera), &mut commands);
            starts.extend(camera.values.iter().map(|(property, value)| (id.clone(), *property, *value)));
        }
    }
    starting_writes(&target, starts, &mut commands);

    // What of the old file is gone, children before their groups.
    let mut gone: Vec<&Item> = Vec::new();
    doc.walk(|item, ancestors| {
        if ancestors.iter().any(|a| &a.id == group) && is_import_child(group, &item.id) && !kept.contains(&item.id) {
            gone.push(item);
        }
    });
    let removed = gone.iter().filter(|item| item.kind() == ItemKind::Object).count();
    for item in gone.into_iter().rev() {
        commands.push(Box::new(RemoveSceneItemCommand::new(layer.clone(), item.id.clone())));
    }
    Ok(SwapPlan { commands, removed })
}
