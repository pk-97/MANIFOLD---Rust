use std::collections::HashSet;
use std::io::{Cursor, Read, Write};
use std::path::Path;

use sha2::{Sha256, Digest};
use zip::write::SimpleFileOptions;
use zip::{ZipArchive, ZipWriter};

use crate::manifest::{ProjectManifest, SnapshotEntry, ProjectInfo};

/// V2 project archive constants.
/// Port of C# ProjectArchive.cs lines 19-22.
const MANIFEST_ENTRY: &str = "manifest.json";
const PROJECT_ENTRY: &str = "project.json";
const HISTORY_FOLDER: &str = "history/";
const DEFAULT_MAX_AUTO_SAVES: usize = 50;

// ──────────────────────────────────────
// FORMAT DETECTION
// ──────────────────────────────────────

/// Returns true if the file at `path` is a V2 zip archive containing a manifest.json entry.
/// Port of C# ProjectArchive.IsV2Archive (lines 32-46).
pub fn is_v2_archive(path: &str) -> bool {
    if path.is_empty() || !Path::new(path).exists() {
        return false;
    }

    let file_bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(_) => return false,
    };

    let cursor = Cursor::new(&file_bytes);
    match ZipArchive::new(cursor) {
        Ok(mut archive) => archive.by_name(MANIFEST_ENTRY).is_ok(),
        Err(_) => false,
    }
}

// ──────────────────────────────────────
// SAVE — V2 ZIP WRITER
// ──────────────────────────────────────

/// Save a project as a V2 zip archive. Creates snapshot history automatically.
/// Returns true on success.
/// Port of C# ProjectArchive.Save (lines 130-249).
///
/// `project_json` — the already-serialized JSON string of the project.
/// `project_name` — the project name for the manifest.
/// `path` — the file path to save to.
/// `label` — optional label for the snapshot entry.
/// `is_auto` — whether this is an auto-save.
pub fn save_v2_archive(
    project_json: &str,
    project_name: &str,
    path: &str,
    label: Option<&str>,
    is_auto: bool,
) -> Result<bool, String> {
    if path.is_empty() {
        return Err("File path cannot be empty".to_string());
    }

    // Create parent directory if needed (Unity line 139-141)
    if let Some(directory) = Path::new(path).parent()
        && !directory.as_os_str().is_empty() && !directory.exists() {
            std::fs::create_dir_all(directory)
                .map_err(|e| format!("Failed to create directory: {e}"))?;
        }

    let project_bytes = project_json.as_bytes();

    // Compute hash (Unity line 149)
    let hash = compute_hash(project_bytes);

    // Read existing manifest if file exists and is V2
    let mut manifest: Option<ProjectManifest> = None;
    let mut previous_hash: Option<String> = None;
    let mut previous_project_bytes: Option<Vec<u8>> = None;

    if Path::new(path).exists() && is_v2_archive(path) {
        manifest = read_manifest(path);
        previous_hash = manifest.as_ref().map(|m| m.current_hash.clone());

        // Dedup: if nothing changed, skip the write (Unity lines 153-160)
        if previous_hash.as_deref() == Some(&hash) {
            log::info!("[ProjectArchive] No changes detected, skipping save");
            return Ok(true);
        }

        // Read previous project.json for history
        previous_project_bytes = read_entry_bytes(path, PROJECT_ENTRY);
    }

    let mut manifest = manifest.unwrap_or_default();

    // Write to temp file, then rename for atomic save (Unity line 200)
    let temp_path = format!("{}.tmp", path);

    let write_result = (|| -> Result<(), String> {
        let file = std::fs::File::create(&temp_path)
            .map_err(|e| format!("Failed to create temp file: {e}"))?;
        let mut zip = ZipWriter::new(file);

        let options_no_compress = SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);

        // Copy existing history entries from old archive (Unity lines 186-190)
        if Path::new(path).exists() && is_v2_archive(path) {
            copy_history_entries(path, &mut zip)?;
        }

        // Push previous state to history (Unity lines 192-196)
        if let (Some(prev_bytes), Some(prev_hash)) =
            (&previous_project_bytes, &previous_hash)
            && !prev_hash.is_empty() {
                let entry_name = format!("{}{}.json.gz", HISTORY_FOLDER, prev_hash);
                write_gzip_entry(&mut zip, &entry_name, prev_bytes)?;
            }

        // Write current project.json — uncompressed for fast reads (Unity line 199)
        zip.start_file(PROJECT_ENTRY, options_no_compress)
            .map_err(|e| format!("Failed to start project entry: {e}"))?;
        zip.write_all(project_bytes)
            .map_err(|e| format!("Failed to write project entry: {e}"))?;

        // Update manifest (Unity lines 202-215)
        let now = chrono_now_iso8601();
        manifest.format_version = 2;
        manifest.name = project_name.to_string();
        manifest.current_hash = hash.clone();
        manifest.saved_at = now.clone();

        // Add current save to history (Unity line 209)
        manifest.history.insert(
            0,
            SnapshotEntry {
                hash: hash.clone(),
                timestamp: now,
                label: label.map(|l| l.to_string()),
                is_auto,
            },
        );

        // Prune auto-saves (Unity line 218)
        prune_history_list(&mut manifest.history, DEFAULT_MAX_AUTO_SAVES);

        // Write manifest.json (Unity lines 220-223)
        let manifest_json = serde_json::to_string_pretty(&manifest)
            .map_err(|e| format!("Failed to serialize manifest: {e}"))?;
        zip.start_file(MANIFEST_ENTRY, options_no_compress)
            .map_err(|e| format!("Failed to start manifest entry: {e}"))?;
        zip.write_all(manifest_json.as_bytes())
            .map_err(|e| format!("Failed to write manifest entry: {e}"))?;

        zip.finish()
            .map_err(|e| format!("Failed to finish zip: {e}"))?;

        Ok(())
    })();

    match write_result {
        Ok(()) => {
            // Atomic rename: temp → final path (Unity lines 227-229)
            if Path::new(path).exists() {
                std::fs::remove_file(path)
                    .map_err(|e| format!("Failed to remove old file: {e}"))?;
            }
            std::fs::rename(&temp_path, path)
                .map_err(|e| format!("Failed to rename temp file: {e}"))?;

            log::info!("[ProjectArchive] Saved V2: {}", path);
            Ok(true)
        }
        Err(e) => {
            // Clean up temp file on failure
            let _ = std::fs::remove_file(&temp_path);
            Err(format!("[ProjectArchive] Failed to save: {e}"))
        }
    }
}

// ──────────────────────────────────────
// FAST METADATA
// ──────────────────────────────────────

/// Read only the manifest from a V2 archive. Fast — does not deserialize the project.
/// Port of C# ProjectArchive.ReadManifest (lines 258-279).
pub fn read_manifest(path: &str) -> Option<ProjectManifest> {
    if !Path::new(path).exists() {
        return None;
    }

    let file_bytes = std::fs::read(path).ok()?;
    let cursor = Cursor::new(&file_bytes);
    let mut archive = ZipArchive::new(cursor).ok()?;
    let mut entry = archive.by_name(MANIFEST_ENTRY).ok()?;
    let mut json = String::new();
    entry.read_to_string(&mut json).ok()?;
    serde_json::from_str(&json).ok()
}

/// Check if a file is a valid V2 project file. Fast — reads manifest only.
/// Port of C# ProjectArchive.IsValidProjectFile (lines 284-288).
pub fn is_valid_project_file(path: &str) -> bool {
    match read_manifest(path) {
        Some(manifest) => manifest.format_version >= 2,
        None => false,
    }
}

/// Get project info without fully loading. Fast — reads manifest only.
/// Port of C# ProjectArchive.GetProjectInfo (lines 293-307).
pub fn get_project_info(path: &str) -> Option<ProjectInfo> {
    let manifest = read_manifest(path)?;

    let metadata = std::fs::metadata(path).ok()?;
    let last_modified = metadata.modified().ok()?;

    Some(ProjectInfo {
        project_name: manifest.name,
        project_version: format!("V{}", manifest.format_version),
        file_path: path.to_string(),
        file_size: metadata.len(),
        last_modified,
    })
}

// ──────────────────────────────────────
// HISTORY OPERATIONS
// ──────────────────────────────────────

/// Get the snapshot history for a project file.
/// Port of C# ProjectArchive.GetHistory (lines 316-320).
pub fn get_history(path: &str) -> Vec<SnapshotEntry> {
    match read_manifest(path) {
        Some(manifest) => manifest.history,
        None => Vec::new(),
    }
}

/// Revert to a previous snapshot. Pushes current state to history first
/// so the revert is itself revertable.
/// Port of C# ProjectArchive.RevertTo (lines 326-414).
pub fn revert_to(path: &str, hash: &str) -> bool {
    if path.is_empty() || !Path::new(path).exists() || hash.is_empty() {
        return false;
    }

    let manifest = match read_manifest(path) {
        Some(m) => m,
        None => return false,
    };

    // Already at this hash
    if manifest.current_hash == hash {
        return true;
    }

    let history_entry_name = format!("{}{}.json.gz", HISTORY_FOLDER, hash);

    // Read the target snapshot
    let snapshot_bytes = match read_gzip_entry_bytes(path, &history_entry_name) {
        Some(b) => b,
        None => {
            log::error!("[ProjectArchive] Snapshot not found: {}", hash);
            return false;
        }
    };

    // Read current project.json to push to history
    let current_bytes = read_entry_bytes(path, PROJECT_ENTRY);
    let current_hash = manifest.current_hash.clone();

    let temp_path = format!("{}.tmp", path);

    let write_result = (|| -> Result<(), String> {
        let file = std::fs::File::create(&temp_path)
            .map_err(|e| format!("Failed to create temp file: {e}"))?;
        let mut zip = ZipWriter::new(file);
        let options = SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);

        // Copy all existing history entries
        copy_history_entries(path, &mut zip)?;

        // Push current state to history (if not already there as a file)
        if let Some(ref cur_bytes) = current_bytes
            && !current_hash.is_empty() {
                let current_history_entry = format!("{}{}.json.gz", HISTORY_FOLDER, current_hash);
                if !history_entry_exists(path, &current_history_entry) {
                    write_gzip_entry(&mut zip, &current_history_entry, cur_bytes)?;
                }
            }

        // Write reverted snapshot as current project.json
        zip.start_file(PROJECT_ENTRY, options)
            .map_err(|e| format!("Failed to start project entry: {e}"))?;
        zip.write_all(&snapshot_bytes)
            .map_err(|e| format!("Failed to write project entry: {e}"))?;

        // Update manifest
        let now = chrono_now_iso8601();
        let mut manifest = manifest.clone();
        manifest.current_hash = hash.to_string();
        manifest.saved_at = now.clone();
        manifest.history.insert(
            0,
            SnapshotEntry {
                hash: hash.to_string(),
                timestamp: now,
                label: Some(format!("Reverted to {}", hash)),
                is_auto: false,
            },
        );

        let manifest_json = serde_json::to_string_pretty(&manifest)
            .map_err(|e| format!("Failed to serialize manifest: {e}"))?;
        zip.start_file(MANIFEST_ENTRY, options)
            .map_err(|e| format!("Failed to start manifest entry: {e}"))?;
        zip.write_all(manifest_json.as_bytes())
            .map_err(|e| format!("Failed to write manifest entry: {e}"))?;

        zip.finish()
            .map_err(|e| format!("Failed to finish zip: {e}"))?;

        Ok(())
    })();

    match write_result {
        Ok(()) => {
            let _ = std::fs::remove_file(path);
            if std::fs::rename(&temp_path, path).is_err() {
                return false;
            }
            log::info!("[ProjectArchive] Reverted to snapshot: {}", hash);
            true
        }
        Err(e) => {
            log::error!("[ProjectArchive] Revert failed: {}", e);
            let _ = std::fs::remove_file(&temp_path);
            false
        }
    }
}

/// Add or update a label on a snapshot in the history.
/// Port of C# ProjectArchive.LabelSnapshot (lines 419-452).
pub fn label_snapshot(path: &str, hash: &str, label: &str) -> bool {
    if path.is_empty() || !Path::new(path).exists() {
        return false;
    }

    let mut manifest = match read_manifest(path) {
        Some(m) => m,
        None => return false,
    };

    let mut found = false;
    for entry in &mut manifest.history {
        if entry.hash == hash {
            entry.label = Some(label.to_string());
            found = true;
            break;
        }
    }

    if !found {
        return false;
    }

    rewrite_manifest(path, &manifest)
}

/// Prune old auto-save snapshots beyond the given limit.
/// Returns the number of entries removed.
/// Port of C# ProjectArchive.PruneHistory (lines 458-483).
pub fn prune_history(path: &str, max_auto_saves: Option<usize>) -> usize {
    let max = max_auto_saves.unwrap_or(DEFAULT_MAX_AUTO_SAVES);

    if path.is_empty() || !Path::new(path).exists() {
        return 0;
    }

    let mut manifest = match read_manifest(path) {
        Some(m) => m,
        None => return 0,
    };

    let removed = prune_history_list(&mut manifest.history, max);
    if removed > 0 {
        rebuild_archive(path, &manifest);
    }

    removed
}

// ──────────────────────────────────────
// INTERNAL HELPERS
// ──────────────────────────────────────

/// SHA-256 of data, returns first 6 hex chars.
/// Port of C# ProjectArchive.ComputeHash (lines 489-498).
fn compute_hash(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    let result = hasher.finalize();
    // Use first 3 bytes (6 hex chars, 24 bits — 16M+ unique values)
    format!("{:02x}{:02x}{:02x}", result[0], result[1], result[2])
}

/// Write an uncompressed ZIP entry.
/// Port of C# ProjectArchive.WriteEntry (lines 500-505).
#[allow(dead_code)]
fn write_entry<W: Write + std::io::Seek>(
    zip: &mut ZipWriter<W>,
    entry_name: &str,
    data: &[u8],
) -> Result<(), String> {
    let options = SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Stored);
    zip.start_file(entry_name, options)
        .map_err(|e| format!("Failed to start entry {}: {}", entry_name, e))?;
    zip.write_all(data)
        .map_err(|e| format!("Failed to write entry {}: {}", entry_name, e))?;
    Ok(())
}

/// Write a gzip-compressed ZIP entry.
/// Port of C# ProjectArchive.WriteGzipEntry (lines 507-513).
fn write_gzip_entry<W: Write + std::io::Seek>(
    zip: &mut ZipWriter<W>,
    entry_name: &str,
    data: &[u8],
) -> Result<(), String> {
    // Gzip-compress the data
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    encoder.write_all(data)
        .map_err(|e| format!("Failed to gzip data: {e}"))?;
    let compressed = encoder.finish()
        .map_err(|e| format!("Failed to finish gzip: {e}"))?;

    // Write as uncompressed ZIP entry (the entry itself is already gzipped)
    let options = SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Stored);
    zip.start_file(entry_name, options)
        .map_err(|e| format!("Failed to start entry {}: {}", entry_name, e))?;
    zip.write_all(&compressed)
        .map_err(|e| format!("Failed to write entry {}: {}", entry_name, e))?;
    Ok(())
}

/// Read raw ZIP entry bytes.
/// Port of C# ProjectArchive.ReadEntryBytes (lines 515-526).
fn read_entry_bytes(archive_path: &str, entry_name: &str) -> Option<Vec<u8>> {
    let file_bytes = std::fs::read(archive_path).ok()?;
    let cursor = Cursor::new(&file_bytes);
    let mut archive = ZipArchive::new(cursor).ok()?;
    let mut entry = archive.by_name(entry_name).ok()?;
    let mut data = Vec::new();
    entry.read_to_end(&mut data).ok()?;
    Some(data)
}

/// Read and decompress a gzip ZIP entry.
/// Port of C# ProjectArchive.ReadGzipEntryBytes (lines 528-539).
fn read_gzip_entry_bytes(archive_path: &str, entry_name: &str) -> Option<Vec<u8>> {
    let compressed = read_entry_bytes(archive_path, entry_name)?;
    let mut decoder = flate2::read::GzDecoder::new(Cursor::new(&compressed));
    let mut decompressed = Vec::new();
    decoder.read_to_end(&mut decompressed).ok()?;
    Some(decompressed)
}

/// Check if a history entry exists in the archive.
/// Port of C# ProjectArchive.HistoryEntryExists (lines 541-545).
fn history_entry_exists(archive_path: &str, entry_name: &str) -> bool {
    let file_bytes = match std::fs::read(archive_path) {
        Ok(b) => b,
        Err(_) => return false,
    };
    let cursor = Cursor::new(&file_bytes);
    match ZipArchive::new(cursor) {
        Ok(mut archive) => archive.by_name(entry_name).is_ok(),
        Err(_) => false,
    }
}

/// Copy all history/ entries from source archive to destination zip writer.
/// Port of C# ProjectArchive.CopyHistoryEntries (lines 547-564).
fn copy_history_entries<W: Write + std::io::Seek>(
    source_path: &str,
    dest_zip: &mut ZipWriter<W>,
) -> Result<(), String> {
    let file_bytes = std::fs::read(source_path)
        .map_err(|e| format!("Failed to read source archive: {e}"))?;
    let cursor = Cursor::new(&file_bytes);
    let mut source_archive = ZipArchive::new(cursor)
        .map_err(|e| format!("Failed to open source archive: {e}"))?;

    let options = SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Stored);

    for i in 0..source_archive.len() {
        let mut entry = source_archive.by_index(i)
            .map_err(|e| format!("Failed to read entry {}: {}", i, e))?;
        let name = entry.name().to_string();

        if !name.starts_with(HISTORY_FOLDER) {
            continue;
        }

        let mut data = Vec::new();
        entry.read_to_end(&mut data)
            .map_err(|e| format!("Failed to read entry data {}: {}", name, e))?;

        dest_zip.start_file(&name, options)
            .map_err(|e| format!("Failed to start dest entry {}: {}", name, e))?;
        dest_zip.write_all(&data)
            .map_err(|e| format!("Failed to write dest entry {}: {}", name, e))?;
    }

    Ok(())
}

/// Prune auto-save entries from the history list (in-place).
/// Keeps all manual saves and up to `max_auto_saves` auto-saves.
/// Returns the number of entries removed.
/// Port of C# ProjectArchive.PruneHistoryList (lines 571-590).
fn prune_history_list(history: &mut Vec<SnapshotEntry>, max_auto_saves: usize) -> usize {
    let mut auto_count: usize = 0;
    let mut removed: usize = 0;

    // Iterate from end to beginning (oldest first), matching Unity
    let mut i = history.len();
    while i > 0 {
        i -= 1;
        if !history[i].is_auto {
            continue;
        }
        auto_count += 1;
        if auto_count > max_auto_saves {
            history.remove(i);
            removed += 1;
        }
    }

    removed
}

/// Rewrite the archive with an updated manifest (project.json and history unchanged).
/// Port of C# ProjectArchive.RewriteManifest (lines 592-626).
fn rewrite_manifest(path: &str, manifest: &ProjectManifest) -> bool {
    let temp_path = format!("{}.tmp", path);

    let result = (|| -> Result<(), String> {
        let file_bytes = std::fs::read(path)
            .map_err(|e| format!("Failed to read source: {e}"))?;
        let cursor = Cursor::new(&file_bytes);
        let mut source_archive = ZipArchive::new(cursor)
            .map_err(|e| format!("Failed to open source: {e}"))?;

        let dest_file = std::fs::File::create(&temp_path)
            .map_err(|e| format!("Failed to create temp: {e}"))?;
        let mut zip = ZipWriter::new(dest_file);
        let options = SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);

        // Copy everything except old manifest
        for i in 0..source_archive.len() {
            let mut entry = source_archive.by_index(i)
                .map_err(|e| format!("Failed to read entry: {e}"))?;
            let name = entry.name().to_string();

            if name == MANIFEST_ENTRY {
                continue; // Skip old manifest
            }

            let mut data = Vec::new();
            entry.read_to_end(&mut data)
                .map_err(|e| format!("Failed to read entry data: {e}"))?;

            zip.start_file(&name, options)
                .map_err(|e| format!("Failed to start entry: {e}"))?;
            zip.write_all(&data)
                .map_err(|e| format!("Failed to write entry: {e}"))?;
        }

        // Write updated manifest
        let manifest_json = serde_json::to_string_pretty(manifest)
            .map_err(|e| format!("Failed to serialize manifest: {e}"))?;
        zip.start_file(MANIFEST_ENTRY, options)
            .map_err(|e| format!("Failed to start manifest: {e}"))?;
        zip.write_all(manifest_json.as_bytes())
            .map_err(|e| format!("Failed to write manifest: {e}"))?;

        zip.finish()
            .map_err(|e| format!("Failed to finish zip: {e}"))?;

        Ok(())
    })();

    match result {
        Ok(()) => {
            let _ = std::fs::remove_file(path);
            std::fs::rename(&temp_path, path).is_ok()
        }
        Err(_) => {
            let _ = std::fs::remove_file(&temp_path);
            false
        }
    }
}

/// Rebuild archive removing pruned history files not in manifest.History.
/// Port of C# ProjectArchive.RebuildArchive (lines 628-675).
fn rebuild_archive(path: &str, manifest: &ProjectManifest) {
    // Collect hashes that should be kept
    let keep_hashes: HashSet<&str> = manifest.history.iter().map(|e| e.hash.as_str()).collect();

    let temp_path = format!("{}.tmp", path);

    let result = (|| -> Result<(), String> {
        let file_bytes = std::fs::read(path)
            .map_err(|e| format!("Failed to read source: {e}"))?;
        let cursor = Cursor::new(&file_bytes);
        let mut source_archive = ZipArchive::new(cursor)
            .map_err(|e| format!("Failed to open source: {e}"))?;

        let dest_file = std::fs::File::create(&temp_path)
            .map_err(|e| format!("Failed to create temp: {e}"))?;
        let mut zip = ZipWriter::new(dest_file);
        let options = SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);

        for i in 0..source_archive.len() {
            let mut entry = source_archive.by_index(i)
                .map_err(|e| format!("Failed to read entry: {e}"))?;
            let name = entry.name().to_string();

            // Skip old manifest
            if name == MANIFEST_ENTRY {
                continue;
            }

            // For history entries, only keep those still in the manifest
            if name.starts_with(HISTORY_FOLDER) {
                // Strip "history/" prefix and ".json.gz" suffix to get hash
                let hash = name
                    .strip_prefix(HISTORY_FOLDER)
                    .unwrap_or("")
                    .strip_suffix(".json.gz")
                    .unwrap_or("");
                if !keep_hashes.contains(hash) {
                    continue;
                }
            }

            let mut data = Vec::new();
            entry.read_to_end(&mut data)
                .map_err(|e| format!("Failed to read entry data: {e}"))?;

            zip.start_file(&name, options)
                .map_err(|e| format!("Failed to start entry: {e}"))?;
            zip.write_all(&data)
                .map_err(|e| format!("Failed to write entry: {e}"))?;
        }

        // Write updated manifest
        let manifest_json = serde_json::to_string_pretty(manifest)
            .map_err(|e| format!("Failed to serialize manifest: {e}"))?;
        zip.start_file(MANIFEST_ENTRY, options)
            .map_err(|e| format!("Failed to start manifest: {e}"))?;
        zip.write_all(manifest_json.as_bytes())
            .map_err(|e| format!("Failed to write manifest: {e}"))?;

        zip.finish()
            .map_err(|e| format!("Failed to finish zip: {e}"))?;

        Ok(())
    })();

    if result.is_ok() {
        let _ = std::fs::remove_file(path);
        let _ = std::fs::rename(&temp_path, path);
    } else {
        let _ = std::fs::remove_file(&temp_path);
    }
}

/// Generate ISO 8601 timestamp (UTC) matching Unity's DateTime.UtcNow.ToString("o").
fn chrono_now_iso8601() -> String {
    // Use std::time to avoid adding chrono dependency
    // Format: 2026-03-18T12:00:00.000Z (simplified ISO 8601)
    let now = std::time::SystemTime::now();
    let duration = now
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = duration.as_secs();

    // Simple UTC time calculation
    let days = secs / 86400;
    let time_of_day = secs % 86400;
    let hours = time_of_day / 3600;
    let minutes = (time_of_day % 3600) / 60;
    let seconds = time_of_day % 60;
    let millis = duration.subsec_millis();

    // Days since epoch to date (simplified Gregorian)
    let mut y = 1970i64;
    let mut remaining_days = days as i64;

    loop {
        let days_in_year = if is_leap_year(y) { 366 } else { 365 };
        if remaining_days < days_in_year {
            break;
        }
        remaining_days -= days_in_year;
        y += 1;
    }

    let month_days = if is_leap_year(y) {
        [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    } else {
        [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    };

    let mut m = 0;
    for (i, &md) in month_days.iter().enumerate() {
        if remaining_days < md as i64 {
            m = i;
            break;
        }
        remaining_days -= md as i64;
    }

    let d = remaining_days + 1;

    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        y,
        m + 1,
        d,
        hours,
        minutes,
        seconds,
        millis
    )
}

fn is_leap_year(y: i64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}
