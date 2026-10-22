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
    SceneEdit, SetSceneItemFieldsCommand,
};
pub use group::{group_commands, ungroup_commands};
pub use import::{SwapPlan, import_model_commands, swap_model_file_commands};
pub use material::{add_object_commands, new_material, own_material_commands};
pub use modifiers::{add_modifier, move_modifier, new_modifier};

#[cfg(test)]
mod tests;

use manifold_core::effects::PresetInstance;
use manifold_core::params::Param;
use manifold_core::project::Project;
use manifold_core::scene_doc::{ItemFields, ItemId, SceneDoc, SceneEditError, split_param_id};
use manifold_core::LayerId;

use crate::commands::graph::{prune_by_param_id, prune_instance_params};

/// What a document edit changed, so undo puts back exactly that: the
/// document, every param (with its modulators) the edit removed or reset,
/// and the Camera row's pick where the edit moved it. A value written since,
/// outside undo, stays as it is.
#[derive(Debug)]
pub(crate) struct Before {
    doc: SceneDoc,
    removed: ParamSlice,
    pick: Option<(String, f32)>,
    /// Params the edit wrote once installed (a moved item kept in place),
    /// with their value and base before, in write order.
    written: Vec<(String, f32, f32)>,
}

impl Before {
    /// Remember what `writes` replaced, so undo puts it back.
    pub(crate) fn note_written(&mut self, writes: Vec<(String, f32, f32)>) {
        self.written.extend(writes);
    }
}

/// Some params of an instance and every modulator aimed at them.
#[derive(Debug, Default)]
struct ParamSlice {
    params: Vec<Param>,
    drivers: Vec<manifold_core::effects::ParameterDriver>,
    envelopes: Vec<manifold_core::effects::ParamEnvelope>,
    ableton_mappings: Vec<manifold_core::ableton_mapping::AbletonParamMapping>,
    audio_mods: Vec<manifold_core::audio_mod::ParameterAudioMod>,
    automation_lanes: Vec<manifold_core::effects::AutomationLane>,
}

fn kept<T: Clone>(values: &Option<Vec<T>>, keep: impl Fn(&T) -> bool) -> Vec<T> {
    values.iter().flatten().filter(|value| keep(value)).cloned().collect()
}

fn extended<T>(values: &mut Option<Vec<T>>, more: Vec<T>) {
    if !more.is_empty() {
        values.get_or_insert_with(Vec::new).extend(more);
    }
}

impl ParamSlice {
    fn capture(instance: &PresetInstance, ids: &[String]) -> Self {
        let wanted = |id: &str| ids.iter().any(|want| want == id);
        Self {
            params: instance.params.iter().filter(|p| wanted(p.id())).cloned().collect(),
            drivers: kept(&instance.drivers, |d| wanted(&d.param_id)),
            envelopes: kept(&instance.envelopes, |e| wanted(&e.param_id)),
            ableton_mappings: kept(&instance.ableton_mappings, |m| wanted(&m.param_id)),
            audio_mods: kept(&instance.audio_mods, |a| wanted(&a.param_id)),
            automation_lanes: kept(&instance.automation_lanes, |l| wanted(&l.param_id)),
        }
    }

    /// Put the params back over the ones the document compiled to, and their
    /// modulators beside the ones there.
    fn restore(self, instance: &mut PresetInstance) {
        for param in self.params {
            match instance.params.get_mut(param.id()) {
                Some(slot) => *slot = param,
                None => instance.params.push(param),
            }
        }
        extended(&mut instance.drivers, self.drivers);
        extended(&mut instance.envelopes, self.envelopes);
        extended(&mut instance.ableton_mappings, self.ableton_mappings);
        extended(&mut instance.audio_mods, self.audio_mods);
        extended(&mut instance.automation_lanes, self.automation_lanes);
    }
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
    edit: impl FnOnce(&mut SceneDoc, &dyn Fn(&str) -> Option<f32>) -> Result<(), SceneEditError>,
) -> Result<Before, String> {
    let instance = scene_instance(project, layer)?;
    let doc = instance.scene.clone().expect("scene layer");
    let shown = shown_camera(&doc, instance);
    let mut edited = doc.clone();
    let values = |id: &str| instance.params.contains(id).then(|| instance.get_base_param(id));
    edit(&mut edited, &values).map_err(|error| error.to_string())?;
    // A swapped Modifier card's rows start at the new type's defaults
    // (section 7.19), even where the two types share a property name.
    let swapped = swapped_cards(&doc, &edited);
    let old_ids: Vec<String> = instance.params.iter().map(|p| p.id().to_owned()).collect();
    let reset: Vec<String> = old_ids
        .iter()
        .filter(|id| split_param_id(id).is_some_and(|(item, _)| swapped.iter().any(|card| card.as_str() == item)))
        .cloned()
        .collect();
    let mut held = ParamSlice::capture(instance, &old_ids);
    instance.install_scene_document(edited)?;
    let gone: Vec<String> = old_ids.into_iter().filter(|id| instance.params.get(id).is_none()).collect();
    prune_instance_params(instance, &gone);
    if !reset.is_empty() {
        prune_instance_params(instance, &reset);
        instance.refresh_manifest_from_graph();
    }
    let changed: Vec<String> = gone.into_iter().chain(reset).collect();
    held = ParamSlice::capture_kept(held, &changed);
    let pick = keep_showing(instance, shown);
    Ok(Before { doc, removed: held, pick, written: Vec::new() })
}

impl ParamSlice {
    /// Only the part of `slice` aimed at `ids`.
    fn capture_kept(slice: Self, ids: &[String]) -> Self {
        let wanted = |id: &str| ids.iter().any(|want| want == id);
        Self {
            params: slice.params.into_iter().filter(|p| wanted(p.id())).collect(),
            drivers: slice.drivers.into_iter().filter(|d| wanted(&d.param_id)).collect(),
            envelopes: slice.envelopes.into_iter().filter(|e| wanted(&e.param_id)).collect(),
            ableton_mappings: slice.ableton_mappings.into_iter().filter(|m| wanted(&m.param_id)).collect(),
            audio_mods: slice.audio_mods.into_iter().filter(|a| wanted(&a.param_id)).collect(),
            automation_lanes: slice.automation_lanes.into_iter().filter(|l| wanted(&l.param_id)).collect(),
        }
    }
}

/// The Modifier cards whose type an edit changed.
fn swapped_cards(before: &SceneDoc, after: &SceneDoc) -> Vec<ItemId> {
    let type_of = |doc: &SceneDoc, id: &ItemId| match doc.find(id).map(|item| &item.fields) {
        Some(ItemFields::Modifier(card)) => Some(card.modifier),
        _ => None,
    };
    after
        .items
        .iter()
        .filter(|item| matches!(item.fields, ItemFields::Modifier(_)))
        .filter(|item| type_of(before, &item.id).is_some_and(|old| Some(old) != type_of(after, &item.id)))
        .map(|item| item.id.clone())
        .collect()
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
/// the same undo step. Returns the row's id and its value before, when moved.
fn keep_showing(instance: &mut PresetInstance, shown: Option<ItemId>) -> Option<(String, f32)> {
    let doc = instance.scene.as_ref()?;
    let id = doc.camera_pick_param().filter(|id| instance.params.contains(id))?;
    let index = shown.and_then(|camera| doc.cameras().iter().position(|&c| *c == camera)).unwrap_or(0) as f32;
    let was = instance.get_base_param(&id);
    (was != index && instance.set_base_param(&id, index)).then_some((id, was))
}

/// Put back what [`apply`] changed: the document, the params it removed or
/// reset with their modulators, and the Camera row. Params the edit added
/// leave with their modulators; every other value stays as it is now.
pub(crate) fn restore(project: &mut Project, layer: &LayerId, before: Before) {
    let Ok(instance) = scene_instance(project, layer) else {
        return;
    };
    let ids: Vec<String> = instance.params.iter().map(|p| p.id().to_owned()).collect();
    // A reset param comes back with its own modulators, not the ones the new
    // type's row gathered.
    let reset: Vec<&str> = before.removed.params.iter().map(Param::id).filter(|id| ids.iter().any(|now| now == id)).collect();
    drop_modulators(instance, &reset);
    let mut doc = before.doc;
    if let Some(current) = &instance.scene {
        doc.models.keep_from(&current.models);
    }
    if let Err(error) = instance.install_scene_document(doc) {
        eprintln!("[manifold-editing] scene undo did not compile: {error}");
    }
    let added: Vec<String> = ids.into_iter().filter(|id| instance.params.get(id).is_none()).collect();
    prune_instance_params(instance, &added);
    before.removed.restore(instance);
    if let Some((id, was)) = before.pick {
        instance.set_base_param(&id, was);
    }
    for (id, value, base) in before.written.into_iter().rev() {
        if let Some(param) = instance.params.get_mut(&id) {
            param.value = value;
            param.base = base;
        }
    }
}

/// Drop the modulators aimed at `ids`, keeping the params.
fn drop_modulators(instance: &mut PresetInstance, ids: &[&str]) {
    let ids: std::collections::BTreeSet<&str> = ids.iter().copied().collect();
    prune_by_param_id(&mut instance.drivers, &ids, |d| &d.param_id);
    prune_by_param_id(&mut instance.envelopes, &ids, |e| &e.param_id);
    prune_by_param_id(&mut instance.ableton_mappings, &ids, |m| &m.param_id);
    prune_by_param_id(&mut instance.audio_mods, &ids, |a| &a.param_id);
    prune_by_param_id(&mut instance.automation_lanes, &ids, |l| &l.param_id);
}
