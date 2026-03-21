//! Platform-persistent key-value string storage.
//!
//! 1:1 equivalent of Unity's `PlayerPrefs` (string subset).
//! Backs onto a JSON file at the platform config directory:
//!   macOS:   ~/Library/Application Support/MANIFOLD/prefs.json
//!   Linux:   ~/.config/manifold/prefs.json
//!   Windows: %APPDATA%/MANIFOLD/prefs.json

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

// ── Constants ────────────────────────────────────────────────────────

const APP_DIR_NAME: &str = "MANIFOLD";
const PREFS_FILE_NAME: &str = "prefs.json";

// ── Platform config directory ────────────────────────────────────────

fn config_dir() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        std::env::var("HOME")
            .ok()
            .map(|h| PathBuf::from(h).join("Library/Application Support"))
    }
    #[cfg(target_os = "linux")]
    {
        std::env::var("XDG_CONFIG_HOME")
            .ok()
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var("HOME")
                    .ok()
                    .map(|h| PathBuf::from(h).join(".config"))
            })
    }
    #[cfg(target_os = "windows")]
    {
        std::env::var("APPDATA").ok().map(PathBuf::from)
    }
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
        let dir = config_dir().unwrap_or_else(|| PathBuf::from("."));
        let prefs_dir = dir.join(APP_DIR_NAME);
        let file_path = prefs_dir.join(PREFS_FILE_NAME);

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
            && let Err(e) = fs::create_dir_all(parent) {
                log::error!("[UserPrefs] Failed to create dir {}: {e}", parent.display());
                return;
            }
        match serde_json::to_string_pretty(&self.data) {
            Ok(json) => {
                if let Err(e) = fs::write(&self.file_path, &json) {
                    log::error!("[UserPrefs] Failed to write {}: {e}", self.file_path.display());
                }
            }
            Err(e) => log::error!("[UserPrefs] Failed to serialize prefs: {e}"),
        }
    }
}
