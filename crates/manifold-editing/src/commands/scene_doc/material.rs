//! Materials an object owns (SCENE_DOCUMENT_DESIGN.md section 7.6 (Materials
//! and layer planes)): a new Object comes with its own Material, as
//! Blender's default cube does, and Make Unique gives an item a Material for
//! it alone. Both are command lists for one `ExecuteBatch`, so one undo step.

use manifold_core::LayerId;
use manifold_core::scene_doc::{Item, ItemFields, ItemId, ItemKind, MaterialFields, SceneDoc};

use super::commands::{AddSceneItemCommand, RenameSceneItemCommand, SetSceneItemFieldsCommand};
use crate::command::Command;

/// Add `object` under `parent` at `index` with `material` right after it,
/// the object bound to the material.
pub fn add_object_commands(
    layer: &LayerId,
    parent: Option<ItemId>,
    index: usize,
    mut object: Item,
    material: Item,
) -> Vec<Box<dyn Command>> {
    if let ItemFields::Object(fields) = &mut object.fields {
        fields.material = Some(material.id.clone());
    }
    vec![
        Box::new(AddSceneItemCommand::with_subtree(layer.clone(), parent.clone(), index, material)),
        Box::new(AddSceneItemCommand::with_subtree(layer.clone(), parent, index, object)),
    ]
}

/// A plain Material item named `name`.
pub fn new_material(id: ItemId, name: impl Into<String>) -> Item {
    Item::new(id, name, ItemFields::Material(MaterialFields::default()))
}

/// Give `owner` a Material of its own, `material` named `name`: a copy (with
/// its values) of the one it draws with, or a plain one when it draws with
/// none. The new Material sits right after the one it copies, or right
/// after the owner.
pub fn own_material_commands(
    layer: &LayerId,
    doc: &SceneDoc,
    owner: &ItemId,
    material: ItemId,
    name: &str,
) -> Result<Vec<Box<dyn Command>>, String> {
    let found = doc.find(owner).ok_or("That item is no longer in the scene")?;
    let mut fields = found.fields.clone();
    match &mut fields {
        ItemFields::Object(object) => object.material = Some(material.clone()),
        ItemFields::Group(group) => group.material = Some(material.clone()),
        ItemFields::Water(water) => water.material = Some(material.clone()),
        _ => return Err("Only an object, a group or a water draws with a material".to_owned()),
    }
    let add: Box<dyn Command> = match doc.effective_material(owner) {
        Some(source) => {
            let (parent, index) = doc.locate(&source).ok_or("That material is no longer in the scene")?;
            Box::new(AddSceneItemCommand::new(layer.clone(), ItemKind::Material, parent, index + 1, Some(source), material.clone()))
        }
        None => {
            let (parent, index) = doc.locate(owner).ok_or("That item is no longer in the scene")?;
            Box::new(AddSceneItemCommand::with_subtree(layer.clone(), parent, index + 1, new_material(material.clone(), name)))
        }
    };
    Ok(vec![
        add,
        Box::new(RenameSceneItemCommand::new(layer.clone(), material, name)),
        Box::new(SetSceneItemFieldsCommand::new(layer.clone(), owner.clone(), fields)),
    ])
}
