//! Per-context directory memory for file dialogs.
//!
//! 1:1 port of Unity `DialogPathMemory.cs`.
//! Each dialog type (open, save, export, import) remembers its own last-used
//! directory independently, matching professional DAW behaviour.
//! Persisted via `UserPrefs`.

use std::path::Path;

use crate::user_prefs::UserPrefs;

// ── DialogContext enum — mirrors Unity DialogContext ──────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[allow(dead_code)] // All variants ported from Unity — export/import will use them later
pub enum DialogContext {
    ProjectOpen,
    ProjectSave,
    ExportMP4,
    ExportResolveXML,
    MidiImport,
    PercussionImport,
}

// ── Constants — same as Unity DialogPathMemory.cs ────────────────────

const PREF_KEY_PREFIX: &str = "MANIFOLD_DialogPath_";

// Legacy keys from before centralised path memory.
const LEGACY_PROJECT_PATH_KEY: &str = "MANIFOLD_LastOpenedProjectPath";
const LEGACY_PERCUSSION_DIR_KEY: &str = "MANIFOLD_PercussionImportLastDir";

// ── Public API ───────────────────────────────────────────────────────

/// Returns the last-used directory for the given context, or `""` if none remembered.
/// On first call for a context, migrates from legacy PlayerPrefs keys if present.
///
/// 1:1 port of `DialogPathMemory.GetLastDirectory`.
pub fn get_last_directory(context: DialogContext, prefs: &mut UserPrefs) -> String {
    let key = pref_key(context);
    let dir = prefs.get_string(&key, "");

    if !dir.is_empty() {
        if Path::new(&dir).is_dir() {
            return dir;
        }
        return String::new();
    }

    // Migrate from legacy keys on first access.
    let legacy = get_legacy_path(context, prefs);
    if !legacy.is_empty() {
        if let Some(legacy_dir) = extract_directory(&legacy) {
            prefs.set_string(&key, &legacy_dir);
            prefs.save();
            return legacy_dir;
        }
    }

    String::new()
}

/// Remembers the directory of a successfully chosen file or folder path.
///
/// 1:1 port of `DialogPathMemory.RememberDirectory`.
pub fn remember_directory(context: DialogContext, file_or_dir_path: &str, prefs: &mut UserPrefs) {
    if file_or_dir_path.is_empty() {
        return;
    }

    if let Some(dir) = extract_directory(file_or_dir_path) {
        let key = pref_key(context);
        prefs.set_string(&key, &dir);
        prefs.save();
    }
}

// ── Private helpers ──────────────────────────────────────────────────

fn pref_key(context: DialogContext) -> String {
    format!("{}{:?}", PREF_KEY_PREFIX, context)
}

/// Extract the directory from a path. If the path itself is a directory, return it.
/// Otherwise return its parent if it exists on disk.
///
/// 1:1 port of `DialogPathMemory.ExtractDirectory`.
fn extract_directory(path: &str) -> Option<String> {
    let p = Path::new(path);
    if p.is_dir() {
        return Some(path.to_string());
    }

    p.parent()
        .filter(|d| d.is_dir())
        .map(|d| d.to_string_lossy().into_owned())
}

/// Get legacy PlayerPrefs path for migration.
///
/// 1:1 port of `DialogPathMemory.GetLegacyPath`.
fn get_legacy_path(context: DialogContext, prefs: &UserPrefs) -> String {
    match context {
        DialogContext::ProjectOpen | DialogContext::ProjectSave => {
            prefs.get_string(LEGACY_PROJECT_PATH_KEY, "")
        }
        DialogContext::PercussionImport => {
            prefs.get_string(LEGACY_PERCUSSION_DIR_KEY, "")
        }
        _ => String::new(),
    }
}
