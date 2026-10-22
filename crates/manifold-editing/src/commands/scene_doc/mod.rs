//! The scene document commands (SCENE_DOCUMENT_DESIGN.md section 7.2
//! (Identity and the five commands)): Add (with an optional source, which is
//! Duplicate), Remove, Reparent, Rename and Set, plus the Group and Ungroup
//! composites. Each is one undo step. Each edits the layer's document, then
//! installs the graph compiled from it (`PresetInstance::install_scene_document`)
//! on the content thread. Param writes are not here: they are the ordinary param
//! commands, addressed by `<item id>.<property>`, and never recompile.

mod commands;
mod group;
mod import;
mod material;
mod modifiers;

pub use commands::{
    AddSceneItemCommand, RemoveSceneItemCommand, RenameSceneItemCommand, ReparentSceneItemCommand,
    SetSceneItemFieldsCommand,
};
pub use group::{group_commands, ungroup_commands};
pub use import::{SwapPlan, import_model_commands, swap_model_file_commands};
pub use material::{add_object_commands, new_material, own_material_commands};
pub use modifiers::{add_modifier, move_modifier, new_modifier};

#[cfg(test)]
mod tests;

use manifold_core::effects::PresetInstance;
use manifold_core::project::Project;
use manifold_core::scene_doc::{ItemId, SceneDoc, SceneEditError};
use manifold_core::LayerId;

use crate::commands::graph::{InstanceLayerSnapshot, prune_instance_params};

/// What a document edit replaced: the document and every param, value and
/// modulator on the instance, so undo puts back exactly what was there.
#[derive(Debug)]
pub(crate) struct Before {
    doc: SceneDoc,
    instance: InstanceLayerSnapshot,
}

/// The scene layer's generator instance.
fn scene_instance<'p>(project: &'p mut Project, layer: &LayerId) -> Result<&'p mut PresetInstance, String> {
    let (_, layer_ref) = project
        .timeline
        .find_layer_by_id_mut(layer.as_str())
        .ok_or_else(|| format!("no layer {layer}"))?;
    let instance = layer_ref.gen_params_mut().ok_or_else(|| format!("layer {layer} has no generator"))?;
    if instance.scene.is_none() {
        return Err(format!("layer {layer} is not a scene layer"));
    }
    Ok(instance)
}

/// Edit the layer's document with `edit`, install the compiled graph, and
/// drop the params (and their modulators) of anything that left the scene.
/// Refused edits leave the project untouched.
pub(crate) fn apply(
    project: &mut Project,
    layer: &LayerId,
    edit: impl FnOnce(&mut SceneDoc) -> Result<(), SceneEditError>,
) -> Result<Before, String> {
    let instance = scene_instance(project, layer)?;
    let doc = instance.scene.clone().expect("scene layer");
    let before = Before { doc: doc.clone(), instance: InstanceLayerSnapshot::capture(instance) };
    let shown = shown_camera(&doc, instance);
    let mut edited = doc;
    edit(&mut edited).map_err(|error| error.to_string())?;
    let old_ids: Vec<String> = instance.params.iter().map(|p| p.id().to_owned()).collect();
    instance.install_scene_document(edited)?;
    let gone: Vec<String> = old_ids.into_iter().filter(|id| instance.params.get(id).is_none()).collect();
    prune_instance_params(instance, &gone);
    keep_showing(instance, shown);
    Ok(before)
}

/// The camera the World's Camera row shows.
fn shown_camera(doc: &SceneDoc, instance: &PresetInstance) -> Option<ItemId> {
    let id = doc.camera_pick_param()?;
    let pick = instance.params.contains(&id).then(|| instance.get_base_param(&id))?;
    doc.picked_camera(pick).cloned()
}

/// The Camera row picks by position among the cameras, so an edit that adds,
/// removes or moves one would cut to another camera. Point the row back at
/// the camera it showed, or at the first camera when that one is gone, in
/// the same undo step.
fn keep_showing(instance: &mut PresetInstance, shown: Option<ItemId>) {
    let Some(doc) = instance.scene.as_ref() else {
        return;
    };
    let Some(id) = doc.camera_pick_param().filter(|id| instance.params.contains(id)) else {
        return;
    };
    let index = shown.and_then(|camera| doc.cameras().iter().position(|&c| *c == camera)).unwrap_or(0) as f32;
    if instance.get_base_param(&id) != index {
        instance.set_base_param(&id, index);
    }
}

/// Put back what [`apply`] replaced.
pub(crate) fn restore(project: &mut Project, layer: &LayerId, before: Before) {
    let Ok(instance) = scene_instance(project, layer) else {
        return;
    };
    let mut doc = before.doc;
    if let Some(current) = &instance.scene {
        doc.models.keep_from(&current.models);
    }
    if let Err(error) = instance.install_scene_document(doc) {
        eprintln!("[manifold-editing] scene undo did not compile: {error}");
    }
    before.instance.restore(instance);
}
