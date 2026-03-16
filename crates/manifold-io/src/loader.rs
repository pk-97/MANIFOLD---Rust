use std::path::Path;
use std::io::Read;
use manifold_core::project::Project;
use crate::migrate;

/// Load a .manifold project file.
///
/// Supports both formats:
/// - V2: ZIP archive containing `project.json` (latest Unity format)
/// - V1: Plain JSON text file (legacy)
///
/// Detection: tries ZIP first; if the file isn't a valid ZIP, falls back to plain JSON.
pub fn load_project(path: &Path) -> Result<Project, LoadError> {
    let file_bytes = std::fs::read(path)
        .map_err(|e| LoadError::Io(e.to_string()))?;

    // Try V2 ZIP format first
    let json = match extract_json_from_zip(&file_bytes) {
        Ok(json) => {
            log::info!("Loaded V2 .manifold archive from {}", path.display());
            json
        }
        Err(_) => {
            // Not a ZIP — treat as plain JSON (V1)
            String::from_utf8(file_bytes)
                .map_err(|e| LoadError::Io(format!("Invalid UTF-8: {e}")))?
        }
    };

    load_project_from_json(&json)
}

/// Extract `project.json` from a V2 ZIP archive.
fn extract_json_from_zip(bytes: &[u8]) -> Result<String, LoadError> {
    let cursor = std::io::Cursor::new(bytes);
    let mut archive = zip::ZipArchive::new(cursor)
        .map_err(|e| LoadError::Io(format!("Not a ZIP: {e}")))?;

    // Look for project.json entry
    let mut entry = archive.by_name("project.json")
        .map_err(|e| LoadError::Io(format!("No project.json in archive: {e}")))?;

    let mut json = String::new();
    entry.read_to_string(&mut json)
        .map_err(|e| LoadError::Io(format!("Failed to read project.json: {e}")))?;

    Ok(json)
}

/// Load from raw JSON string.
pub fn load_project_from_json(json: &str) -> Result<Project, LoadError> {
    // Run version migration
    let migrated = migrate::migrate_if_needed(json)
        .map_err(|e| LoadError::Migration(e.to_string()))?;

    // Deserialize
    let mut project: Project = serde_json::from_str(&migrated)
        .map_err(|e| LoadError::Deserialize(format!("{e}")))?;

    // Post-deserialize initialization
    project.on_after_deserialize();

    Ok(project)
}

#[derive(Debug)]
pub enum LoadError {
    Io(String),
    Migration(String),
    Deserialize(String),
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoadError::Io(e) => write!(f, "IO error: {e}"),
            LoadError::Migration(e) => write!(f, "Migration error: {e}"),
            LoadError::Deserialize(e) => write!(f, "Deserialize error: {e}"),
        }
    }
}

impl std::error::Error for LoadError {}
