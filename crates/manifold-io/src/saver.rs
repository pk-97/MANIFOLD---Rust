use std::path::Path;
use manifold_core::project::Project;
use crate::path_resolver::PathResolver;
use crate::archive;

/// Save a project to disk as a V2 ZIP archive.
/// Port of C# ProjectArchive.Save (lines 130-249).
///
/// Flow:
/// 1. Create parent directory if needed
/// 2. Store relative paths (PathResolver)
/// 3. Serialize to JSON
/// 4. Delegate to archive::save_v2_archive (hash, dedup, history, atomic write)
/// 5. Update project.last_saved_path
pub fn save_project(
    project: &mut Project,
    path: &Path,
    label: Option<&str>,
    is_auto: bool,
) -> Result<(), SaveError> {
    let path_str = path.to_string_lossy().to_string();

    // Create parent directory if needed (Unity line 139-141)
    if let Some(directory) = path.parent() {
        if !directory.as_os_str().is_empty() && !directory.exists() {
            std::fs::create_dir_all(directory)
                .map_err(|e| SaveError::Io(format!("Failed to create directory: {e}")))?;
        }
    }

    // Compute relative paths before serialization (Unity line 144)
    let project_dir = path.parent()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();
    PathResolver::store_relative_paths(project, &project_dir);

    // Serialize project to JSON
    let json = serde_json::to_string_pretty(project)
        .map_err(|e| SaveError::Serialize(e.to_string()))?;

    // Delegate to V2 archive save
    archive::save_v2_archive(
        &json,
        &project.project_name,
        &path_str,
        label,
        is_auto,
    )
    .map_err(|e| SaveError::Io(e))?;

    // Update last_saved_path after successful save (Unity line 231)
    project.last_saved_path = path_str;

    Ok(())
}

/// Save a project as plain JSON (V1 format, for backwards compatibility or testing).
pub fn save_project_v1(project: &Project, path: &Path) -> Result<(), SaveError> {
    // Create parent directory if needed
    if let Some(directory) = path.parent() {
        if !directory.as_os_str().is_empty() && !directory.exists() {
            std::fs::create_dir_all(directory)
                .map_err(|e| SaveError::Io(format!("Failed to create directory: {e}")))?;
        }
    }

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
