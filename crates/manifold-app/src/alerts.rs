//! Native alert dialogs for project-file failures and crash notices.
//!
//! GIG_RESILIENCE_DESIGN §6 (G4): save and load failures were log-only — the
//! worst failure mode is believing you saved when you didn't. These are
//! blocking native dialogs on the UI thread, following the existing pattern
//! (`app_render.rs` `confirm_remove_node_orphans`): fine for authoring-time
//! events, never reached during performance — perform mode parks autosave and
//! has no dialogs on any path.
//!
//! When the perform-surface chrome work lands (PERFORM_SURFACE_DESIGN), the
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
