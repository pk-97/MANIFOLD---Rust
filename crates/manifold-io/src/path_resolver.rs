use crate::collect::{collect_asset_paths, AssetTarget};
use manifold_core::id::{ClipId, LayerId};
use manifold_core::project::Project;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Result of path resolution during project load.
/// Port of C# PathResolutionResult (PathResolver.cs lines 373-378).
#[derive(Debug, Default)]
pub struct PathResolutionResult {
    pub resolved_count: i32,
    pub already_valid_count: i32,
    pub unresolved_count: i32,
    pub unresolved: Vec<String>,
}

/// Resolves broken file paths after project migration.
/// Resolution chain: absolute path → relative path → filename+size search.
/// Port of C# PathResolver (PathResolver.cs lines 12-367).
pub struct PathResolver;

impl PathResolver {
    /// Resolve all file references in a project. Call after deserialization,
    /// before validate_clips / purge_orphaned_references.
    /// Port of C# PathResolver.ResolveAll (lines 18-161).
    ///
    /// The asset families come from [`collect_asset_paths`] — the single
    /// inventory (PROJECT_FOLDERS_DESIGN D4). Every family gets the same
    /// re-link chain: stored relative → filename+size search → known dirs.
    /// Video clips, layer video folders, and audio clips re-point their own
    /// field + relative sibling; flagged string params (GLB/HDRI paths) write
    /// back into `TimelineClip.string_params` through their [`AssetTarget`].
    pub fn resolve_all(project: &mut Project, project_file_path: &str) -> PathResolutionResult {
        let mut result = PathResolutionResult::default();

        if project_file_path.is_empty() {
            return result;
        }

        let project_dir = match Path::new(project_file_path).parent() {
            Some(p) => p.to_string_lossy().to_string(),
            None => return result,
        };

        if project_dir.is_empty() {
            return result;
        }

        let search_dirs = Self::build_search_dirs(project, &project_dir);
        let refs = collect_asset_paths(project);

        for asset in refs {
            match asset.target {
                AssetTarget::VideoClip { clip_id } => {
                    Self::resolve_video_clip(project, &clip_id, &project_dir, &search_dirs, &mut result);
                }
                AssetTarget::LayerVideoFolder { layer_id } => {
                    Self::resolve_video_folder(
                        project,
                        &layer_id,
                        &project_dir,
                        &search_dirs,
                        &mut result,
                    );
                }
                AssetTarget::AudioClip { layer_id, clip_id } => {
                    Self::resolve_audio_clip(
                        project,
                        &layer_id,
                        &clip_id,
                        &project_dir,
                        &search_dirs,
                        &mut result,
                    );
                }
                AssetTarget::StringParam { layer_id, key } => {
                    Self::resolve_string_param(
                        project,
                        &layer_id,
                        &key,
                        &asset.path,
                        &project_dir,
                        &search_dirs,
                        &mut result,
                    );
                }
            }
        }

        if result.resolved_count > 0 || result.unresolved_count > 0 {
            log::info!(
                "[PathResolver] Re-linked {} files, {} already valid, {} unresolved",
                result.resolved_count,
                result.already_valid_count,
                result.unresolved_count
            );
        }

        result
    }

    /// Populate relative paths on all path-bearing objects before save.
    /// Port of C# PathResolver.StoreRelativePaths (lines 166-209).
    ///
    /// Reads the same [`collect_asset_paths`] inventory as [`resolve_all`] —
    /// no second enumeration. Flagged string params have no relative sibling
    /// field, so they are skipped here.
    pub fn store_relative_paths(project: &mut Project, project_dir: &str) {
        if project_dir.is_empty() {
            return;
        }

        let refs = collect_asset_paths(project);

        for asset in refs {
            let relative = Self::make_relative(&asset.path.to_string_lossy(), project_dir);
            match asset.target {
                AssetTarget::VideoClip { clip_id } => {
                    if let Some(clip) = project.video_library.clips.iter_mut().find(|c| c.id == clip_id)
                    {
                        clip.relative_file_path = relative;
                    }
                }
                AssetTarget::LayerVideoFolder { layer_id } => {
                    if let Some((_, layer)) = project.timeline.find_layer_by_id_mut(layer_id.as_str())
                    {
                        layer.relative_video_folder_path = relative;
                    }
                }
                AssetTarget::AudioClip { layer_id, clip_id } => {
                    if let Some((_, layer)) = project.timeline.find_layer_by_id_mut(layer_id.as_str())
                        && let Some(clip) = layer.clips.iter_mut().find(|c| c.id == clip_id)
                    {
                        clip.relative_audio_file_path = relative;
                    }
                }
                AssetTarget::StringParam { .. } => {}
            }
        }
    }

    fn resolve_video_clip(
        project: &mut Project,
        clip_id: &str,
        project_dir: &str,
        search_dirs: &HashSet<String>,
        result: &mut PathResolutionResult,
    ) {
        let Some(clip) = project.video_library.clips.iter_mut().find(|c| c.id == clip_id) else {
            return;
        };
        if clip.file_path.is_empty() {
            return;
        }

        if Path::new(&clip.file_path).exists() {
            result.already_valid_count += 1;
            return;
        }

        let resolved = Self::try_resolve(
            &clip.file_path,
            clip.relative_file_path.as_deref(),
            clip.file_size,
            project_dir,
            search_dirs,
        );

        if let Some(resolved_path) = resolved {
            let relative = Self::make_relative(&resolved_path, project_dir);
            clip.file_path = resolved_path;
            clip.relative_file_path = relative;
            result.resolved_count += 1;
        } else {
            result.unresolved_count += 1;
            result.unresolved.push(clip.file_path.clone());
        }
    }

    fn resolve_video_folder(
        project: &mut Project,
        layer_id: &LayerId,
        project_dir: &str,
        search_dirs: &HashSet<String>,
        result: &mut PathResolutionResult,
    ) {
        let Some((_, layer)) = project.timeline.find_layer_by_id_mut(layer_id.as_str()) else {
            return;
        };
        let folder_path = match &layer.video_folder_path {
            Some(p) if !p.is_empty() => p.clone(),
            _ => return,
        };

        if Path::new(&folder_path).is_dir() {
            result.already_valid_count += 1;
            return;
        }

        let resolved = Self::try_resolve_directory(
            &folder_path,
            layer.relative_video_folder_path.as_deref(),
            project_dir,
            search_dirs,
        );

        if let Some(resolved_path) = resolved {
            let relative = Self::make_relative(&resolved_path, project_dir);
            layer.video_folder_path = Some(resolved_path);
            layer.relative_video_folder_path = relative;
            result.resolved_count += 1;
        } else {
            result.unresolved_count += 1;
            result.unresolved.push(folder_path);
        }
    }

    fn resolve_audio_clip(
        project: &mut Project,
        layer_id: &LayerId,
        clip_id: &ClipId,
        project_dir: &str,
        search_dirs: &HashSet<String>,
        result: &mut PathResolutionResult,
    ) {
        let Some((_, layer)) = project.timeline.find_layer_by_id_mut(layer_id.as_str()) else {
            return;
        };
        let Some(clip) = layer.clips.iter_mut().find(|c| &c.id == clip_id) else {
            return;
        };
        if clip.audio_file_path.is_empty() {
            return;
        }

        if Path::new(&clip.audio_file_path).exists() {
            result.already_valid_count += 1;
            return;
        }

        // Audio clips carry no file size — pass -1 to skip the size check in
        // the filename+size search.
        let resolved = Self::try_resolve(
            &clip.audio_file_path,
            clip.relative_audio_file_path.as_deref(),
            -1,
            project_dir,
            search_dirs,
        );

        if let Some(resolved_path) = resolved {
            let relative = Self::make_relative(&resolved_path, project_dir);
            clip.audio_file_path = resolved_path;
            clip.relative_audio_file_path = relative;
            result.resolved_count += 1;
        } else {
            result.unresolved_count += 1;
            result.unresolved.push(clip.audio_file_path.clone());
        }
    }

    /// Re-link a flagged string param (GLB/HDRI path) and write the resolved
    /// path back into `TimelineClip.string_params` via its [`AssetTarget`] —
    /// the per-clip home the design names (D4). The value has no relative
    /// sibling and no size, so the chain is filename search in known dirs
    /// only. The `path` handed in is the effective value [`collect_asset_paths`]
    /// enumerated (a per-clip override, or the preset-def default when no clip
    /// overrides it); write-back touches only clips whose override equals that
    /// path, so a def-default value is re-linked without clobbering explicit
    /// per-clip overrides that already point elsewhere.
    fn resolve_string_param(
        project: &mut Project,
        layer_id: &LayerId,
        key: &str,
        path: &Path,
        project_dir: &str,
        search_dirs: &HashSet<String>,
        result: &mut PathResolutionResult,
    ) {
        let path_str = path.to_string_lossy().to_string();
        if path_str.is_empty() {
            return;
        }

        if path.exists() {
            result.already_valid_count += 1;
            return;
        }

        let Some((_, layer)) = project.timeline.find_layer_by_id_mut(layer_id.as_str()) else {
            return;
        };

        let resolved = Self::try_resolve(&path_str, None, -1, project_dir, search_dirs);
        let Some(resolved_path) = resolved else {
            result.unresolved_count += 1;
            result.unresolved.push(path_str);
            return;
        };

        let mut written = 0;
        for clip in &mut layer.clips {
            let Some(params) = clip.string_params.as_mut() else {
                continue;
            };
            let Some(current) = params.get_mut(key) else {
                continue;
            };
            if *current == path_str {
                *current = resolved_path.clone();
                written += 1;
            }
        }

        if written > 0 {
            result.resolved_count += 1;
        } else {
            // The enumerated value had no per-clip override home to write back
            // into (it was the preset-def default) — nothing to re-point.
            result.unresolved_count += 1;
            result.unresolved.push(path_str);
        }
    }

    /// Try to resolve a missing file path. Returns the resolved absolute path, or None.
    /// Port of C# PathResolver.TryResolve (lines 219-254).
    ///
    /// - `absolute_path`: Original absolute path (broken)
    /// - `relative_path`: Stored relative path (may be None for legacy projects)
    /// - `expected_file_size`: Expected file size for search matching (-1 to skip size check)
    /// - `project_dir`: Directory containing the project file
    /// - `search_dirs`: Directories to search for filename matches
    pub fn try_resolve(
        absolute_path: &str,
        relative_path: Option<&str>,
        expected_file_size: i64,
        project_dir: &str,
        search_dirs: &HashSet<String>,
    ) -> Option<String> {
        // Step 1: Try relative path from project location
        if let Some(rel_path) = relative_path
            && !rel_path.is_empty()
            && !project_dir.is_empty()
        {
            let candidate = PathBuf::from(project_dir).join(rel_path);
            if let Ok(canonical) = std::fs::canonicalize(&candidate)
                && canonical.exists()
            {
                return Some(canonical.to_string_lossy().to_string());
            }
        }

        // Step 2: Filename+size search in known directories
        let file_name = Path::new(absolute_path)
            .file_name()?
            .to_string_lossy()
            .to_string();
        if file_name.is_empty() {
            return None;
        }

        for dir in search_dirs {
            if dir.is_empty() || !Path::new(dir).is_dir() {
                continue;
            }

            let candidate = PathBuf::from(dir).join(&file_name);
            if candidate.exists() {
                if expected_file_size < 0 {
                    return Some(candidate.to_string_lossy().to_string());
                }

                if let Ok(metadata) = std::fs::metadata(&candidate)
                    && metadata.len() as i64 == expected_file_size
                {
                    return Some(candidate.to_string_lossy().to_string());
                }
            }
        }

        None
    }

    /// Try to resolve a missing directory path.
    /// Port of C# PathResolver.TryResolveDirectory (lines 259-286).
    pub fn try_resolve_directory(
        absolute_path: &str,
        relative_path: Option<&str>,
        project_dir: &str,
        search_dirs: &HashSet<String>,
    ) -> Option<String> {
        // Step 1: Try relative path
        if let Some(rel_path) = relative_path
            && !rel_path.is_empty()
            && !project_dir.is_empty()
        {
            let candidate = PathBuf::from(project_dir).join(rel_path);
            if let Ok(canonical) = std::fs::canonicalize(&candidate)
                && canonical.is_dir()
            {
                return Some(canonical.to_string_lossy().to_string());
            }
        }

        // Step 2: Search by folder name in known parent directories
        let trimmed = absolute_path
            .trim_end_matches(std::path::MAIN_SEPARATOR)
            .trim_end_matches('/');
        let folder_name = Path::new(trimmed)
            .file_name()?
            .to_string_lossy()
            .to_string();
        if folder_name.is_empty() {
            return None;
        }

        for dir in search_dirs {
            if dir.is_empty() || !Path::new(dir).is_dir() {
                continue;
            }

            let candidate = PathBuf::from(dir).join(&folder_name);
            if candidate.is_dir() {
                return Some(candidate.to_string_lossy().to_string());
            }
        }

        None
    }

    /// Compute a relative path from a project directory to a target path.
    /// Returns None if the path cannot be made relative.
    /// Port of C# PathResolver.MakeRelative (lines 292-313).
    pub fn make_relative(absolute_path: &str, project_dir: &str) -> Option<String> {
        if absolute_path.is_empty() || project_dir.is_empty() {
            return None;
        }

        let abs = Path::new(absolute_path);
        let base = Path::new(project_dir);

        // Use pathdiff for cross-platform relative path computation
        // (equivalent to C#'s Uri.MakeRelativeUri)
        pathdiff::diff_paths(abs, base).map(|rel| rel.to_string_lossy().to_string())
    }

    /// Build the set of directories to search when doing filename-based re-linking.
    /// Port of C# PathResolver.BuildSearchDirs (lines 318-367).
    fn build_search_dirs(project: &Project, project_dir: &str) -> HashSet<String> {
        let mut dirs = HashSet::new();

        // 1. Project file directory
        if !project_dir.is_empty() && Path::new(project_dir).is_dir() {
            dirs.insert(project_dir.to_string());
        }

        // 2. Parent of project directory (catches sibling folders)
        if let Some(parent_dir) = Path::new(project_dir).parent() {
            let parent_str = parent_dir.to_string_lossy().to_string();
            if !parent_str.is_empty() && parent_dir.is_dir() {
                dirs.insert(parent_str.clone());
                // Also add immediate subdirectories of parent
                if let Ok(entries) = std::fs::read_dir(parent_dir) {
                    for entry in entries.flatten() {
                        if let Ok(ft) = entry.file_type()
                            && ft.is_dir()
                        {
                            dirs.insert(entry.path().to_string_lossy().to_string());
                        }
                    }
                }
            }
        }

        // 3. All layer video folder directories (even if broken — try parent)
        for layer in &project.timeline.layers {
            if let Some(ref folder_path) = layer.video_folder_path {
                if folder_path.is_empty() {
                    continue;
                }

                if Path::new(folder_path).is_dir() {
                    dirs.insert(folder_path.clone());
                }

                if let Some(folder_parent) = Path::new(folder_path).parent() {
                    let parent_str = folder_parent.to_string_lossy().to_string();
                    if !parent_str.is_empty() && folder_parent.is_dir() {
                        dirs.insert(parent_str);
                    }
                }
            }
        }

        dirs
    }
}
