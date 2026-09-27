use crate::archive;
use crate::path_resolver::PathResolver;
use manifold_core::project::Project;
use std::path::Path;

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
    crate::graph_schema::validate_project_graphs(project)
        .map_err(|e| SaveError::Serialize(e.to_string()))?;
    let path_str = path.to_string_lossy().to_string();

    // Create parent directory if needed (Unity line 139-141)
    if let Some(directory) = path.parent()
        && !directory.as_os_str().is_empty()
        && !directory.exists()
    {
        std::fs::create_dir_all(directory)
            .map_err(|e| SaveError::Io(format!("Failed to create directory: {e}")))?;
    }

    // Compute relative paths before serialization (Unity line 144)
    let project_dir = path
        .parent()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();
    PathResolver::store_relative_paths(project, &project_dir);

    // Serialize project to JSON
    let json = portable_project_json(project, Path::new(&project_dir))?;

    // Delegate to V2 archive save
    archive::save_v2_archive(&json, &project.project_name, &path_str, label, is_auto)
        .map_err(SaveError::Io)?;

    // Update last_saved_path after successful save (Unity line 231)
    project.last_saved_path = path_str;

    Ok(())
}

/// Save a project as plain JSON (V1 format, for backwards compatibility or testing).
pub fn save_project_v1(project: &Project, path: &Path) -> Result<(), SaveError> {
    crate::graph_schema::validate_project_graphs(project)
        .map_err(|e| SaveError::Serialize(e.to_string()))?;
    // Create parent directory if needed
    if let Some(directory) = path.parent()
        && !directory.as_os_str().is_empty()
        && !directory.exists()
    {
        std::fs::create_dir_all(directory)
            .map_err(|e| SaveError::Io(format!("Failed to create directory: {e}")))?;
    }

    let json = portable_project_json(project, path.parent().unwrap_or(Path::new(".")))?;

    std::fs::write(path, json).map_err(|e| SaveError::Io(e.to_string()))?;

    Ok(())
}

/// String-bound assets have no relative-path sibling in the project model.
/// Rewrite their owned values on a serialization snapshot, using the same
/// inventory and relocation helpers as Collect All and Save. Live render paths
/// remain absolute; loading resolves these relative values against the new
/// project directory before falling back to missing-file searches.
fn portable_project_json(project: &Project, directory: &Path) -> Result<String, SaveError> {
    use crate::collect::{
        AssetTarget, collect_asset_paths, re_point_scene_modifier_asset, re_point_string_param,
    };
    let directory = if directory.as_os_str().is_empty() {
        Path::new(".")
    } else {
        directory
    };
    let directory = std::fs::canonicalize(directory).unwrap_or_else(|_| directory.to_owned());
    let mut snapshot = None;
    for asset in collect_asset_paths(project) {
        if !matches!(
            asset.target,
            AssetTarget::StringParam { .. } | AssetTarget::SceneModifierStringParam { .. }
        ) {
            continue;
        }
        let absolute = if asset.path.is_absolute() {
            asset.path.clone()
        } else {
            directory.join(&asset.path)
        };
        let absolute = std::fs::canonicalize(&absolute).unwrap_or(absolute);
        let Ok(relative) = absolute.strip_prefix(&directory) else {
            continue;
        };
        let old = asset.path.to_string_lossy();
        let relative = relative.to_string_lossy();
        if relative.is_empty() || relative == old {
            continue;
        }
        let saved = snapshot.get_or_insert_with(|| project.clone());
        match &asset.target {
            AssetTarget::StringParam { layer_id, key, .. } => {
                re_point_string_param(saved, layer_id, key, &old, &relative);
            }
            target @ AssetTarget::SceneModifierStringParam { .. } => {
                re_point_scene_modifier_asset(saved, target, &old, &relative);
            }
            _ => unreachable!("string asset targets filtered above"),
        }
    }
    serde_json::to_string_pretty(snapshot.as_ref().unwrap_or(project))
        .map_err(|e| SaveError::Serialize(e.to_string()))
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
