use serde::{Deserialize, Serialize};

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

/// Lightweight project information without loading full data.
/// Port of C# ProjectInfo (ProjectSerializer.cs lines 108-120).
#[derive(Debug, Clone)]
pub struct ProjectInfo {
    pub project_name: String,
    pub project_version: String,
    pub file_path: String,
    pub file_size: u64,
    pub last_modified: std::time::SystemTime,
}

impl std::fmt::Display for ProjectInfo {
    /// Matches Unity's ToString() override:
    /// "{name} (v{version}) - {size}KB - Modified: {date}"
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let kb = self.file_size / 1024;
        write!(
            f,
            "{} (v{}) - {}KB - Modified: {:?}",
            self.project_name, self.project_version, kb, self.last_modified
        )
    }
}
