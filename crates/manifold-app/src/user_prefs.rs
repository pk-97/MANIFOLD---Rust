//! Platform-persistent key-value string storage.
//!
//! 1:1 equivalent of Unity's `PlayerPrefs` (string subset).
//! Backs onto `prefs.json` in the app's data folder (`AppDirs::data`).

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

// ── Constants ────────────────────────────────────────────────────────

const APP_DIR_NAME: &str = "MANIFOLD";
const PREFS_FILE_NAME: &str = "prefs.json";

// ── Data directory ───────────────────────────────────────────────────

/// The app's persistent data directory (same root `prefs.json` lives in).
/// Falls back to a relative `MANIFOLD` folder when `HOME` is unset, so this
/// always returns a usable path.
pub fn app_data_dir() -> PathBuf {
    manifold_foundation::AppDirs::current()
        .map(|dirs| dirs.data)
        .unwrap_or_else(|| PathBuf::from(APP_DIR_NAME))
}

// ── UserPrefs ────────────────────────────────────────────────────────

/// Simple key-value string storage persisted to disk as JSON.
///
/// Maps 1:1 to Unity `PlayerPrefs.GetString` / `SetString` / `Save`.
/// Keys use the same `"MANIFOLD_*"` naming convention as the Unity version
/// so that the prefs semantics are identical across ports.
pub struct UserPrefs {
    data: HashMap<String, String>,
    file_path: PathBuf,
}

impl UserPrefs {
    /// Load prefs from disk (or start empty if the file doesn't exist yet).
    /// Called once at app startup — equivalent to Unity's implicit PlayerPrefs load.
    pub fn load() -> Self {
        let file_path = app_data_dir().join(PREFS_FILE_NAME);

        let data = if file_path.exists() {
            fs::read_to_string(&file_path)
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default()
        } else {
            HashMap::new()
        };

        log::info!("[UserPrefs] Loaded from {}", file_path.display());
        Self { data, file_path }
    }

    /// Prefs that never touch the real config file — the headless
    /// ui-snapshot driver's seam. Reads resolve to caller defaults (empty
    /// map, deterministic on any host); a stray `save()` from a dispatched
    /// action lands in the OS temp dir, not the user's prefs.
    #[cfg(any(test, feature = "ui-snapshot"))]
    pub fn in_memory() -> Self {
        Self {
            data: HashMap::new(),
            file_path: std::env::temp_dir().join("manifold-ui-snap-prefs.json"),
        }
    }

    /// Same shape as [`Self::in_memory`] but always available under
    /// `cfg(test)` — unit tests elsewhere in the crate (e.g.
    /// `blender_import`'s discovery tests) want a throwaway `UserPrefs`
    /// without depending on the `ui-snapshot` feature being enabled.
    #[cfg(test)]
    pub(crate) fn for_test() -> Self {
        Self {
            data: HashMap::new(),
            file_path: std::env::temp_dir().join("manifold-test-prefs.json"),
        }
    }

    /// Same disk-free shape as `in_memory`/`for_test`, but unconditionally
    /// available — the background model-import worker
    /// (`import_worker.rs::run_import_worker`) uses this to carry just the
    /// one pref key `blender_import::discover_blender` reads
    /// (`BLENDER_PATH_PREF_KEY`) across the thread boundary, rather than
    /// requiring `UserPrefs` itself to be `Clone`/`Send`. A real,
    /// always-built production call site — not test-only, despite the
    /// sibling constructors' naming.
    pub(crate) fn ephemeral() -> Self {
        Self {
            data: HashMap::new(),
            file_path: std::env::temp_dir().join("manifold-import-worker-prefs.json"),
        }
    }

    /// Get a string value by key, returning `default` if the key doesn't exist.
    /// Equivalent to `PlayerPrefs.GetString(key, default)`.
    pub fn get_string(&self, key: &str, default: &str) -> String {
        self.data
            .get(key)
            .cloned()
            .unwrap_or_else(|| default.to_string())
    }

    /// Set a string value by key.
    /// Equivalent to `PlayerPrefs.SetString(key, value)`.
    /// Does NOT persist to disk until `save()` is called.
    pub fn set_string(&mut self, key: &str, value: &str) {
        self.data.insert(key.to_string(), value.to_string());
    }

    /// Persist all prefs to disk.
    /// Equivalent to `PlayerPrefs.Save()`.
    pub fn save(&self) {
        if let Some(parent) = self.file_path.parent()
            && let Err(e) = fs::create_dir_all(parent)
        {
            log::error!("[UserPrefs] Failed to create dir {}: {e}", parent.display());
            return;
        }
        match serde_json::to_string_pretty(&self.data) {
            Ok(json) => {
                if let Err(e) = fs::write(&self.file_path, &json) {
                    log::error!(
                        "[UserPrefs] Failed to write {}: {e}",
                        self.file_path.display()
                    );
                }
            }
            Err(e) => log::error!("[UserPrefs] Failed to serialize prefs: {e}"),
        }
    }
}
