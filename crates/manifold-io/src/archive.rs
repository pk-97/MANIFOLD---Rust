use std::collections::HashSet;
use std::io::{Cursor, Read, Write};
use std::path::Path;

use sha2::{Digest, Sha256};
use zip::write::SimpleFileOptions;
use zip::{ZipArchive, ZipWriter};

use crate::manifest::{ProjectInfo, ProjectManifest, SnapshotEntry};

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
        && !directory.as_os_str().is_empty()
        && !directory.exists()
    {
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

    // Update the manifest BEFORE writing the zip: insert the current save,
    // apply the auto-save cap, and derive the set of history hashes that
    // survive. The copy below drops blobs for pruned entries, so the manifest
    // cap and the stored bytes stay in lockstep — without this, autosave
    // journaling would grow the archive unboundedly while the manifest lied
    // about it.
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

    let live_hashes: HashSet<String> = manifest.history.iter().map(|e| e.hash.clone()).collect();

    // Write to temp file, then rename for atomic save (Unity line 200).
    // The temp name is unique per writer (pid + nanos) so a background
    // autosave and a manual save hitting the same archive concurrently can
    // never collide on the temp file — each rename stays atomic on its own.
    let temp_path = format!(
        "{}.tmp.{}.{}",
        path,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or_default()
    );

    let write_result = (|| -> Result<(), String> {
        let file = std::fs::File::create(&temp_path)
            .map_err(|e| format!("Failed to create temp file: {e}"))?;
        let mut zip = ZipWriter::new(file);

        let options_no_compress =
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);

        // Copy existing history entries from old archive (Unity lines 186-190).
        // Entries pruned out of the manifest above are dropped here, keeping
        // the archive's byte size bounded by the auto-save cap.
        let copied_entries = if Path::new(path).exists() && is_v2_archive(path) {
            copy_history_entries(path, &mut zip, &live_hashes)?
        } else {
            HashSet::new()
        };

        // Push previous state to history (Unity lines 192-196).
        // Skip if already copied from the existing archive (duplicate entry)
        // or if the previous save was itself just pruned out of the manifest.
        if let (Some(prev_bytes), Some(prev_hash)) = (&previous_project_bytes, &previous_hash)
            && !prev_hash.is_empty()
            && live_hashes.contains(prev_hash.as_str())
        {
            let entry_name = format!("{}{}.json.gz", HISTORY_FOLDER, prev_hash);
            if !copied_entries.contains(&entry_name) {
                write_gzip_entry(&mut zip, &entry_name, prev_bytes)?;
            }
        }

        // Write current project.json — uncompressed for fast reads (Unity line 199)
        zip.start_file(PROJECT_ENTRY, options_no_compress)
            .map_err(|e| format!("Failed to start project entry: {e}"))?;
        zip.write_all(project_bytes)
            .map_err(|e| format!("Failed to write project entry: {e}"))?;

        // Write manifest.json (Unity lines 220-223)
        let manifest_json = serde_json::to_string_pretty(&manifest)
            .map_err(|e| format!("Failed to serialize manifest: {e}"))?;
        zip.start_file(MANIFEST_ENTRY, options_no_compress)
            .map_err(|e| format!("Failed to start manifest entry: {e}"))?;
        zip.write_all(manifest_json.as_bytes())
            .map_err(|e| format!("Failed to write manifest entry: {e}"))?;

        let file = zip
            .finish()
            .map_err(|e| format!("Failed to finish zip: {e}"))?;
        file.sync_all()
            .map_err(|e| format!("Failed to fsync temp file: {e}"))?;

        Ok(())
    })();

    match write_result {
        Ok(()) => {
            // Atomic rename: temp → final path (Unity lines 227-229)
            // On Unix, rename() atomically replaces the target — no remove needed.
            std::fs::rename(&temp_path, path)
                .map_err(|e| format!("Failed to rename temp file: {e}"))?;

            // fsync parent directory to ensure the rename is durable on disk.
            if let Some(parent) = Path::new(path).parent()
                && let Ok(dir) = std::fs::File::open(parent)
            {
                let _ = dir.sync_all();
            }

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

/// Read one history snapshot's project JSON out of a V2 archive by its
/// manifest hash. History entries are stored gzip-compressed at
/// `history/<hash>.json.gz` (see `save_v2_archive`); this gunzips and
/// returns the raw project JSON, ready for `loader::load_project_from_json`.
pub fn read_history_snapshot(path: &str, hash: &str) -> Result<String, String> {
    if hash.is_empty() {
        return Err("Snapshot hash cannot be empty".to_string());
    }
    let entry_name = format!("{}{}.json.gz", HISTORY_FOLDER, hash);
    let compressed = read_entry_bytes(path, &entry_name)
        .ok_or_else(|| format!("Snapshot {hash} not found in {path}"))?;

    let mut decoder = flate2::read::GzDecoder::new(Cursor::new(compressed));
    let mut json = String::new();
    decoder
        .read_to_string(&mut json)
        .map_err(|e| format!("Failed to gunzip snapshot {hash}: {e}"))?;
    Ok(json)
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
// INTERNAL HELPERS
// ──────────────────────────────────────

/// SHA-256 of data, returns first 16 hex chars.
/// Port of C# ProjectArchive.ComputeHash (lines 489-498); widened per
/// PROJECT_FILE_INTEGRITY_DESIGN D7.
fn compute_hash(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    let result = hasher.finalize();
    // Use first 8 bytes (16 hex chars, 64 bits — collision-negligible for a
    // bounded per-archive history).
    result[..8].iter().map(|b| format!("{b:02x}")).collect()
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
    encoder
        .write_all(data)
        .map_err(|e| format!("Failed to gzip data: {e}"))?;
    let compressed = encoder
        .finish()
        .map_err(|e| format!("Failed to finish gzip: {e}"))?;

    // Write as uncompressed ZIP entry (the entry itself is already gzipped)
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
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

/// Copy history/ entries from source archive to destination zip writer,
/// skipping entries whose hash is no longer in the (already-pruned) manifest —
/// pruned snapshots lose their blob, keeping archive size bounded.
/// Returns the set of entry names that were copied (used to avoid duplicates).
/// Port of C# ProjectArchive.CopyHistoryEntries (lines 547-564).
fn copy_history_entries<W: Write + std::io::Seek>(
    source_path: &str,
    dest_zip: &mut ZipWriter<W>,
    live_hashes: &HashSet<String>,
) -> Result<HashSet<String>, String> {
    let file_bytes =
        std::fs::read(source_path).map_err(|e| format!("Failed to read source archive: {e}"))?;
    let cursor = Cursor::new(&file_bytes);
    let mut source_archive =
        ZipArchive::new(cursor).map_err(|e| format!("Failed to open source archive: {e}"))?;

    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);

    let mut copied = HashSet::new();

    for i in 0..source_archive.len() {
        let mut entry = source_archive
            .by_index(i)
            .map_err(|e| format!("Failed to read entry {}: {}", i, e))?;
        let name = entry.name().to_string();

        if !name.starts_with(HISTORY_FOLDER) {
            continue;
        }

        // Drop blobs for entries pruned out of the manifest. Entry names are
        // `history/<hash>.json.gz` (see save); anything unparseable is kept —
        // never silently discard bytes we don't understand.
        if let Some(entry_hash) = name
            .strip_prefix(HISTORY_FOLDER)
            .and_then(|s| s.strip_suffix(".json.gz"))
            && !live_hashes.contains(entry_hash)
        {
            continue;
        }

        let mut data = Vec::new();
        entry
            .read_to_end(&mut data)
            .map_err(|e| format!("Failed to read entry data {}: {}", name, e))?;

        dest_zip
            .start_file(&name, options)
            .map_err(|e| format!("Failed to start dest entry {}: {}", name, e))?;
        dest_zip
            .write_all(&data)
            .map_err(|e| format!("Failed to write dest entry {}: {}", name, e))?;

        copied.insert(name);
    }

    Ok(copied)
}

/// Prune auto-save entries from the history list (in-place).
/// Keeps all manual saves and the `max_auto_saves` MOST RECENT auto-saves.
/// Returns the number of entries removed.
///
/// Port of C# ProjectArchive.PruneHistoryList (lines 571-590) — with the
/// iteration direction corrected for this struct's ordering. `history` is
/// newest-first (`save_v2_archive` inserts at index 0); the Unity port
/// counted autos from the END of the list, which on a newest-first list
/// kept the OLDEST autos and discarded the newest. Dead code until autosave
/// started writing `is_auto = true` entries; fixed when it went live.
fn prune_history_list(history: &mut Vec<SnapshotEntry>, max_auto_saves: usize) -> usize {
    let mut auto_count: usize = 0;
    let mut removed: usize = 0;

    // Walk newest → oldest; keep the first `max_auto_saves` autos we meet.
    let mut i = 0;
    while i < history.len() {
        if history[i].is_auto {
            auto_count += 1;
            if auto_count > max_auto_saves {
                history.remove(i);
                removed += 1;
                continue; // index now points at the next entry
            }
        }
        i += 1;
    }

    removed
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
