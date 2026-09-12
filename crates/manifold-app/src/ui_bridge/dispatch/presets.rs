//! Shared preset lifecycle for effect, generator and captured modifier targets.
use manifold_core::GraphTarget;
use manifold_editing::command::Command;
use manifold_editing::commands::preset::{ForkPresetCommand, RevertToLibraryCommand};
use manifold_ui::panels::actions::PresetActionKind;

use super::super::{DispatchCtx, DispatchResult};
use super::resolve::preset_source_def;
use crate::content_command::ContentCommand;
use crate::text_input::SavePresetDestination;

pub(crate) fn dispatch_preset(
    target: Option<GraphTarget>,
    kind: PresetActionKind,
    ctx: &mut DispatchCtx,
) -> DispatchResult {
    let Some(target) = target else {
        return DispatchResult::handled();
    };
    match kind {
        PresetActionKind::MakeUnique => {
            let source = if matches!(target, GraphTarget::SceneModifier { .. }) {
                crate::graph_target::resolve(ctx.project, &target).cloned()
            } else {
                preset_source_def(&target, ctx.project).map(|(def, _)| def)
            };
            if let Some(source) = source {
                submit(
                    &target,
                    Box::new(ForkPresetCommand::new(
                        target.clone(),
                        target.preset_kind(),
                        source,
                    )),
                    ctx,
                );
            }
            DispatchResult::structural()
        }
        PresetActionKind::Import => {
            if let Some(path) = rfd::FileDialog::new()
                .add_filter("MANIFOLD Preset", &["json"])
                .pick_file()
            {
                match manifold_io::preset_file::import_preset(&path) {
                    Ok(def) => submit(
                        &target,
                        Box::new(ForkPresetCommand::importing(
                            target.clone(),
                            target.preset_kind(),
                            def,
                        )),
                        ctx,
                    ),
                    Err(error) => log::error!("[preset] import failed: {error}"),
                }
            }
            DispatchResult::structural()
        }
        PresetActionKind::Export => {
            if let Some((def, id)) = preset_source_def(&target, ctx.project)
                && let Some(path) = rfd::FileDialog::new()
                    .add_filter("MANIFOLD Preset", &["json"])
                    .set_file_name(format!("{}.json", id.as_str()))
                    .save_file()
                && let Err(error) = manifold_io::preset_file::export_preset(&def, &path)
            {
                log::error!("[preset] export failed: {error}");
            }
            DispatchResult::handled()
        }
        PresetActionKind::SaveToLibrary
        | PresetActionKind::SaveToProject
        | PresetActionKind::PushToLibrary => {
            let mut result = DispatchResult::handled();
            if let Some((def, id)) = preset_source_def(&target, ctx.project) {
                let library = crate::user_library::UserLibrary::new();
                if kind == PresetActionKind::PushToLibrary
                    && library.is_user_entry(target.preset_kind(), &id)
                {
                    if let Err(error) = library.push(target.preset_kind(), &id, &def) {
                        log::error!("[preset] push to library failed: {error}");
                    }
                } else {
                    let destination = if kind == PresetActionKind::SaveToProject {
                        SavePresetDestination::Project
                    } else {
                        SavePresetDestination::Library
                    };
                    result.begin_save_preset = Some((target.preset_kind(), def, destination));
                }
            }
            result
        }
        PresetActionKind::RevertToLibrary => {
            let resolved = if matches!(target, GraphTarget::SceneModifier { .. }) {
                let Some(host) = target
                    .host_target()
                    .and_then(|host| crate::graph_target::resolve(ctx.project, host))
                else {
                    return DispatchResult::handled();
                };
                let Some(local) = crate::graph_target::resolve(ctx.project, &target) else {
                    return DispatchResult::handled();
                };
                match crate::modifier_preset::library_baseline(host, local) {
                    Ok(def) => def,
                    Err(error) => {
                        ContentCommand::send(
                            ctx.content_tx,
                            ContentCommand::GraphEditRejected(error),
                        );
                        return DispatchResult::handled();
                    }
                }
            } else {
                ctx.project
                    .instance_preset_id(&target)
                    .and_then(|id| manifold_renderer::node_graph::bundled_preset_def(&id).cloned())
            };
            let mut command = RevertToLibraryCommand::new(target.clone(), resolved.is_some());
            if let Some(def) = resolved {
                command = command.with_resolved_def(def);
            }
            submit(&target, Box::new(command), ctx);
            DispatchResult::structural()
        }
    }
}

fn submit(target: &GraphTarget, mut command: Box<dyn Command + Send>, ctx: &mut DispatchCtx) {
    if matches!(target, GraphTarget::SceneModifier { .. }) {
        // Structural admission and authoritative mutation both belong to content.
        ContentCommand::send(ctx.content_tx, ContentCommand::ExecuteOnContent(command));
    } else {
        // Preserve the existing optimistic effect/generator UI contract.
        command.execute(ctx.project);
        ContentCommand::send(ctx.content_tx, ContentCommand::Execute(command));
    }
}
