use crate::migrate;
use crate::path_resolver::PathResolver;
use manifold_core::project::Project;
use std::io::Read;
use std::path::Path;

/// Load a .manifold project file with full post-load validation.
///
/// Supports both formats:
/// - V2: ZIP archive containing `project.json` (latest Unity format)
/// - V1: Plain JSON text file (legacy)
///
/// Detection: tries ZIP first; if the file isn't a valid ZIP, falls back to plain JSON.
///
/// Post-load validation (matches Unity ProjectSerializer.cs + ProjectArchive.cs):
/// 1. OnAfterDeserialize — rebuild caches, align params
/// 2. BPM sync from tempo map beat 0 (clamp 20-300)
/// 3. DurationMode migration — force all layers to NoteOff (V1 ONLY)
/// 4. PathResolver.ResolveAll — fix broken file paths
/// 5. Validate — structural integrity check
/// 6. ValidateClips — missing file detection
/// 7. PurgeOrphanedReferences — stale clip/MIDI cleanup
pub fn load_project(path: &Path) -> Result<Project, LoadError> {
    let file_bytes = std::fs::read(path).map_err(|e| LoadError::Io(e.to_string()))?;

    // Try V2 ZIP format first
    let (json, is_v2) = match extract_json_from_zip(&file_bytes) {
        Ok(json) => (json, true),
        Err(_) => {
            // Not a ZIP — treat as plain JSON (V1)
            let json = String::from_utf8(file_bytes)
                .map_err(|e| LoadError::Io(format!("Invalid UTF-8: {e}")))?;
            (json, false)
        }
    };

    let mut project = load_project_from_json(&json)?;

    // Step 3: Duration mode migration — V1 ONLY
    // Unity: ProjectSerializer.cs lines 46-50 (V1 path only)
    // Unity: ProjectArchive.cs Load() does NOT call this
    if !is_v2 {
        project.migrate_duration_modes();
        log::info!("[Loader] Loaded V1: {}", path.display());
    } else {
        log::info!("[Loader] Loaded V2: {}", path.display());
    }

    // Store the file path for PathResolver and save-back
    project.last_saved_path = path.to_string_lossy().to_string();

    // Step 4: Resolve broken file paths (migration support)
    // Unity: PathResolver.ResolveAll called in BOTH V1 (ProjectSerializer.cs line 55)
    // and V2 (ProjectArchive.cs line 98) load paths
    let saved_path = project.last_saved_path.clone();
    PathResolver::resolve_all(&mut project, &saved_path);

    // Post-load validation steps 5-7 (steps 1-2 done in load_project_from_json)
    run_post_load_validation(&mut project);

    Ok(project)
}

/// Extract `project.json` from a V2 ZIP archive.
fn extract_json_from_zip(bytes: &[u8]) -> Result<String, LoadError> {
    let cursor = std::io::Cursor::new(bytes);
    let mut archive =
        zip::ZipArchive::new(cursor).map_err(|e| LoadError::Io(format!("Not a ZIP: {e}")))?;

    // Look for project.json entry
    let mut entry = archive
        .by_name("project.json")
        .map_err(|e| LoadError::Io(format!("No project.json in archive: {e}")))?;

    let mut json = String::new();
    entry
        .read_to_string(&mut json)
        .map_err(|e| LoadError::Io(format!("Failed to read project.json: {e}")))?;

    Ok(json)
}

/// Load from raw JSON string. Runs steps 1-2 of post-load validation.
/// Steps 3-7 (duration mode migration, PathResolver, validate, validate_clips, purge)
/// are run by load_project after the file path is set. Callers using this directly
/// should call run_post_load_validation() separately.
pub fn load_project_from_json(json: &str) -> Result<Project, LoadError> {
    // Run version migration
    let migrated =
        migrate::migrate_if_needed(json).map_err(|e| LoadError::Migration(e.to_string()))?;

    // Deserialize
    let mut project: Project =
        serde_json::from_str(&migrated).map_err(|e| LoadError::Deserialize(format!("{e}")))?;

    // Strip unrecognized effect types (e.g. removed effects from Unity projects).
    // Without this, Unknown effects stay in the effect list and show in the UI.
    project.strip_unknown_effects();

    // Step 1: Rebuild runtime data structures
    project.on_after_deserialize();

    // Step 2: Sync BPM from tempo-map beat 0 (clamp 20-300)
    // on_after_deserialize already syncs BPM, but we clamp explicitly
    // to match Unity's Mathf.Clamp(startBpm, 20f, 300f)
    project.sync_bpm_from_tempo_map();

    Ok(project)
}

/// Run post-load validation steps 5-7: structural validation, missing file
/// detection, and orphaned reference cleanup.
/// Port of C# ProjectSerializer.cs lines 52-79 / ProjectArchive.cs lines 105-124.
pub fn run_post_load_validation(project: &mut Project) {
    // Step 5: Validate project structure
    let errors = project.validate();
    if !errors.is_empty() {
        log::warn!(
            "Project loaded with validation errors:\n{}",
            errors.join("\n")
        );
    }

    // Step 6: Validate video clips exist
    let validation = project.video_library.validate_clips();
    if !validation.is_valid() {
        log::warn!(
            "Project has {} missing video files:\n{}",
            validation.missing_files.len(),
            validation.missing_files.join("\n")
        );
    }

    // Step 7: Purge orphaned references
    let purge_result = project.purge_orphaned_references();
    if purge_result.total_removed() > 0 {
        log::info!(
            "[Loader] Cleaned {} orphaned timeline clips, {} stale MIDI mappings",
            purge_result.timeline_clips_removed,
            purge_result.midi_mappings_removed
        );
    }

    // Step 8: Populate clip.layer_id from structural ownership.
    // layer_id is skip_serializing (not persisted), so it deserializes as empty.
    // Stamp it here so runtime code that reads clip.layer_id gets the correct value.
    for layer in &mut project.timeline.layers {
        for clip in &mut layer.clips {
            clip.layer_id = layer.layer_id.clone();
        }
    }

    // Step 9.5: Reconcile generator identity. A generator running a
    // per-instance graph override carries its preset id twice — on the
    // instance (`generator_type`) and in the graph's `preset_metadata.id`.
    // Files saved while those desynced (graph names a real preset, instance
    // reports `None`) render fine but blank the inspector card and drop OSC
    // addressing. Mirror the graph's id back onto the instance.
    let mut reconciled = 0usize;
    for layer in &mut project.timeline.layers {
        if layer.reconcile_generator_identity() {
            reconciled += 1;
        }
    }
    if reconciled > 0 {
        log::info!(
            "[Loader] Reconciled {reconciled} generator(s) whose type id desynced from their graph"
        );
    }

    // Step 10: Repair pre-existing clip overlaps.
    // Projects saved before overlap enforcement was added to all mutation paths
    // may contain overlapping clips. Remove the shorter clip in each collision.
    repair_overlapping_clips(project);
}

/// Repair overlapping clips by removing the shorter clip in each collision pair.
/// Only needed for projects saved before overlap enforcement was complete.
fn repair_overlapping_clips(project: &mut Project) {
    let mut total_removed = 0usize;
    for layer in &mut project.timeline.layers {
        if !layer.has_overlapping_clips() {
            continue;
        }

        // Sort by start_beat, sweep to find overlaps, mark shorter clip for removal.
        // Iterate until no overlaps remain (removal may reveal new overlaps).
        loop {
            let mut remove_ids: Vec<manifold_core::ClipId> = Vec::new();
            let mut sorted: Vec<(usize, manifold_core::Beats, manifold_core::Beats)> = layer
                .clips
                .iter()
                .enumerate()
                .map(|(i, c)| (i, c.start_beat, c.end_beat()))
                .collect();
            sorted.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));

            for w in sorted.windows(2) {
                if w[0].2 > w[1].1 {
                    // Overlap: remove the shorter clip
                    let dur_a = w[0].2 - w[0].1;
                    let dur_b = w[1].2 - w[1].1;
                    let remove_idx = if dur_a <= dur_b { w[0].0 } else { w[1].0 };
                    remove_ids.push(layer.clips[remove_idx].id.clone());
                }
            }

            if remove_ids.is_empty() {
                break;
            }
            for id in &remove_ids {
                layer.remove_clip(id);
                total_removed += 1;
            }
        }
    }
    if total_removed > 0 {
        log::warn!(
            "[Loader] Repaired {} overlapping clips across timeline layers",
            total_removed
        );
        project.timeline.mark_clip_lookup_dirty();
    }
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
