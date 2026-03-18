use std::collections::HashSet;
use std::path::{Path, PathBuf};
use manifold_core::project::Project;

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

        // Resolve video clip paths
        for clip in &mut project.video_library.clips {
            if clip.file_path.is_empty() {
                continue;
            }

            if Path::new(&clip.file_path).exists() {
                result.already_valid_count += 1;
                continue;
            }

            let resolved = Self::try_resolve(
                &clip.file_path,
                clip.relative_file_path.as_deref(),
                clip.file_size,
                &project_dir,
                &search_dirs,
            );

            if let Some(resolved_path) = resolved {
                let relative = Self::make_relative(&resolved_path, &project_dir);
                clip.file_path = resolved_path;
                clip.relative_file_path = relative;
                result.resolved_count += 1;
            } else {
                result.unresolved_count += 1;
                result.unresolved.push(clip.file_path.clone());
            }
        }

        // Resolve layer video folder paths
        for layer in &mut project.timeline.layers {
            let folder_path = match &layer.video_folder_path {
                Some(p) if !p.is_empty() => p.clone(),
                _ => continue,
            };

            if Path::new(&folder_path).is_dir() {
                result.already_valid_count += 1;
                continue;
            }

            let resolved = Self::try_resolve_directory(
                &folder_path,
                layer.relative_video_folder_path.as_deref(),
                &project_dir,
                &search_dirs,
            );

            if let Some(resolved_path) = resolved {
                let relative = Self::make_relative(&resolved_path, &project_dir);
                layer.video_folder_path = Some(resolved_path);
                layer.relative_video_folder_path = relative;
                result.resolved_count += 1;
            } else {
                result.unresolved_count += 1;
                result.unresolved.push(folder_path);
            }
        }

        // Resolve percussion audio path
        if let Some(ref mut perc) = project.percussion_import {
            // Audio path
            if let Some(ref audio_path) = perc.audio_path.clone() {
                if !audio_path.is_empty() && !Path::new(audio_path).exists() {
                    let resolved = Self::try_resolve(
                        audio_path,
                        perc.relative_audio_path.as_deref(),
                        -1, // no size check for percussion audio
                        &project_dir,
                        &search_dirs,
                    );

                    if let Some(resolved_path) = resolved {
                        let relative = Self::make_relative(&resolved_path, &project_dir);
                        perc.audio_path = Some(resolved_path);
                        perc.relative_audio_path = relative;
                        result.resolved_count += 1;
                    } else {
                        result.unresolved_count += 1;
                        result.unresolved.push(audio_path.clone());
                    }
                }
            }

            // Resolve stem paths
            if let Some(ref mut stem_paths) = perc.stem_paths.clone() {
                let mut rel_stems = perc.relative_stem_paths.clone();
                let mut changed = false;

                for i in 0..stem_paths.len() {
                    let stem_path = &stem_paths[i];
                    if stem_path.is_empty() || Path::new(stem_path).exists() {
                        continue;
                    }

                    let rel_stem = rel_stems
                        .as_ref()
                        .and_then(|rs| rs.get(i))
                        .map(|s| s.as_str());

                    let resolved = Self::try_resolve(
                        stem_path, rel_stem, -1, &project_dir, &search_dirs,
                    );

                    if let Some(resolved_path) = resolved {
                        let relative = Self::make_relative(&resolved_path, &project_dir);
                        // Update stem_paths in place on the perc struct
                        if let Some(ref mut actual_stems) = perc.stem_paths {
                            actual_stems[i] = resolved_path;
                        }
                        if rel_stems.is_none() {
                            rel_stems = Some(vec![String::new(); stem_paths.len()]);
                        }
                        if let Some(ref mut rs) = rel_stems {
                            if i < rs.len() {
                                rs[i] = relative.unwrap_or_default();
                            }
                        }
                        changed = true;
                        result.resolved_count += 1;
                    } else {
                        result.unresolved_count += 1;
                        result.unresolved.push(stem_path.clone());
                    }
                }

                if changed {
                    perc.relative_stem_paths = rel_stems;
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
    pub fn store_relative_paths(project: &mut Project, project_dir: &str) {
        if project_dir.is_empty() {
            return;
        }

        // Video clips
        for clip in &mut project.video_library.clips {
            if clip.file_path.is_empty() {
                continue;
            }
            let relative = Self::make_relative(&clip.file_path, project_dir);
            clip.relative_file_path = relative;
        }

        // Layer video folder paths
        for layer in &mut project.timeline.layers {
            if let Some(ref folder_path) = layer.video_folder_path {
                if !folder_path.is_empty() {
                    layer.relative_video_folder_path =
                        Self::make_relative(folder_path, project_dir);
                }
            }
        }

        // Percussion
        if let Some(ref mut perc) = project.percussion_import {
            if let Some(ref audio_path) = perc.audio_path {
                if !audio_path.is_empty() {
                    perc.relative_audio_path = Self::make_relative(audio_path, project_dir);
                }
            }

            if let Some(ref stem_paths) = perc.stem_paths {
                let mut rel_stems = vec![String::new(); stem_paths.len()];
                for (i, stem_path) in stem_paths.iter().enumerate() {
                    if !stem_path.is_empty() {
                        if let Some(rel) = Self::make_relative(stem_path, project_dir) {
                            rel_stems[i] = rel;
                        }
                    }
                }
                perc.relative_stem_paths = Some(rel_stems);
            }
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
        if let Some(rel_path) = relative_path {
            if !rel_path.is_empty() && !project_dir.is_empty() {
                let candidate = PathBuf::from(project_dir).join(rel_path);
                if let Ok(canonical) = std::fs::canonicalize(&candidate) {
                    if canonical.exists() {
                        return Some(canonical.to_string_lossy().to_string());
                    }
                }
            }
        }

        // Step 2: Filename+size search in known directories
        let file_name = Path::new(absolute_path).file_name()?.to_string_lossy().to_string();
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

                if let Ok(metadata) = std::fs::metadata(&candidate) {
                    if metadata.len() as i64 == expected_file_size {
                        return Some(candidate.to_string_lossy().to_string());
                    }
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
        if let Some(rel_path) = relative_path {
            if !rel_path.is_empty() && !project_dir.is_empty() {
                let candidate = PathBuf::from(project_dir).join(rel_path);
                if let Ok(canonical) = std::fs::canonicalize(&candidate) {
                    if canonical.is_dir() {
                        return Some(canonical.to_string_lossy().to_string());
                    }
                }
            }
        }

        // Step 2: Search by folder name in known parent directories
        let trimmed = absolute_path
            .trim_end_matches(std::path::MAIN_SEPARATOR)
            .trim_end_matches('/');
        let folder_name = Path::new(trimmed).file_name()?.to_string_lossy().to_string();
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
        match pathdiff::diff_paths(abs, base) {
            Some(rel) => Some(rel.to_string_lossy().to_string()),
            None => None,
        }
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
                        if let Ok(ft) = entry.file_type() {
                            if ft.is_dir() {
                                dirs.insert(entry.path().to_string_lossy().to_string());
                            }
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

        // 4. Percussion audio directory
        if let Some(ref perc) = project.percussion_import {
            if let Some(ref audio_path) = perc.audio_path {
                if let Some(audio_dir) = Path::new(audio_path).parent() {
                    let dir_str = audio_dir.to_string_lossy().to_string();
                    if !dir_str.is_empty() && audio_dir.is_dir() {
                        dirs.insert(dir_str);
                    }
                }
            }
        }

        dirs
    }
}
