use std::path::Path;
use manifold_core::project::Project;
use crate::migrate;

/// Load a .manifold project file (V1 JSON format).
pub fn load_project(path: &Path) -> Result<Project, LoadError> {
    let json = std::fs::read_to_string(path)
        .map_err(|e| LoadError::Io(e.to_string()))?;

    load_project_from_json(&json)
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
