use crate::migrate;
use crate::path_resolver::PathResolver;
use manifold_core::project::{EmbeddedPreset, Project};
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
    load_project_with(path, |_| {})
}

/// [`load_project`] with an install hook: `register_embedded_presets` receives
/// the file's own project-scoped presets right after the typed `Project`
/// deserializes, so the caller (the app — the one seam that reaches both the
/// project and the renderer's catalog) can install them into the catalog
/// overlay + core definition registry BEFORE
/// [`Project::reconcile_param_manifests`] rebuilds every instance's param
/// manifest against the now-complete registry.
pub fn load_project_with(
    path: &Path,
    register_embedded_presets: impl FnOnce(&[EmbeddedPreset]),
) -> Result<Project, LoadError> {
    let file_bytes = std::fs::read(path).map_err(|e| LoadError::Io(e.to_string()))?;

    // Try V2 ZIP format first
    let (json, is_v2) = match extract_json_from_zip(&file_bytes) {
        Ok(json) => {
            // Archive-level guard (D5 site 2, coarse secondary check): refuse
            // a future container-format bump before touching project.json.
            if let Some(manifest) = crate::archive::read_manifest(&path.to_string_lossy())
                && manifest.format_version > crate::manifest::CURRENT_ARCHIVE_FORMAT_VERSION
            {
                return Err(LoadError::TooNew {
                    file_version: format!("archive v{}", manifest.format_version),
                    this_version: format!(
                        "archive v{}",
                        crate::manifest::CURRENT_ARCHIVE_FORMAT_VERSION
                    ),
                });
            }
            (json, true)
        }
        Err(_) => {
            // Not a ZIP — treat as plain JSON (V1)
            let json = String::from_utf8(file_bytes)
                .map_err(|e| LoadError::Io(format!("Invalid UTF-8: {e}")))?;
            (json, false)
        }
    };

    let mut project = load_project_from_json_with(&json, register_embedded_presets)?;

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

/// Load a history snapshot out of a V2 archive by its manifest hash.
///
/// Runs the exact same pipeline as [`load_project`]'s V2 path — migration,
/// deserialize, path resolution against the archive's directory, and full
/// post-load validation — so a restored snapshot behaves identically to an
/// opened project. Snapshot hashes come from the archive manifest
/// (`crate::archive::read_manifest`).
pub fn load_project_snapshot(archive_path: &Path, hash: &str) -> Result<Project, LoadError> {
    load_project_snapshot_with(archive_path, hash, |_| {})
}

/// [`load_project_snapshot`] with the same install hook as
/// [`load_project_with`] — a snapshot's embedded presets are installed and
/// reconciled exactly like a normal open.
pub fn load_project_snapshot_with(
    archive_path: &Path,
    hash: &str,
    register_embedded_presets: impl FnOnce(&[EmbeddedPreset]),
) -> Result<Project, LoadError> {
    let path_str = archive_path.to_string_lossy().to_string();
    let json =
        crate::archive::read_history_snapshot(&path_str, hash).map_err(LoadError::Io)?;

    let mut project = load_project_from_json_with(&json, register_embedded_presets)?;
    log::info!(
        "[Loader] Loaded history snapshot {hash} from {}",
        archive_path.display()
    );

    // Resolve media paths relative to the archive location, same as a normal
    // V2 open (the snapshot was saved from this same archive directory).
    project.last_saved_path = path_str.clone();
    PathResolver::resolve_all(&mut project, &path_str);

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

/// Parse `projectVersion` out of raw JSON without deserializing the full
/// `Project` (D5 site 1 — the same cheap `serde_json::Value` read
/// `migrate_if_needed` already does). Never panics: malformed JSON or a
/// missing/non-string field both degrade to `"1.0.0"`, matching migrate's
/// own default — a legacy pre-`projectVersion` V1 file is never too new.
fn raw_project_version(json: &str) -> String {
    serde_json::from_str::<serde_json::Value>(json)
        .ok()
        .and_then(|v| {
            v.get("projectVersion")
                .and_then(|pv| pv.as_str().map(str::to_string))
        })
        .unwrap_or_else(|| "1.0.0".to_string())
}

/// Load from raw JSON string. Runs steps 1-2 of post-load validation.
/// Steps 3-7 (duration mode migration, PathResolver, validate, validate_clips, purge)
/// are run by load_project after the file path is set. Callers using this directly
/// should call run_post_load_validation() separately.
pub fn load_project_from_json(json: &str) -> Result<Project, LoadError> {
    load_project_from_json_with(json, |_| {})
}

/// [`load_project_from_json`] with the embedded-presets install hook (see
/// [`load_project_with`]). Ordering (`PARAM_STORAGE_BOUNDARIES_DESIGN.md`
/// D1–D3), all inside this one function so no caller can get it wrong:
/// deserialize (each instance stashes its raw `params` wire map) → install
/// the file's own embedded presets into the registry → reconcile every
/// instance's manifest against that now-complete registry.
pub fn load_project_from_json_with(
    json: &str,
    register_embedded_presets: impl FnOnce(&[EmbeddedPreset]),
) -> Result<Project, LoadError> {
    // Forward-compat guard (PROJECT_FILE_INTEGRITY_DESIGN D1/D5) — MUST run
    // before migrate_if_needed: migrate is forward-only and would silently
    // pass a too-new file straight to deserialize, which drops fields this
    // build doesn't recognize (BUG-062).
    let file_version = raw_project_version(json);
    if migrate::is_version_less_than(manifold_core::project::CURRENT_PROJECT_VERSION, &file_version)
    {
        return Err(LoadError::TooNew {
            file_version,
            this_version: manifold_core::project::CURRENT_PROJECT_VERSION.to_string(),
        });
    }

    // Run version migration
    let migrated =
        migrate::migrate_if_needed(json).map_err(|e| LoadError::Migration(e.to_string()))?;

    // Deserialize. Each `PresetInstance` stashes its raw V1.4 `params` wire
    // map (`pending_wire`) rather than resolving it fully here — this
    // project's own embedded (forked / imported) preset types haven't been
    // installed into the registry yet.
    let mut project: Project =
        serde_json::from_str(&migrated).map_err(|e| LoadError::Deserialize(format!("{e}")))?;

    // Install the file's own embedded presets NOW — typed, post-parse. The
    // caller (the app) installs them into the catalog overlay + core
    // definition registry.
    register_embedded_presets(&project.embedded_presets);

    // Reconcile: rebuild every instance's manifest from its stash against
    // the now-complete registry. Unconditional — this is what makes BUG-036
    // (project-local preset types registering after their own layer data
    // deserialized) unreachable by construction, not just correctly ordered.
    // BUG-079: instances that still can't resolve a template after this pass
    // (deleted/unregistered/missing preset def) are counted into
    // `load_report` so the "opened with repairs" toast names them instead of
    // only logging to console.
    project.load_report.unresolved_preset_templates = project.reconcile_param_manifests();

    // Strip unrecognized effect types (e.g. removed effects from Unity projects).
    // Without this, Unknown effects stay in the effect list and show in the UI.
    project.load_report.unknown_effects_removed = project.strip_unknown_effects();

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
    project.load_report.missing_media_files = validation.missing_files;

    // Step 7: Purge orphaned references
    let purge_result = project.purge_orphaned_references();
    if purge_result.total_removed() > 0 {
        log::info!(
            "[Loader] Cleaned {} orphaned timeline clips, {} stale MIDI mappings",
            purge_result.timeline_clips_removed,
            purge_result.midi_mappings_removed
        );
    }
    project.load_report.orphaned_clips_purged = purge_result.timeline_clips_removed;
    project.load_report.orphaned_midi_purged = purge_result.midi_mappings_removed;

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

    // Step 9.6: Backfill display names for legacy `#N`-id forks (P1, D2).
    // Projects saved before fork ids became display-based carry an
    // embedded preset with an empty `display_name` — the card used to
    // derive a "(variant)" label from the id at render time
    // (`card_preset_name`'s now-deleted `'#'` split). Stamp the equivalent
    // readable name once here so old projects still read cleanly.
    let backfilled = project.backfill_legacy_fork_display_names();
    if backfilled > 0 {
        log::info!(
            "[Loader] Backfilled {backfilled} legacy fork preset display name(s)"
        );
    }

    // Step 10: Repair pre-existing clip overlaps.
    // Projects saved before overlap enforcement was added to all mutation paths
    // may contain overlapping clips. Remove the shorter clip in each collision.
    project.load_report.overlapping_clips_repaired = repair_overlapping_clips(project);
}

/// Repair overlapping clips by removing the shorter clip in each collision pair.
/// Only needed for projects saved before overlap enforcement was complete.
/// Returns the count removed (BUG-063 — feeds `Project::load_report`).
fn repair_overlapping_clips(project: &mut Project) -> usize {
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
    total_removed
}

#[derive(Debug)]
pub enum LoadError {
    Io(String),
    Migration(String),
    Deserialize(String),
    /// The file was written by a newer MANIFOLD than this build can open.
    /// `file_version` and `this_version` are the project-format versions
    /// (D4) — for the archive-container guard (§3.3 site 2) they read
    /// "archive vN" instead.
    TooNew {
        file_version: String,
        this_version: String,
    },
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoadError::Io(e) => write!(f, "IO error: {e}"),
            LoadError::Migration(e) => write!(f, "Migration error: {e}"),
            LoadError::Deserialize(e) => write!(f, "Deserialize error: {e}"),
            LoadError::TooNew {
                file_version,
                this_version,
            } => write!(
                f,
                "This project was saved by a newer version of MANIFOLD (project format {file_version}) than this build can open ({this_version}). Update MANIFOLD to open it."
            ),
        }
    }
}

#[cfg(test)]
mod legacy_clip_trigger_migration_tests {
    //! Round-trip gate for the P2 clip-trigger migration
    //! (`docs/AUDIO_SETUP_DOCK_AND_TRIGGER_UNIFICATION_DESIGN.md` §3.2), run
    //! through the REAL loader pipeline + REAL serde — not
    //! `Project::migrate_legacy_clip_triggers` called directly (that's
    //! manifold-core's own unit-level proof). Create-path green is half a
    //! gate (`docs/DESIGN_DOC_STANDARD.md` §5, BUG-036): this proves
    //! save -> reload survives too.
    //!
    //! `AudioSend.triggers` is `#[serde(skip_serializing)]`, so current code
    //! can never itself PRODUCE legacy JSON with a populated `triggers`
    //! array — these tests splice one onto a freshly-serialized project's raw
    //! JSON `Value`, reproducing exactly what a pre-P2 `.manifold` file on
    //! disk looks like.

    use super::*;
    use manifold_core::audio_mod::AudioBand;
    use manifold_core::audio_setup::AudioSend;
    use manifold_core::layer::Layer;
    use manifold_core::types::LayerType;
    use manifold_core::units::Beats;
    use serde_json::json;

    /// A project with one send (`send_label`) and one layer (`layer_name`),
    /// neither carrying any trigger data yet, serialized to JSON with a
    /// hand-spliced legacy `triggers` array on the send — `target_layer:
    /// None` when `explicit_target` is false (exercising the by-name
    /// auto-route), `Some(layer_id)` when true.
    fn legacy_json_with_route(
        send_label: &str,
        layer_name: &str,
        band: AudioBand,
        sensitivity: f32,
        one_shot_beats: f64,
        explicit_target: bool,
    ) -> (String, manifold_core::id::LayerId) {
        let mut project = Project::default();
        project.audio_setup.sends.push(AudioSend::new(send_label));
        let layer = Layer::new(layer_name.to_string(), LayerType::Video, 0);
        let layer_id = layer.layer_id.clone();
        project.timeline.layers.push(layer);

        let json_str = serde_json::to_string(&project).unwrap();
        let mut value: serde_json::Value = serde_json::from_str(&json_str).unwrap();

        let band_str = serde_json::to_value(band).unwrap();
        let mut route = json!({
            "enabled": true,
            "source": band_str,
            "sensitivity": sensitivity,
            "oneShotBeats": one_shot_beats,
        });
        if explicit_target {
            route["targetLayer"] = serde_json::Value::String(layer_id.to_string());
        }
        value["audioSetup"]["sends"][0]["triggers"] = json!([route]);

        (serde_json::to_string(&value).unwrap(), layer_id)
    }

    #[test]
    fn legacy_route_migrates_and_the_round_trip_survives_save_and_reload() {
        let (legacy_json, layer_id) =
            legacy_json_with_route("Kick", "Strobe", AudioBand::Low, 0.8, 2.0, true);

        // ── Load: migration runs inside `on_after_deserialize`. ──
        let loaded = load_project_from_json(&legacy_json).expect("legacy project loads");
        assert!(
            loaded.audio_setup.sends[0].triggers.is_empty(),
            "legacy storage drained on load"
        );
        let (_, layer) = loaded.timeline.find_layer_by_id(&layer_id).unwrap();
        assert_eq!(layer.clip_triggers.len(), 1, "migration produced exactly one config");
        let cfg = layer.clip_triggers[0].clone();
        assert!(cfg.enabled);
        assert_eq!(cfg.source.send_id, loaded.audio_setup.sends[0].id);
        assert_eq!(
            cfg.source.feature,
            manifold_core::audio_mod::AudioFeature::new(
                manifold_core::audio_mod::AudioFeatureKind::Transients,
                AudioBand::Low
            )
        );
        assert_eq!(cfg.shape.sensitivity, 0.8, "U5-verbatim sensitivity-to-Amount mapping");
        assert_eq!(cfg.one_shot_beats, Beats(2.0));

        // ── Save: `skip_serializing` proof — the legacy field never comes
        //    back, even though the in-memory `Vec` is a real (empty) field. ──
        let resaved = serde_json::to_string(&loaded).unwrap();
        let resaved_value: serde_json::Value = serde_json::from_str(&resaved).unwrap();
        assert!(
            resaved_value["audioSetup"]["sends"][0].get("triggers").is_none(),
            "triggers key must be entirely ABSENT from the saved JSON, not merely an empty array"
        );

        // ── Reload: the round trip. Create-path green is half a gate. ──
        let reloaded = load_project_from_json(&resaved).expect("re-saved project reloads");
        let (_, layer2) = reloaded.timeline.find_layer_by_id(&layer_id).unwrap();
        assert_eq!(layer2.clip_triggers.len(), 1, "clip trigger survives save -> reload");
        assert_eq!(
            layer2.clip_triggers[0], cfg,
            "the config is byte-identical after a full save/reload cycle — nothing about \
             serialization can prevent this trigger from firing exactly as it did before"
        );
    }

    #[test]
    fn legacy_route_auto_routes_by_send_label_when_no_explicit_target() {
        let (legacy_json, layer_id) =
            legacy_json_with_route("Kick", "Kick", AudioBand::Full, 0.5, 1.0, false);

        let loaded = load_project_from_json(&legacy_json).expect("legacy project loads");
        let (_, layer) = loaded.timeline.find_layer_by_id(&layer_id).unwrap();
        assert_eq!(layer.clip_triggers.len(), 1, "resolved by case-insensitive name match");
        assert!(loaded.audio_setup.sends[0].triggers.is_empty());
    }

    #[test]
    fn legacy_route_with_no_resolvable_target_is_dropped_but_still_drains() {
        // Label "Ghost" has no name-matching layer and no explicit target.
        let (legacy_json, _unrelated_layer_id) =
            legacy_json_with_route("Ghost", "Some Other Layer", AudioBand::Full, 0.5, 1.0, false);

        let loaded = load_project_from_json(&legacy_json).expect("legacy project still loads");
        assert!(
            loaded.audio_setup.sends[0].triggers.is_empty(),
            "drained even though the route was unresolvable — never silently kept"
        );
        assert!(
            loaded.timeline.layers.iter().all(|l| l.clip_triggers.is_empty()),
            "no layer received a config it wasn't targeted for"
        );
    }
}

impl std::error::Error for LoadError {}
