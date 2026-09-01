//! Quarantined one-time schema migrations that are big/self-contained
//! enough to deserve their own module instead of a function in
//! `crate::migrate`. See `docs/PARAM_STORAGE_DESIGN.md` section 4 (D4) for the
//! first (and so far only) resident.

use std::cell::RefCell;

pub mod param_storage_v14;
pub mod scene_cinematic_tail_v1130;
pub mod scene_scale_coc_v1140;
pub mod scene_transform_v1120;

thread_local! {
    /// Per-load handoff from the pre-deserialize migrations (which run pure
    /// `Value → Value` and can't touch `Project::load_report`) to the loader
    /// (which can). Migrations push human-readable notes (skip-loudly
    /// signals, upgrade summaries); the loader drains after each
    /// `migrate_if_needed` call. Thread-local, never shared — a load is a
    /// single-threaded affair per call.
    static MIGRATION_NOTES: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

pub(crate) fn note_migration(msg: String) {
    MIGRATION_NOTES.with(|n| n.borrow_mut().push(msg));
}

/// Drain the accumulated notes. Called once per load by the loader, and by
/// tests that assert a note fired.
pub fn take_migration_notes() -> Vec<String> {
    MIGRATION_NOTES.with(|n| std::mem::take(&mut *n.borrow_mut()))
}
