//! An Object's mesh modifier list (SCENE_DOCUMENT_DESIGN.md section 6,
//! difference 9): adding, removing and reordering are each one
//! `SetSceneItemFieldsCommand`, so each is one undo step. The list is the
//! structural field; the modifiers' dials are ordinary params addressed by
//! `<object id>.<key>.<param>`, so a reorder never moves a mapping.

use manifold_core::LayerId;
use manifold_core::scene_doc::{ItemFields, ItemId, MeshModifierKind, ObjectFields, SceneDoc};
use manifold_core::scene_schema::modifiers_unavailable;

use super::SetSceneItemFieldsCommand;

fn object_fields<'d>(doc: &'d SceneDoc, object: &ItemId) -> Result<&'d ObjectFields, String> {
    match doc.find(object).map(|item| &item.fields) {
        Some(ItemFields::Object(fields)) => Ok(fields),
        Some(_) => Err("Only an Object takes modifiers".into()),
        None => Err(format!("No item {object}")),
    }
}

fn set(layer: &LayerId, object: &ItemId, fields: ObjectFields) -> SetSceneItemFieldsCommand {
    SetSceneItemFieldsCommand::new(layer.clone(), object.clone(), ItemFields::Object(fields))
}

/// Add a modifier of `kind` at the end of `object`'s list, under a fresh key.
pub fn add_modifier(
    layer: &LayerId,
    doc: &SceneDoc,
    object: &ItemId,
    kind: MeshModifierKind,
) -> Result<SetSceneItemFieldsCommand, String> {
    let fields = object_fields(doc, object)?;
    if let Some(reason) = modifiers_unavailable(&fields.shape) {
        return Err(reason.into());
    }
    Ok(set(layer, object, fields.with_modifier(manifold_core::math::short_id(), kind)))
}

/// Remove the modifier `key` from `object`'s list. Its params leave the
/// scene with it.
pub fn remove_modifier(
    layer: &LayerId,
    doc: &SceneDoc,
    object: &ItemId,
    key: &str,
) -> Result<SetSceneItemFieldsCommand, String> {
    Ok(set(layer, object, object_fields(doc, object)?.without_modifier(key)))
}

/// Move the modifier `key` to `index` in `object`'s list.
pub fn move_modifier(
    layer: &LayerId,
    doc: &SceneDoc,
    object: &ItemId,
    key: &str,
    index: usize,
) -> Result<SetSceneItemFieldsCommand, String> {
    Ok(set(layer, object, object_fields(doc, object)?.with_modifier_moved(key, index)))
}
