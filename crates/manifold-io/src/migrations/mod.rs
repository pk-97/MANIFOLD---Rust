//! One-time schema migrations big enough to deserve their own module
//! instead of a function in `crate::migrate`. Each runs from one rung of
//! the ladder at or above the 1.11.0 floor.

use std::cell::RefCell;

pub mod contacts_wire_v1200;
pub mod pressure_cap_card_v1190;
pub mod reversed_depth_v1160;
pub mod rigid_body_density_v1170;
pub mod scene_scale_coc_v1140;
pub mod scene_transform_v1120;
pub mod solve_level_card_v1180;

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
