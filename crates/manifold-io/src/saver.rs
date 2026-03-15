use std::path::Path;
use manifold_core::project::Project;

/// Save a project to disk as JSON.
pub fn save_project(project: &Project, path: &Path) -> Result<(), SaveError> {
    let json = serde_json::to_string_pretty(project)
        .map_err(|e| SaveError::Serialize(e.to_string()))?;

    std::fs::write(path, json)
        .map_err(|e| SaveError::Io(e.to_string()))?;

    Ok(())
}

#[derive(Debug)]
pub enum SaveError {
    Io(String),
    Serialize(String),
}

impl std::fmt::Display for SaveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SaveError::Io(e) => write!(f, "IO error: {e}"),
            SaveError::Serialize(e) => write!(f, "Serialize error: {e}"),
        }
    }
}

impl std::error::Error for SaveError {}
