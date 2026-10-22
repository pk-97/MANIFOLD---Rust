//! Model imports into a scene document (SCENE_DOCUMENT_DESIGN.md section 7.7
//! (Imports)): the commands for a picked model and for a Model File swap.
//! Each returns the command list for one `ExecuteBatch`, so one undo step.
//! The file drop goes through `Application::finish_import_model` instead,
//! after its worker has parsed the file off the UI thread.

use std::path::Path;

use manifold_core::project::Project;
use manifold_core::scene_doc::{ItemId, SceneDoc};
use manifold_core::LayerId;
use manifold_editing::command::Command;
use manifold_editing::commands::scene_doc::{import_model_commands, swap_model_file_commands};
use manifold_nodes_scene::node_graph::gltf_import::import_model;

/// Parse `file` and add it to scene layer `layer` as a Group (or one Object).
pub(crate) fn add_model_commands(layer: &LayerId, file: &Path) -> Result<Vec<Box<dyn Command>>, String> {
    let model = import_model(file)?;
    let group = ItemId::new(manifold_core::math::short_id());
    Ok(import_model_commands(layer, None, usize::MAX, group, &file.to_string_lossy(), &model))
}

/// What a Model File swap does, ready to run.
pub(crate) struct ModelSwap {
    pub commands: Vec<Box<dyn Command>>,
    /// The toast to show: how many parts the new file no longer has.
    pub toast: Option<String>,
}

/// Point import Group `group` of scene layer `layer` at `file`. Parts whose
/// file node path still exists keep their edits; the rest are removed and new
/// paths are added, all in the returned commands.
pub(crate) fn swap_model_file(project: &Project, layer: &LayerId, group: &ItemId, file: &Path) -> Result<ModelSwap, String> {
    let doc: &SceneDoc = project
        .timeline
        .find_layer_by_id(layer.as_str())
        .and_then(|(_, layer)| layer.gen_params())
        .and_then(|fx| fx.scene.as_ref())
        .ok_or_else(|| "this layer has no scene".to_owned())?;
    let model = import_model(file)?;
    let plan = swap_model_file_commands(layer, doc, group, &file.to_string_lossy(), &model)?;
    let toast = (plan.removed > 0).then(|| match plan.removed {
        1 => "The new file has no part for 1 object; it was removed".to_owned(),
        n => format!("The new file has no part for {n} objects; they were removed"),
    });
    Ok(ModelSwap { commands: plan.commands, toast })
}
