use serde::{Deserialize, Serialize};

/// The archive container format this build writes and is the newest it can
/// open. Coarse secondary gate for a future container-format bump — the
/// primary forward-compat gate is `manifold_core::project::CURRENT_PROJECT_VERSION`,
/// not this (PROJECT_FILE_INTEGRITY_DESIGN D2/D5).
pub const CURRENT_ARCHIVE_FORMAT_VERSION: i32 = 2;

/// Envelope stored as manifest.json inside a V2 .manifold zip archive.
/// Contains format version, project name, current snapshot hash, and history.
/// Port of C# ProjectManifest.cs (lines 13-28).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectManifest {
    #[serde(default = "default_format_version")]
    pub format_version: i32,

    #[serde(default)]
    pub name: String,

    #[serde(default)]
    pub current_hash: String,

    #[serde(default)]
    pub saved_at: String,

    #[serde(default)]
    pub history: Vec<SnapshotEntry>,
}

fn default_format_version() -> i32 {
    2
}

impl Default for ProjectManifest {
    fn default() -> Self {
        Self {
            format_version: 2,
            name: String::new(),
            current_hash: String::new(),
            saved_at: String::new(),
            history: Vec::new(),
        }
    }
}

/// A single snapshot entry in the project history.
/// Port of C# SnapshotEntry (ProjectManifest.cs lines 34-47).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotEntry {
    #[serde(default)]
    pub hash: String,

    #[serde(default)]
    pub timestamp: String,

    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,

    /// Unity serializes as "auto" (JsonProperty("auto"))
    #[serde(default, rename = "auto")]
    pub is_auto: bool,
}
