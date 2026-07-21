//! Browser popup preset actions: rename / duplicate / delete / reveal
//! (UI_FUNNEL_DECOMPOSITION P-B, D6). One slice of the inspector dispatch,
//! reached by `dispatch_inspector`'s first-non-unhandled chain. Arms are the
//! former `dispatch_inspector` arms VERBATIM (they already read `ctx` fields
//! directly); a `_ => unhandled()` fall-through lets the chain advance.

use super::super::DispatchResult;
use crate::content_command::ContentCommand;
use manifold_ui::BrowserAction;

/// `manifold_ui::panels::browser_popup::BrowserPopupMode` stands in for
/// `manifold_core::preset_def::PresetKind` on the UI side of the browser's
/// management actions (PRESET_LIBRARY_DESIGN P5) — `manifold-ui` mirrors core
/// types rather than depending on `manifold-core` (see `BrowserCellContext`'s
/// doc comment). `Node` never reaches these arms in practice: the browser
/// only classifies a source (and therefore only ever fires
/// `BrowserCellRightClicked`) for the Effect/Generator pickers, never the
/// graph-editor's node picker — degrade to `Effect` rather than panic if that
/// invariant is ever violated.
pub(crate) fn browser_mode_to_kind(
    mode: manifold_ui::panels::browser_popup::BrowserPopupMode,
) -> manifold_core::preset_def::PresetKind {
    use manifold_ui::panels::browser_popup::BrowserPopupMode;
    match mode {
        BrowserPopupMode::Effect | BrowserPopupMode::Node => {
            manifold_core::preset_def::PresetKind::Effect
        }
        BrowserPopupMode::Generator => manifold_core::preset_def::PresetKind::Generator,
    }
}

pub(crate) fn dispatch_browser(action: &BrowserAction, ctx: &mut super::super::DispatchCtx) -> DispatchResult {
    match action {
        BrowserAction::BrowserCellRightClicked(..) => DispatchResult::handled(),
        BrowserAction::BrowserRenamePresetClicked(mode, type_id, source) => {
            use manifold_ui::panels::picker_core::Source;

            let kind = browser_mode_to_kind(*mode);
            let id = manifold_core::PresetTypeId::from_string(type_id.clone());
            let initial_name = match source {
                Source::MyLibrary => {
                    manifold_core::preset_type_registry::available_of_kind(kind)
                        .iter()
                        .find(|r| r.id.as_str() == type_id.as_str())
                        .map(|r| r.display_name.to_string())
                }
                Source::Project => ctx.project
                    .embedded_preset(&id)
                    .and_then(|ep| ep.def.preset_metadata.as_ref())
                    .map(|m| m.display_name.clone()),
                Source::Factory => None, // unreachable — the menu never offers Rename for Factory
            }
            .unwrap_or_else(|| type_id.clone());

            let mut result = DispatchResult::handled();
            result.begin_rename_preset = Some((kind, id, *source, initial_name));
            ctx.ui.browser_popup.close();
            result
        }
        BrowserAction::BrowserDuplicatePresetClicked(mode, type_id) => {
            // My Library only — the menu never offers Duplicate for Project.
            let kind = browser_mode_to_kind(*mode);
            let id = manifold_core::PresetTypeId::from_string(type_id.clone());
            let lib = crate::user_library::UserLibrary::new();
            match lib.duplicate(kind, &id) {
                Ok(new_id) => log::info!("[preset] duplicated '{}' as '{}'", id.as_str(), new_id.as_str()),
                Err(e) => log::error!("[preset] duplicate failed: {e}"),
            }
            ctx.ui.browser_popup.close();
            DispatchResult::handled()
        }
        BrowserAction::BrowserDeletePresetClicked(mode, type_id, source) => {
            use manifold_ui::panels::picker_core::Source;

            let kind = browser_mode_to_kind(*mode);
            let id = manifold_core::PresetTypeId::from_string(type_id.clone());
            let (place, undo_note) = match source {
                Source::MyLibrary => ("your library", "This can't be undone."),
                Source::Project => ("this project", "Undo (\u{2318}Z) restores it."),
                Source::Factory => return DispatchResult::handled(), // unreachable
            };
            let confirmed = crate::alerts::confirm(
                "Delete preset",
                &format!("Delete \"{type_id}\" from {place}?\n\n{undo_note}"),
            );
            if !confirmed {
                return DispatchResult::handled();
            }
            match source {
                Source::MyLibrary => {
                    let lib = crate::user_library::UserLibrary::new();
                    if let Err(e) = lib.delete(kind, &id) {
                        log::error!("[preset] delete failed: {e}");
                    }
                    ctx.ui.browser_popup.close();
                    DispatchResult::handled()
                }
                Source::Project => {
                    let cmd = manifold_editing::commands::preset::DeleteEmbeddedPresetCommand::new(id);
                    let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd);
                    boxed.execute(ctx.project);
                    ContentCommand::send(ctx.content_tx, ContentCommand::Execute(boxed));
                    ctx.ui.browser_popup.close();
                    DispatchResult::structural()
                }
                Source::Factory => unreachable!("returned above"),
            }
        }
        BrowserAction::BrowserRevealPresetClicked(mode, type_id) => {
            // My Library only — the menu never offers Reveal for Project
            // (a project-embedded preset has no file to reveal). Doesn't
            // close the popup: a read-only peek shouldn't interrupt browsing.
            let kind = browser_mode_to_kind(*mode);
            let id = manifold_core::PresetTypeId::from_string(type_id.clone());
            crate::user_library::UserLibrary::new().reveal(kind, &id);
            DispatchResult::handled()
        }
    }
}
