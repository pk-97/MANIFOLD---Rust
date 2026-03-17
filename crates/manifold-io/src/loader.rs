use std::path::Path;
use std::io::Read;
use manifold_core::project::Project;
use crate::migrate;

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
/// 3. DurationMode migration — force all layers to NoteOff
/// 4. Validate — structural integrity check
/// 5. ValidateClips — missing file detection
/// 6. PurgeOrphanedReferences — stale clip/MIDI cleanup
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

    let mut project = load_project_from_json(&json)?;

    // Store the file path for PathResolver and save-back
    project.last_saved_path = path.to_string_lossy().to_string();

    // Post-load validation steps 4-6 (steps 1-3 done in load_project_from_json)
    run_post_load_validation(&mut project);

    Ok(project)
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

/// Load from raw JSON string. Runs steps 1-3 of post-load validation.
/// Steps 4-6 (validate, validate_clips, purge) are run by load_project after
/// the file path is set. Callers using this directly should call
/// run_post_load_validation() separately.
pub fn load_project_from_json(json: &str) -> Result<Project, LoadError> {
    // Run version migration
    let migrated = migrate::migrate_if_needed(json)
        .map_err(|e| LoadError::Migration(e.to_string()))?;

    // Deserialize
    let mut project: Project = serde_json::from_str(&migrated)
        .map_err(|e| LoadError::Deserialize(format!("{e}")))?;

    // Step 1: Rebuild runtime data structures
    project.on_after_deserialize();

    // Step 2: Sync BPM from tempo-map beat 0 (clamp 20-300)
    // on_after_deserialize already syncs BPM, but we clamp explicitly
    // to match Unity's Mathf.Clamp(startBpm, 20f, 300f)
    project.sync_bpm_from_tempo_map();

    // Step 3: Migrate old projects: force all layers to NoteOff duration mode
    project.migrate_duration_modes();

    Ok(project)
}

/// Run post-load validation steps 4-6: structural validation, missing file
/// detection, and orphaned reference cleanup.
/// Port of C# ProjectSerializer.cs lines 52-79 / ProjectArchive.cs lines 105-124.
pub fn run_post_load_validation(project: &mut Project) {
    // Step 4: Validate project structure
    let errors = project.validate();
    if !errors.is_empty() {
        log::warn!(
            "Project loaded with validation errors:\n{}",
            errors.join("\n")
        );
    }

    // Step 5: Validate video clips exist
    let validation = project.video_library.validate_clips();
    if !validation.is_valid() {
        log::warn!(
            "Project has {} missing video files:\n{}",
            validation.missing_files.len(),
            validation.missing_files.join("\n")
        );
    }

    // Step 6: Purge orphaned references
    let purge_result = project.purge_orphaned_references();
    if purge_result.total_removed() > 0 {
        log::info!(
            "[Loader] Cleaned {} orphaned timeline clips, {} stale MIDI mappings",
            purge_result.timeline_clips_removed,
            purge_result.midi_mappings_removed
        );
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
