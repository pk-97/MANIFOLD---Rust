# Tier 2 Fix Plan: `manifold-io` Parity Remediation

**Status: COMPLETE** — Implemented 2026-03-18, commit `5641ff9`

**Generated:** 2026-03-18 from line-by-line audit of all Unity Export/*.cs + Data/PathResolver.cs against Rust manifold-io/src/*.rs

**Methodology:** Every fix below references the exact Unity source file and line numbers. The implementing agent MUST read the Unity source — not this document — as the source of truth. This plan tells you WHAT to fix and WHERE to look, not HOW the code should read.

**Dependency:** Tier 0 fixes should be completed first (especially `Project.validate()` completion and `RecordingProvenance.ensure_valid()`).

---

## Phase 1: PathResolver — Cross-Machine Project Portability (CRITICAL)

### 1A. Port `PathResolver` class

**Unity source:** `Data/PathResolver.cs` (~380 lines)
**Rust file:** New file `crates/manifold-io/src/path_resolver.rs`

This is the MOST CRITICAL missing piece in manifold-io. It is called by BOTH the loader and saver and enables projects to survive file/directory moves.

**Port the entire class:**

| Method | Unity Lines | Description |
|--------|------------|-------------|
| `ResolveAll(project, projectFilePath)` | entry point | Resolves broken paths for video clips, layer folder paths, percussion audio |
| `StoreRelativePaths(project, projectDirectory)` | called before save | Populates relative paths on all video clips, layer folder paths, percussion audio |
| `TryResolve(absolutePath, relativePath, searchDirs)` | resolution chain | Try absolute → try relative from project dir → filename+size search |
| `TryResolveDirectory(absolutePath, relativePath, searchDirs)` | directory resolution | Similar chain for directories |
| `MakeRelative(absolutePath, basePath)` | utility | Absolute-to-relative path conversion |
| `BuildSearchDirs(project, projectDir)` | utility | Builds set of directories to search (project dir, parent dir, layer video folders, percussion audio dir) |

**Data model:** `PathResolutionResult` struct — resolution statistics (resolved count, unresolved count, search dirs used).

**Integration points (add after porting the class):**
1. `loader.rs` — Call `PathResolver::resolve_all()` AFTER deserialization + BPM sync but BEFORE validate/validate_clips/purge (matching Unity's `ProjectSerializer.cs` line 55 and `ProjectArchive.cs` line 98)
2. `saver.rs` — Call `PathResolver::store_relative_paths()` BEFORE serialization (matching Unity's `ProjectArchive.Save()` line 144)

---

## Phase 2: V2 Archive Format — ZIP + Manifest + History (CRITICAL)

### 2A. Port `ProjectManifest` and `SnapshotEntry` data models

**Unity source:** `Export/ProjectManifest.cs` lines 13-47
**Rust file:** New file `crates/manifold-io/src/manifest.rs`

```
ProjectManifest:
  format_version: i32 = 2
  name: String
  current_hash: String
  saved_at: String (ISO 8601)
  history: Vec<SnapshotEntry>

SnapshotEntry:
  hash: String
  timestamp: String (ISO 8601)
  label: Option<String>
  is_auto: bool
```

### 2B. Port `ProjectArchive.Save()` — V2 ZIP writer

**Unity source:** `Export/ProjectArchive.cs` lines 130-249
**Rust file:** Rewrite `crates/manifold-io/src/saver.rs` or add new `archive.rs`

**Port these methods in order:**

| Method | Unity Lines | Description |
|--------|------------|-------------|
| `ComputeHash(json)` | 489-497 | SHA-256, first 6 hex chars |
| `WriteEntry(archive, name, data)` | helper | Write uncompressed ZIP entry |
| `WriteGzipEntry(archive, name, data)` | helper | Write gzip-compressed ZIP entry |
| `CopyHistoryEntries(oldArchive, manifest)` | ~250-280 | Copy existing history from old archive to new |
| `PruneHistoryList(manifest, maxAutoSaves)` | ~280-310 | Remove oldest auto-saves beyond cap (default 50) |
| `Save(project, path, label)` | 130-249 | Full V2 save flow |

**Save flow (from Unity lines 130-249):**
1. Create parent directory if needed (line 139-141)
2. Call `PathResolver.StoreRelativePaths()` (line 144)
3. Serialize project to JSON (line 147)
4. Compute SHA-256 hash (line 149)
5. **Change detection:** If old archive exists and hash matches current, skip write (lines 153-160)
6. Build manifest with history from old archive (lines 163-190)
7. Add current snapshot to history (line 192)
8. Prune auto-saves beyond limit (line 194)
9. Write to temp file `.tmp` (line 200)
10. Write `manifest.json` entry (line 205)
11. Write `project.json` entry — uncompressed (line 208)
12. Write `history/*.json.gz` entries — gzipped (lines 210-220)
13. Close archive (line 222)
14. Atomic rename: temp → final path (line 225)
15. Update `project.last_saved_path = path` (line 231)
16. Log success (line 233)

**Constants (Unity `ProjectArchive.cs` lines 19-22):**
- `MANIFEST_ENTRY = "manifest.json"`
- `PROJECT_ENTRY = "project.json"`
- `HISTORY_FOLDER = "history/"`
- `DEFAULT_MAX_AUTO_SAVES = 50`

**Rust crate dependency:** Add `zip` crate (for ZIP read/write), `sha2` crate (for SHA-256), `flate2` crate (for gzip).

### 2C. Update `loader.rs` — V2 manifest-aware loading

**Unity source:** `Export/ProjectArchive.cs` lines 50-120
**Rust file:** `crates/manifold-io/src/loader.rs`

Current Rust loader detects V2 by checking for `project.json` in the ZIP. Unity detects V2 by checking for `manifest.json` (`IsV2Archive()`, lines 32-46).

**Fix:**
1. Add `is_v2_archive()` that checks for `manifest.json` (matching Unity line 38)
2. Read and deserialize `manifest.json` on V2 load (for future use by history features)
3. Ensure `PathResolver::resolve_all()` is called in the V2 load path (matching Unity line 98)

### 2D. Port V2 archive utility methods

**Unity source:** `Export/ProjectArchive.cs` lines 250-500+
**Rust file:** `crates/manifold-io/src/archive.rs`

| Method | Unity Lines | Description |
|--------|------------|-------------|
| `ReadManifest(path)` | ~260-275 | Fast manifest read without loading full project |
| `IsValidProjectFile(path)` | ~280-295 | Format validation (V1 JSON or V2 ZIP) |
| `GetProjectInfo(path)` | ~300-330 | Lightweight project info (name, version, size, date) |
| `GetHistory(path)` | ~335-350 | Get snapshot history list |
| `RevertTo(path, hash)` | ~326-414 | Revert project to a previous snapshot |
| `LabelSnapshot(path, hash, label)` | ~420-450 | Label a history entry |
| `PruneHistory(path, keepCount)` | ~455-475 | Explicit history pruning |
| `RewriteManifest(path, manifest)` | ~478-488 | Update manifest without changing project |
| `RebuildArchive(path)` | ~500-530 | Rebuild archive removing pruned entries |

Also port internal helpers:
- `ReadEntryBytes(archive, entryName)` — read raw ZIP entry
- `ReadGzipEntryBytes(archive, entryName)` — read and decompress gzip entry
- `HistoryEntryExists(archive, hash)` — check if history entry present

### 2E. Port `ProjectInfo` struct

**Unity source:** `Export/ProjectSerializer.cs` lines 108-120
**Rust file:** `crates/manifold-io/src/manifest.rs` or `project_info.rs`

```
ProjectInfo:
  project_name: String
  project_version: String
  file_path: String
  file_size: u64
  last_modified: String (or SystemTime)
```

With `Display` impl matching Unity's `ToString()` override.

---

## Phase 3: Loader Fixes

### 3A. Fix DurationMode migration scope

**Unity source:** `ProjectSerializer.cs` line 46-49 — duration mode migration runs ONLY on V1 loads
**Rust file:** `loader.rs` line 88

**Bug:** Rust calls `project.migrate_duration_modes()` in `load_project_from_json()` which runs for BOTH V1 and V2 loads. Unity only does this in the V1 path.

**Fix:** Move the duration mode migration to the V1-specific load path only. The V2 path should not call it.

### 3B. Add V1 success log

**Unity source:** `ProjectSerializer.cs` line 73
**Rust file:** `loader.rs`

Add `log::info!("[Loader] Loaded V1: {}", path)` after successful V1 load, matching Unity's log format.

---

## Phase 4: Saver Fixes

### 4A. Update `last_saved_path` after save

**Unity source:** `ProjectArchive.Save()` line 231
**Rust file:** `saver.rs`

**Bug:** Rust never updates `project.last_saved_path` after saving. Unity sets `project.LastSavedPath = path` after successful save.

**Fix:** After successful write, set `project.last_saved_path = Some(path.to_string())` (or however the field is typed).

### 4B. Create parent directory before save

**Unity source:** `ProjectArchive.Save()` lines 139-141
**Rust file:** `saver.rs`

**Bug:** Rust does not create parent directories. `std::fs::write` will fail if parent doesn't exist.

**Fix:** Add `std::fs::create_dir_all(path.parent().unwrap())` before writing.

---

## Phase 5: Migrator Hardening

### 5A. Add empty-string guard

**Unity source:** `ProjectJsonMigrator.cs` line 18
**Rust file:** `migrate.rs` line 5

**Fix:** Add early return for empty/whitespace input:
```rust
pub fn migrate_if_needed(json: &str) -> Result<String, ...> {
    if json.trim().is_empty() { return Ok(json.to_string()); }
    // ... existing logic
}
```

### 5B. Add JSON parse error recovery

**Unity source:** `ProjectJsonMigrator.cs` lines 22-29
**Rust file:** `migrate.rs` line 5

Unity wraps `JObject.Parse(json)` in try-catch and returns original string on failure. Rust propagates parse error via `?`.

**Fix:** Catch the parse error and return the original string:
```rust
let root = match serde_json::from_str::<Value>(json) {
    Ok(v) => v,
    Err(_) => return Ok(json.to_string()),  // let downstream deserializer handle it
};
```

---

## Phase 6: JSON Serialization Settings Alignment

### 6A. Verify serde output matches Unity's Newtonsoft output

**Unity source:** `Export/ManifoldJsonSettings.cs` (~145 lines)
**Rust concern:** Ensure field-for-field JSON compatibility

Unity's global JSON settings:
- `NullValueHandling.Ignore` — null fields omitted from output
- `DefaultValueHandling.Include` — default-valued fields included
- `ReferenceLoopHandling.Ignore` — cycle detection
- `StringEnumConverter` — enums as strings, not numbers

**Verification needed:**
1. Does Rust's serde skip `None` values? (Need `#[serde(skip_serializing_if = "Option::is_none")]` on all `Option<T>` fields)
2. Does Rust serialize enums as strings? (Need `#[serde(rename_all = "PascalCase")]` or string-based serialization)
3. Are default values included in output? (serde includes them by default — OK)

**Fix:** Audit all `Option<T>` fields in manifold-core for `skip_serializing_if`. Any `Option<T>` field that Unity would omit when null must have `#[serde(skip_serializing_if = "Option::is_none")]`.

---

## Phase 7: Missing Exporters (Deferred — Feature Gaps)

### 7A. VideoExporter — ~1400 lines

**Unity source:** `Export/VideoExporter.cs`
**Status:** Entirely missing from Rust

This is a major feature gap but can be deferred since it requires:
- GPU frame readback (wgpu-specific)
- Native Metal encoder FFI or FFmpeg pipe
- Audio muxing
- Real-time vs offline frame pacing

**Recommendation:** Defer to a dedicated export sprint. Document as known gap.

### 7B. ResolveFcpxmlExporter — ~260 lines

**Unity source:** `Export/ResolveFcpxmlExporter.cs`
**Status:** Entirely missing from Rust

FCPXML generation for DaVinci Resolve import. Lower priority than video export.

**Recommendation:** Defer. Document as known gap.

### 7C. MetalEncoderNative — ~76 lines

**Unity source:** `Export/MetalEncoderNative.cs`
**Status:** Entirely missing. Dependency of VideoExporter.

**Recommendation:** Defer until VideoExporter is ported.

---

## Phase 8: Update lib.rs exports

After all phases, update `crates/manifold-io/src/lib.rs` to export:
- `PathResolver` struct and `PathResolutionResult`
- `ProjectManifest` and `SnapshotEntry`
- `ProjectInfo`
- Archive utility functions (if standalone module)

---

## Verification Checklist

After implementing all phases:

- [ ] `PathResolver::resolve_all()` called on BOTH V1 and V2 load paths
- [ ] `PathResolver::store_relative_paths()` called before save
- [ ] V2 ZIP archive written with manifest, project.json, and history entries
- [ ] SHA-256 content hashing works (first 6 hex chars)
- [ ] Atomic save pattern (temp file + rename)
- [ ] Change deduplication (skip write if hash unchanged)
- [ ] History pruning (max 50 auto-saves)
- [ ] `project.last_saved_path` updated after save
- [ ] Parent directory created before save
- [ ] Duration mode migration only runs on V1 loads
- [ ] Empty-string guard in migrator
- [ ] JSON parse error recovery in migrator
- [ ] `Option<T>` fields have `skip_serializing_if` for Unity compatibility
- [ ] `cargo build` succeeds for `manifold-io`
- [ ] `cargo test` passes for `manifold-io`
- [ ] Projects saved by Rust can be loaded by Unity's V2 path
- [ ] Projects with relocated files have paths resolved on load

---

## Priority Order

**P0 — Data integrity / portability:**
1. Phase 1: PathResolver (projects can't survive file moves without this)
2. Phase 4A: Update last_saved_path after save
3. Phase 4B: Create parent directory before save

**P1 — Format compatibility:**
4. Phase 2: V2 archive format (ensures Rust↔Unity project interoperability)
5. Phase 6: JSON serialization alignment

**P2 — Correctness:**
6. Phase 3A: Fix duration mode migration scope
7. Phase 5: Migrator hardening

**P3 — Feature gaps (defer):**
8. Phase 7A: VideoExporter
9. Phase 7B: ResolveFcpxmlExporter

---

## Files Changed (Summary)

| File | Changes |
|------|---------|
| (new) `path_resolver.rs` | Port PathResolver class (~380 lines) |
| (new) `manifest.rs` | ProjectManifest, SnapshotEntry, ProjectInfo structs |
| (new) `archive.rs` | V2 ZIP archive read/write (~500 lines) |
| `loader.rs` | Add PathResolver call, fix V2 detection, fix duration mode scope |
| `saver.rs` | Rewrite to V2 format, add PathResolver call, update last_saved_path, create dirs |
| `migrate.rs` | Add empty-string guard, parse error recovery |
| `lib.rs` | Update exports |

## Crate Dependencies to Add

| Crate | Purpose |
|-------|---------|
| `zip` | ZIP archive read/write for V2 format |
| `sha2` | SHA-256 hashing for content deduplication |
| `flate2` | gzip compression for history entries |
