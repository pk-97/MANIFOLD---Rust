//! Model imports into a scene document (SCENE_DOCUMENT_DESIGN.md section 7.7
//! (Imports)): the commands for a Model File swap, as the command list for
//! one `ExecuteBatch`, so one undo step. Every model file, new or swapped
//! in, is parsed by the import worker off the UI thread first
//! (`Application::finish_import_model`).

use manifold_core::LayerId;
use manifold_core::project::Project;
use manifold_core::scene_doc::{ImportedModel, ItemId};
use manifold_editing::command::Command;
use manifold_editing::commands::scene_doc::swap_model_file_commands;

/// What a Model File swap does, ready to run.
pub(crate) struct ModelSwap {
    pub commands: Vec<Box<dyn Command>>,
    /// The toast to show: how many parts the new file no longer has.
    pub toast: Option<String>,
}

/// Point import `item` of scene layer `layer` at `file`, parsed as `model`:
/// an import Group reconciles its parts by file node path (survivors keep
/// their edits, the rest are removed, new paths are added); a single
/// imported Object keeps itself and its edits and draws the new mesh. All in
/// the returned commands.
pub(crate) fn swap_model_file(
    project: &Project,
    layer: &LayerId,
    item: &ItemId,
    file: &str,
    model: &ImportedModel,
) -> Result<ModelSwap, String> {
    let doc = project
        .scene_of(layer)
        .ok_or_else(|| "this layer has no scene".to_owned())?;
    let plan = swap_model_file_commands(layer, doc, item, file, model)?;
    let toast = (plan.removed > 0).then(|| match plan.removed {
        1 => "The new file has no part for 1 object; it was removed".to_owned(),
        n => format!("The new file has no part for {n} objects; they were removed"),
    });
    Ok(ModelSwap {
        commands: plan.commands,
        toast,
    })
}
