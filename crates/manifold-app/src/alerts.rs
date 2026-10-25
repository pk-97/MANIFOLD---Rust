//! Native alert dialogs for project-file failures, crash notices and the
//! quit prompt.
//!
//! Save and load failures were log-only — the
//! worst failure mode is believing you saved when you didn't. These are
//! blocking native dialogs on the UI thread, following the existing pattern
//! (`app_render.rs` `confirm_remove_node_orphans`): fine for authoring-time
//! events, never reached during performance — perform mode parks autosave and
//! has no dialogs on any path but one: quitting with unsaved edits asks there
//! too ([`save_changes`]), because losing the set is worse than a dialog.
//!
//! When the perform-surface chrome work lands, the
//! non-blocking variants of these (autosave failure strip, crash banner)
//! become chrome widgets; the call sites here stay the same.

/// Blocking error dialog. Use for failures the user must not miss:
/// save failed, project load failed, snapshot restore failed.
pub(crate) fn error(title: &str, body: &str) {
    log::error!("[Alert] {title}: {body}");
    rfd::MessageDialog::new()
        .set_title(title)
        .set_description(body)
        .set_buttons(rfd::MessageButtons::Ok)
        .set_level(rfd::MessageLevel::Error)
        .show();
}

/// Blocking info dialog — the crash-notice "quiet banner" until a chrome
/// banner widget exists (one dialog per unclean exit, editor launch only).
pub(crate) fn info(title: &str, body: &str) {
    log::info!("[Alert] {title}: {body}");
    rfd::MessageDialog::new()
        .set_title(title)
        .set_description(body)
        .set_buttons(rfd::MessageButtons::Ok)
        .set_level(rfd::MessageLevel::Info)
        .show();
}

/// The answer to the unsaved-changes prompt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SaveChoice {
    Save,
    DontSave,
    Cancel,
}

/// Blocking Save / Don't Save / Cancel prompt before quitting with unsaved
/// edits. Escape, or closing the dialog any other way, is Cancel.
pub(crate) fn save_changes(project_name: &str) -> SaveChoice {
    const SAVE: &str = "Save";
    const DONT_SAVE: &str = "Don't Save";
    const CANCEL: &str = "Cancel";
    let answer = rfd::MessageDialog::new()
        .set_title(format!(
            "Do you want to save the changes to \u{201c}{project_name}\u{201d}?"
        ))
        .set_description("Your changes will be lost if you don't save them.")
        .set_buttons(rfd::MessageButtons::YesNoCancelCustom(
            SAVE.into(),
            DONT_SAVE.into(),
            CANCEL.into(),
        ))
        .set_level(rfd::MessageLevel::Warning)
        .show();
    let choice = match answer {
        rfd::MessageDialogResult::Custom(label) if label == SAVE => SaveChoice::Save,
        rfd::MessageDialogResult::Custom(label) if label == DONT_SAVE => SaveChoice::DontSave,
        // Backends without custom labels answer with the stock buttons.
        rfd::MessageDialogResult::Yes => SaveChoice::Save,
        rfd::MessageDialogResult::No => SaveChoice::DontSave,
        _ => SaveChoice::Cancel,
    };
    log::info!("[Alert] quit with unsaved changes: {choice:?}");
    choice
}

/// Blocking Yes/No confirmation. Returns true only on Yes.
pub(crate) fn confirm(title: &str, body: &str) -> bool {
    rfd::MessageDialog::new()
        .set_title(title)
        .set_description(body)
        .set_buttons(rfd::MessageButtons::YesNo)
        .set_level(rfd::MessageLevel::Warning)
        .show()
        == rfd::MessageDialogResult::Yes
}
