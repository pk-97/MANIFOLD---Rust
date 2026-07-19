//! ProjectIOService — plain struct owning all project lifecycle logic.
//!
//! 1:1 port of Unity `ProjectIOService.cs` (527 lines).
//! All project file management (new, open, open recent, save, save as)
//! and file drag-and-drop processing live here. Application delegates
//! to this service via thin wrapper methods.
//!
//! Unity's IProjectIOHost callback interface maps to return values —
//! ProjectIOAction tells the caller (Application) what side-effects to apply.

use std::path::{Path, PathBuf};

use manifold_core::PresetTypeId;
use manifold_core::clip::TimelineClip;
use manifold_core::preset_def::PresetKind;
use manifold_core::project::Project;
use manifold_core::video::VideoClip;
use manifold_editing::command::{Command, CompositeCommand};
use manifold_editing::commands::clip::AddClipCommand;
use manifold_editing::commands::layer::AddLayerCommand;
use manifold_editing::service::EditingService;

use crate::dialog_path_memory::{self, DialogContext};
use crate::user_prefs::UserPrefs;

/// Install a loaded project's embedded ("forked") presets into the renderer's
/// catalog overlay, so each resolves by id through the same path as stock/user
/// presets. Called on every project load (with the project's list, possibly
/// empty) so a previous project's forks are cleared/replaced — the overlay is
/// global, but only one project is live at a time. Also re-invoked from the
/// content thread whenever an editing command forks a preset or recalibrates an
/// embedded one (see `ContentThread::refresh_preset_overlay_if_changed`).
pub(crate) fn install_project_preset_overlay(project: &Project) {
    install_embedded_presets(&project.embedded_presets);
}

/// List-based form of [`install_project_preset_overlay`], shaped to plug into
/// the loader's install hook (`manifold_io::loader::load_project_with` and
/// friends): the overlay + core definition registry are populated right
/// after the project deserializes and right before
/// `Project::reconcile_param_manifests` rebuilds every instance's param
/// manifest against them (`PARAM_STORAGE_BOUNDARIES_DESIGN.md` D1–D3 —
/// supersedes the pre-deserialize ordering that guarded against BUG-036).
pub(crate) fn install_embedded_presets(presets: &[manifold_core::project::EmbeddedPreset]) {
    let mut effect = Vec::new();
    let mut generator = Vec::new();
    for p in presets {
        let Some(id) = p.id() else { continue };
        let Ok(json) = serde_json::to_string(&p.def) else {
            log::error!("[ProjectIO] failed to serialize embedded preset `{}`", id);
            continue;
        };
        match p.kind {
            PresetKind::Effect => effect.push((id.as_str().to_string(), json, p.origin)),
            PresetKind::Generator => generator.push((id.as_str().to_string(), json, p.origin)),
        }
    }
    manifold_renderer::preset_loader::set_project_presets(effect, generator);
}

/// Self-containment snapshot (PRESET_LIBRARY_DESIGN D5, P2). Called
/// immediately before every on-disk save (manual save, Save As, autosave) so
/// a `.manifold` file never strands a project on a library file that later
/// disappears.
///
/// Collects the ids referenced by every TRACKING instance
/// ([`Project::tracking_preset_ids`]: effects, clip effects, master effects,
/// generators), and for each one NOT already covered by a `Saved` embedded
/// entry (a `Saved` entry is already self-contained and deliberately never
/// shadowed), upserts its CURRENT resolved def into `embedded_presets` as
/// `origin: Snapshot`. Then prunes `Snapshot` entries no tracking instance
/// references anymore.
///
/// App-side (not `manifold-core`/`manifold-io`) because it needs BOTH the
/// project (to walk instances) AND the renderer's live preset catalog (to
/// read each id's current def) — core has no renderer dependency and io
/// doesn't either, so this is the one seam where both are reachable.
/// `bundled_preset_def` reads the same overlay-merged catalog the runtime
/// resolves against, so "current def" here means exactly what a tracking
/// instance is using right now (disk if present, the prior snapshot as
/// fallback if not — see `preset_loader::build_catalog`'s merge order).
pub(crate) fn snapshot_and_prune_embedded_presets(project: &mut Project) {
    use manifold_core::project::{EmbeddedOrigin, EmbeddedPreset};
    use std::collections::{HashMap, HashSet};

    let referenced = project.tracking_preset_ids();
    let referenced_ids: HashSet<PresetTypeId> =
        referenced.iter().map(|(id, _)| id.clone()).collect();

    // Dedup by id before doing any catalog lookup / clone / upsert work — a
    // preset type is typically tracked by many instances (dozens of layers
    // running the same generator, a common effect on several chains); a
    // typical project has orders of magnitude more instances than distinct
    // types (see `project_typical_project_scale`), so this keeps save-time
    // cost proportional to distinct library ids, not instance count.
    let mut by_id: HashMap<PresetTypeId, PresetKind> = HashMap::new();
    for (id, kind) in referenced {
        by_id.entry(id).or_insert(kind);
    }

    for (id, kind) in &by_id {
        if project
            .embedded_preset(id)
            .is_some_and(|p| p.origin == EmbeddedOrigin::Saved)
        {
            // Already self-contained by an explicit Save-to-Project / fork /
            // import entry — never shadowed by an auto-captured snapshot.
            continue;
        }
        let Some(def) = manifold_renderer::node_graph::bundled_preset_def(id) else {
            // Resolves nowhere (not even the current overlay) — nothing to
            // snapshot. This is the orphan case D9 exists to prevent for new
            // instances; an existing project that somehow reached this state
            // is left as-is rather than manufacturing a def from nothing.
            continue;
        };
        project.upsert_embedded_preset(EmbeddedPreset {
            kind: *kind,
            def: def.clone(),
            origin: EmbeddedOrigin::Snapshot,
        });
    }

    project.prune_stale_snapshots(&referenced_ids);
}

/// A cheap fingerprint of a project's embedded ("forked") presets, used by the
/// content thread to decide whether an editing command changed the fork set and
/// the catalog overlay must be re-derived. `0` when there are no forks (the
/// common case) so the per-edit check is a single integer compare; with forks
/// present it hashes each preset's id + serialized def, which catches both a
/// new fork and an in-place recalibration of an existing one.
pub(crate) fn embedded_presets_fingerprint(project: &Project) -> u64 {
    use std::hash::{Hash, Hasher};
    if project.embedded_presets.is_empty() {
        return 0;
    }
    let mut h = ahash::AHasher::default();
    for p in &project.embedded_presets {
        if let Some(id) = p.id() {
            id.as_str().hash(&mut h);
        }
        if let Ok(json) = serde_json::to_string(&p.def) {
            json.hash(&mut h);
        }
    }
    // Never collide with the empty-set sentinel.
    let f = h.finish();
    if f == 0 { 1 } else { f }
}

// ── Constants — Unity ProjectIOService lines 25-28 ──────────────────

const FILE_DROP_DEFAULT_DURATION_BEATS: f32 = 4.0;
const FILE_DROP_MIN_DURATION_BEATS: f32 = 0.125;
const LAST_OPENED_PROJECT_PREF_KEY: &str = "MANIFOLD_LastOpenedProjectPath";
/// Pref key for the recent-projects list (JSON array of absolute paths,
/// most-recent first). Drives the File → Open Recent submenu.
const RECENT_PROJECTS_PREF_KEY: &str = "MANIFOLD_RecentProjects";
/// How many entries the Open Recent menu retains (matches the Ableton ballpark).
const MAX_RECENT_PROJECTS: usize = 12;

// ── ProjectIOAction — replaces IProjectIOHost callback interface ────

/// Actions the caller (Application) must perform after a ProjectIO operation.
/// Replaces Unity's IProjectIOHost callback interface — since Rust can't have
/// the service call back into Application (ownership), we return actions instead.
#[derive(Default)]
pub struct ProjectIOAction {
    /// A project to apply (replaces host.ApplyProject + host.OnProjectOpened).
    pub apply_project: Option<Project>,
    /// Whether the editing service should be marked clean.
    pub mark_clean: bool,
    /// Whether the UI needs a structural sync (rebuild tree).
    pub needs_structural_sync: bool,
    /// Whether clip sync is needed (replaces host.MarkNeedsClipSync).
    pub needs_clip_sync: bool,
    /// New project path to set as current (for window title, save, etc.).
    pub set_project_path: Option<PathBuf>,
    /// Flash the save button (visual feedback).
    // Set by save paths; the UI flash that reads it is part of the Unity
    // port not yet wired up. Scoped allow so the rest of this struct still
    // trips dead-code.
    #[allow(dead_code)]
    pub flash_save: bool,
    /// Commands to record in the undo stack.
    pub record_commands: Vec<Box<dyn Command>>,
    /// Non-blocking notice for the UI toast (e.g. "Opened with repairs: …").
    /// `None` means nothing to show — the common case. See BUG-063,
    /// `docs/PROJECT_FILE_INTEGRITY_DESIGN.md` §3.6.
    pub notice: Option<String>,
}

// ── ProjectIOService ────────────────────────────────────────────────

/// Plain struct (not a trait object) that owns all project I/O state.
/// Unity ProjectIOService.cs lines 19-527.
pub struct ProjectIOService {
    /// Last opened project path — persisted across sessions via UserPrefs.
    /// Unity field: lastOpenedProjectPath (line 41).
    last_opened_project_path: Option<String>,

    /// Recently opened/saved project paths, most-recent first, capped at
    /// [`MAX_RECENT_PROJECTS`]. Persisted as a JSON array under
    /// [`RECENT_PROJECTS_PREF_KEY`]; drives the File → Open Recent submenu.
    recent_projects: Vec<String>,

    /// Preview metadata cache for file drop duration estimation.
    /// Unity: fileDropPreviewDurationSecondsByPath (line 42).
    file_drop_preview_duration_seconds: std::collections::HashMap<String, f32>,
}

impl ProjectIOService {
    pub fn new(user_prefs: &UserPrefs) -> Self {
        let last_path = user_prefs.get_string(LAST_OPENED_PROJECT_PREF_KEY, "");
        let last_opened_project_path = if last_path.is_empty() {
            None
        } else {
            Some(last_path.clone())
        };

        // Load the recent list (JSON array). Seed from the single last-opened
        // path on first run after upgrade, so the menu isn't empty for users who
        // predate the list.
        let recent_json = user_prefs.get_string(RECENT_PROJECTS_PREF_KEY, "");
        let mut recent_projects: Vec<String> =
            serde_json::from_str(&recent_json).unwrap_or_default();
        if recent_projects.is_empty() && !last_path.is_empty() {
            recent_projects.push(last_path);
        }

        Self {
            last_opened_project_path,
            recent_projects,
            file_drop_preview_duration_seconds: std::collections::HashMap::new(),
        }
    }

    /// Unity ProjectIOService.LastOpenedProjectPath (line 49).
    // Ported service method; its Application wrapper isn't wired yet.
    #[allow(dead_code)]
    pub fn last_opened_project_path(&self) -> Option<&str> {
        self.last_opened_project_path.as_deref()
    }

    /// Recent project paths, most-recent first, filtered to those that still
    /// exist on disk. Drives the File → Open Recent submenu.
    pub fn recent_projects(&self) -> Vec<PathBuf> {
        self.recent_projects
            .iter()
            .map(PathBuf::from)
            .filter(|p| p.exists())
            .collect()
    }

    /// Promote `path_str` to the front of the recent list (dedup, cap) and
    /// persist. Called on every successful open and save-as.
    fn push_recent_project(&mut self, path_str: &str, user_prefs: &mut UserPrefs) {
        promote_recent(&mut self.recent_projects, path_str, MAX_RECENT_PROJECTS);
        self.persist_recent_projects(user_prefs);
    }

    /// Empty the recent-projects list and persist. Backs "Clear Recent Projects".
    pub fn clear_recent_projects(&mut self, user_prefs: &mut UserPrefs) {
        self.recent_projects.clear();
        self.persist_recent_projects(user_prefs);
    }

    fn persist_recent_projects(&self, user_prefs: &mut UserPrefs) {
        let json = serde_json::to_string(&self.recent_projects).unwrap_or_else(|_| "[]".to_string());
        user_prefs.set_string(RECENT_PROJECTS_PREF_KEY, &json);
        user_prefs.save();
    }

    // ── New Project ─────────────────────────────────────────────────

    /// Unity ProjectIOService.OnNewProject (lines 81-90).
    pub fn new_project(&self) -> ProjectIOAction {
        let mut new_project = Project {
            project_name: "New Project".to_string(),
            ..Default::default()
        };
        new_project.timeline.add_layer(
            "Layer 0",
            manifold_core::types::LayerType::Video,
            PresetTypeId::NONE,
        );
        log::info!("[ProjectIO] Created new project");

        ProjectIOAction {
            apply_project: Some(new_project),
            needs_structural_sync: true,
            set_project_path: Some(PathBuf::new()), // Clear current path
            ..Default::default()
        }
    }

    // ── Open Project ────────────────────────────────────────────────

    /// Unity ProjectIOService.OnOpenProject / OnOpenProjectAsync (lines 92-106).
    pub fn open_project(&mut self, user_prefs: &mut UserPrefs) -> ProjectIOAction {
        let last_dir =
            dialog_path_memory::get_last_directory(DialogContext::ProjectOpen, user_prefs);

        let mut dialog = rfd::FileDialog::new()
            .set_title("Open MANIFOLD Project")
            .add_filter("MANIFOLD Project", &["json", "manifold"]);

        if !last_dir.is_empty() {
            dialog = dialog.set_directory(&last_dir);
        }

        if let Some(path) = dialog.pick_file() {
            let path_str = path.to_string_lossy().to_string();
            dialog_path_memory::remember_directory(
                DialogContext::ProjectOpen,
                &path_str,
                user_prefs,
            );
            self.open_project_from_path(&path, user_prefs)
        } else {
            ProjectIOAction::default()
        }
    }

    /// Unity ProjectIOService.OnOpenRecentProject (lines 108-123).
    pub fn open_recent_project(&mut self, user_prefs: &mut UserPrefs) -> ProjectIOAction {
        let last_path = match &self.last_opened_project_path {
            Some(p) if !p.is_empty() => p.clone(),
            _ => {
                log::warn!("[ProjectIO] No recent project to open.");
                return ProjectIOAction::default();
            }
        };

        let path = PathBuf::from(&last_path);
        if !path.exists() {
            log::warn!("[ProjectIO] Recent project not found: {last_path}");
            return ProjectIOAction::default();
        }

        self.open_project_from_path(&path, user_prefs)
    }

    /// Unity ProjectIOService.OpenProjectFromPath (lines 125-173).
    /// Core load logic shared by open, open recent, and file drop.
    pub fn open_project_from_path(
        &mut self,
        path: &Path,
        user_prefs: &mut UserPrefs,
    ) -> ProjectIOAction {
        if path.as_os_str().is_empty() {
            return ProjectIOAction::default();
        }

        // The install hook runs AFTER a successful typed deserialize
        // (PARAM_STORAGE_BOUNDARIES_DESIGN.md D2/D3): the loader hands this
        // project's own embedded presets to `install_embedded_presets`, then
        // reconciles every instance's param manifest against the
        // now-complete registry. A failed load never touches the overlay —
        // there is no rollback window to guard, unlike the pre-P1 order
        // (pre-deserialize install, so a failed load could strand the live
        // project on the candidate file's presets).
        let load_result =
            manifold_io::loader::load_project_with(path, install_embedded_presets);

        match load_result {
            Ok(mut project) => {
                // One-time load upgrade: pre-node-id user bindings stored
                // their target by handle. Resolve those to stable node ids
                // against each effect's graph (override or canonical
                // preset) now, before the project goes live, so a grouped
                // inner node keeps driving its card slider. Idempotent and
                // renderer-side (it needs the bundled preset graphs).
                manifold_renderer::node_graph::migrate_user_param_bindings_to_node_id(
                    &mut project,
                );

                // D5 (SCENE_OBJECT_AND_PANEL_V2_DESIGN.md): the Scene Setup
                // panel reads a generator layer's stored graph override
                // directly (`Layer::generator_graph`), never through
                // `instantiate_def` — so the legacy per-object wire shape
                // must be migrated here, at project load, not only at graph
                // instantiation (`graph_loader.rs` handles that path
                // separately, for runtime playback). Idempotent, per-layer;
                // a def with no legacy wires is untouched.
                // P1 (SCENE_PANEL_EXPOSURE_CONVERGENCE_DESIGN.md): stamp every
                // scene-vocabulary node's params into the def's card
                // exposures at load, same idempotent per-layer posture as the
                // wire migration above — an old project (or one edited before
                // this migration shipped) gets working card rows without a
                // re-import.
                for layer in &mut project.timeline.layers {
                    if let Some(graph) = layer.gen_params_mut().and_then(|gp| gp.graph.as_mut()) {
                        manifold_core::scene_object_migration::migrate_scene_object_wires(graph);
                        manifold_renderer::node_graph::scene_exposure::migrate_scene_exposures(graph);
                    }
                }

                // Overlay install happened in the pre-deserialize hook above;
                // `apply_project_io_action` re-installs on every project apply
                // (unconditionally, even when empty) so a previous project's
                // forks never leak into the next one.

                let path_str = path.to_string_lossy().to_string();
                let was_v1 = !manifold_io::archive::is_v2_archive(&path_str);
                let name = project.project_name.clone();

                // Persist last opened path (Unity lines 157-159)
                self.last_opened_project_path = Some(path_str.clone());
                user_prefs.set_string(LAST_OPENED_PROJECT_PREF_KEY, &path_str);
                user_prefs.save();
                // Promote to the front of the Open Recent list.
                self.push_recent_project(&path_str, user_prefs);

                if was_v1 {
                    log::info!(
                        "[ProjectIO] Opened V1 project (will save as V2): {} from {}",
                        name,
                        path_str
                    );
                } else {
                    log::info!("[ProjectIO] Opened project: {} from {}", name, path_str);
                }

                // Surface silent load-repairs (BUG-063) as a non-blocking
                // toast — never `alerts::error`, which is D1's blocking
                // refusal path for a too-new file.
                let notice = if !project.load_report.is_empty() {
                    Some(format!(
                        "Opened with repairs:\n{}",
                        project.load_report.human_lines().join("\n")
                    ))
                } else {
                    None
                };

                ProjectIOAction {
                    apply_project: Some(project),
                    needs_structural_sync: true,
                    set_project_path: Some(path.to_path_buf()),
                    notice,
                    ..Default::default()
                }
            }
            Err(e) => {
                // G4: load failures were log-only — surface them. The
                // current project's overlay was never touched (D2), so
                // nothing needs to be put back.
                log::error!("[ProjectIO] Failed to open project: {e}");
                crate::alerts::error(
                    "Couldn't Open Project",
                    &format!("MANIFOLD couldn't open\n{}\n\n{e}", path.display()),
                );
                ProjectIOAction::default()
            }
        }
    }

    // ── Save Project ────────────────────────────────────────────────

    /// Unity ProjectIOService.OnSaveProject (lines 175-194).
    // Ported save path; the live save currently routes elsewhere. Kept for
    // the in-progress port integration.
    #[allow(dead_code)]
    pub fn save_project(
        &mut self,
        project: &mut Project,
        current_path: Option<&Path>,
        current_time: f32,
        editing_service: &mut EditingService,
        user_prefs: &mut UserPrefs,
    ) -> ProjectIOAction {
        // Sync playhead before save (Unity line 179)
        project.saved_playhead_time = current_time;
        snapshot_and_prune_embedded_presets(project);

        if let Some(path) = current_path {
            match manifold_io::saver::save_project(project, path, None, false) {
                Ok(()) => {
                    editing_service.mark_clean();
                    log::info!("[ProjectIO] Saved to {}", path.display());
                    ProjectIOAction {
                        mark_clean: true,
                        flash_save: true,
                        ..Default::default()
                    }
                }
                Err(e) => {
                    log::error!("[ProjectIO] Save failed: {e}");
                    ProjectIOAction::default()
                }
            }
        } else {
            // No existing path → trigger Save As
            self.save_project_as(project, current_time, editing_service, user_prefs)
        }
    }

    /// Unity ProjectIOService.OnSaveProjectAs / OnSaveProjectAsAsync (lines 196-228).
    pub fn save_project_as(
        &mut self,
        project: &mut Project,
        current_time: f32,
        editing_service: &mut EditingService,
        user_prefs: &mut UserPrefs,
    ) -> ProjectIOAction {
        // Sync playhead before save (Unity line 205)
        project.saved_playhead_time = current_time;

        let last_dir =
            dialog_path_memory::get_last_directory(DialogContext::ProjectSave, user_prefs);

        let project_name = if project.project_name.is_empty() {
            "project"
        } else {
            &project.project_name
        };

        let mut dialog = rfd::FileDialog::new()
            .set_title("Save MANIFOLD Project")
            .add_filter("MANIFOLD Project", &["manifold"])
            .set_file_name(project_name);

        if !last_dir.is_empty() {
            dialog = dialog.set_directory(&last_dir);
        }

        if let Some(mut path) = dialog.save_file() {
            // Ensure .manifold extension (Unity line 212-213)
            if path.extension().is_none_or(|e| e != "manifold") {
                path.set_extension("manifold");
            }

            snapshot_and_prune_embedded_presets(project);
            match manifold_io::saver::save_project(project, &path, None, false) {
                Ok(()) => {
                    // Update project name from filename (Unity line 217)
                    if let Some(stem) = path.file_stem() {
                        project.project_name = stem.to_string_lossy().into_owned();
                    }

                    // Persist paths (Unity lines 218-221)
                    let path_str = path.to_string_lossy().to_string();
                    self.last_opened_project_path = Some(path_str.clone());
                    user_prefs.set_string(LAST_OPENED_PROJECT_PREF_KEY, &path_str);
                    dialog_path_memory::remember_directory(
                        DialogContext::ProjectSave,
                        &path_str,
                        user_prefs,
                    );
                    user_prefs.save();
                    // Promote to the front of the Open Recent list.
                    self.push_recent_project(&path_str, user_prefs);

                    editing_service.mark_clean();
                    log::info!("[ProjectIO] Saved to {}", path.display());

                    ProjectIOAction {
                        mark_clean: true,
                        flash_save: true,
                        set_project_path: Some(path),
                        ..Default::default()
                    }
                }
                Err(e) => {
                    // G4: a silent Save As failure means believing work is
                    // on disk when it isn't. Log AND surface it.
                    log::error!("[ProjectIO] Save failed: {e}");
                    crate::alerts::error(
                        "Save Failed",
                        &format!(
                            "MANIFOLD couldn't save to\n{}\n\n{e}\n\n\
                             Your work is NOT on disk — check free space and try again.",
                            path.display()
                        ),
                    );
                    ProjectIOAction::default()
                }
            }
        } else {
            ProjectIOAction::default()
        }
    }

    // ── File Drag-and-Drop ──────────────────────────────────────────

    /// Unity ProjectIOService.ProcessDroppedFiles (lines 246-326).
    /// Processes dropped video/MIDI files at a given beat and layer.
    pub fn process_dropped_files(
        &mut self,
        file_paths: &[PathBuf],
        drop_beat: f32,
        drop_layer_index: i32,
        join_audio_layer: Option<manifold_core::LayerId>,
        project: &mut Project,
        seconds_per_beat: f32,
    ) -> ProjectIOAction {
        if file_paths.is_empty() {
            return ProjectIOAction::default();
        }

        let mut action = ProjectIOAction::default();

        // Route MIDI files separately (Unity lines 252-259)
        for raw_path in file_paths {
            let file_path = resolve_dropped_file_path(raw_path);
            if let Some(ref fp) = file_path
                && fp.exists()
                && is_supported_midi_extension(fp)
            {
                let midi_action =
                    self.process_dropped_midi_file(fp, drop_beat, drop_layer_index, project);
                if midi_action.needs_clip_sync {
                    action.needs_clip_sync = true;
                }
                action.record_commands.extend(midi_action.record_commands);
            }
        }

        // Process video files (Unity lines 261-326)
        let mut placement_beat = drop_beat;
        let mut imported_count = 0;
        let mut drop_commands: Vec<Box<dyn Command>> = Vec::new();

        for raw_path in file_paths {
            let file_path = match resolve_dropped_file_path(raw_path) {
                Some(fp) => fp,
                None => continue,
            };
            if !file_path.exists() {
                continue;
            }
            if is_supported_midi_extension(&file_path) {
                continue;
            }
            if !is_supported_video_extension(&file_path) {
                continue;
            }

            let file_name = file_path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "Untitled".to_string());

            let file_path_str = file_path.to_string_lossy().into_owned();

            // Check if video clip already exists in library (Unity lines 278-293)
            let existing_id = project
                .video_library
                .clips
                .iter()
                .find(|vc| vc.file_path == file_path_str)
                .map(|vc| vc.id.clone());

            let video_clip_id = if let Some(id) = existing_id {
                id
            } else {
                let file_size = std::fs::metadata(&file_path)
                    .map(|m| m.len() as i64)
                    .unwrap_or(0);
                let last_modified = std::fs::metadata(&file_path)
                    .ok()
                    .and_then(|m| m.modified().ok())
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0);

                let video_clip = VideoClip {
                    id: manifold_core::short_id(),
                    file_path: file_path_str.clone(),
                    relative_file_path: None,
                    file_name: file_name.clone(),
                    duration: 0.0,
                    resolution_width: 0,
                    resolution_height: 0,
                    file_size,
                    last_modified_ticks: last_modified,
                };
                let id = video_clip.id.clone();
                project.video_library.add_clip(video_clip);
                id
            };

            // Ensure enough layers exist (Unity lines 295-296)
            while (project.timeline.layers.len() as i32) <= drop_layer_index {
                let name = format!("Layer {}", project.timeline.layers.len());
                project.timeline.add_layer(
                    &name,
                    manifold_core::types::LayerType::Video,
                    PresetTypeId::NONE,
                );
            }

            let duration_beats = self.get_clip_duration_beats(
                &video_clip_id,
                &file_path_str,
                project,
                seconds_per_beat,
            );

            // Create timeline clip (Unity lines 301-307)
            let drop_layer_id = project
                .timeline
                .layers
                .get(drop_layer_index as usize)
                .map(|l| l.layer_id.clone())
                .unwrap_or_default();
            let timeline_clip = TimelineClip {
                video_clip_id: video_clip_id.clone(),
                layer_id: drop_layer_id.clone(),
                start_beat: manifold_core::Beats::from_f32(placement_beat),
                duration_beats: manifold_core::Beats::from_f32(duration_beats),
                in_point: manifold_core::Seconds::ZERO,
                generator_type: PresetTypeId::NONE,
                ..TimelineClip::default()
            };

            // AddClipCommand enforces non-overlap internally.
            let layer_idx = drop_layer_index as usize;
            if layer_idx < project.timeline.layers.len() {
                let mut add_cmd =
                    AddClipCommand::new(timeline_clip, drop_layer_id, seconds_per_beat);
                add_cmd.execute(project);
                drop_commands.push(Box::new(add_cmd));
            }

            placement_beat += duration_beats;
            imported_count += 1;
        }

        // Process audio files (Audio Layer feature, docs/AUDIO_LAYER_DESIGN.md §6).
        // A file dropped ONTO an existing audio lane joins it (a new clip on that
        // lane at the drop beat); a file dropped on empty timeline space appends
        // its own new audio lane. `join_audio_layer` is the lane under the cursor,
        // validated here to still exist and be audio.
        let join_target: Option<manifold_core::LayerId> = join_audio_layer.and_then(|id| {
            project
                .timeline
                .layers
                .iter()
                .find(|l| l.layer_id == id && l.is_audio())
                .map(|l| l.layer_id.clone())
        });
        let mut audio_imported = 0;
        for raw_path in file_paths {
            let file_path = match resolve_dropped_file_path(raw_path) {
                Some(fp) => fp,
                None => continue,
            };
            if !file_path.exists() || !is_supported_audio_extension(&file_path) {
                continue;
            }

            let path_str = file_path.to_string_lossy().into_owned();
            // Decode once: the full file length bounds trimming (source_duration)
            // and, at the project tempo, sets the initial clip length.
            let source_duration = audio_source_duration(&path_str);
            let duration_beats = if seconds_per_beat > 0.0 {
                manifold_core::Beats::from_f32(source_duration.as_f32() / seconds_per_beat)
            } else {
                manifold_core::Beats::ZERO
            };

            // Join the targeted audio lane, or append a new one for this file.
            let (layer_id, add_layer_cmd) = if let Some(ref target) = join_target {
                (target.clone(), None)
            } else {
                let file_name = file_path
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "Audio".to_string());
                let insert_index = project.timeline.layers.len();
                let mut add_layer = AddLayerCommand::new(
                    file_name,
                    manifold_core::types::LayerType::Audio,
                    PresetTypeId::NONE,
                    insert_index,
                    None,
                );
                add_layer.execute(project);
                let Some(lid) = project
                    .timeline
                    .layers
                    .get(insert_index)
                    .map(|l| l.layer_id.clone())
                else {
                    continue;
                };
                (lid, Some(add_layer))
            };

            let clip = TimelineClip::new_audio(
                path_str,
                manifold_core::Beats::from_f32(drop_beat),
                duration_beats,
                manifold_core::Seconds::ZERO,
                source_duration,
            );
            let mut add_clip = AddClipCommand::new(clip, layer_id, seconds_per_beat);
            add_clip.execute(project);

            // One undo step per file: a new lane removes clip + lane; a join removes
            // just the clip.
            let cmd: Box<dyn Command> = match add_layer_cmd {
                Some(add_layer) => Box::new(CompositeCommand::new(
                    vec![Box::new(add_layer), Box::new(add_clip)],
                    "Drop audio".to_string(),
                )),
                None => Box::new(add_clip),
            };
            action.record_commands.push(cmd);
            action.needs_clip_sync = true;
            audio_imported += 1;
        }
        if audio_imported > 0 {
            let how = if join_target.is_some() { "onto lane" } else { "as new lane(s)" };
            log::info!(
                "[ProjectIO] Dropped {audio_imported} audio file(s) {how} at beat {drop_beat:.2}"
            );
        }

        if imported_count > 0 {
            let description = if imported_count == 1 {
                "Drop clip".to_string()
            } else {
                "Drop clips".to_string()
            };

            if drop_commands.len() == 1 {
                action.record_commands.push(drop_commands.remove(0));
            } else {
                action
                    .record_commands
                    .push(Box::new(CompositeCommand::new(drop_commands, description)));
            }

            action.needs_clip_sync = true;
            log::info!(
                "[ProjectIO] Dropped {} file(s) at beat {:.2}, layer {}",
                imported_count,
                drop_beat,
                drop_layer_index
            );
        }

        action
    }

    /// Unity ProjectIOService.ProcessDroppedMidiFile (lines 486-507).
    fn process_dropped_midi_file(
        &self,
        file_path: &Path,
        drop_beat: f32,
        drop_layer_index: i32,
        project: &mut Project,
    ) -> ProjectIOAction {
        let file_path_str = file_path.to_string_lossy().into_owned();

        let notes = manifold_playback::midi_parser::MidiFileParser::parse_file(&file_path_str);

        if notes.is_empty() {
            log::warn!(
                "[ProjectIO] MIDI file contained no notes: {}",
                file_path_str
            );
            return ProjectIOAction::default();
        }

        let target_layer_id = project
            .timeline
            .layers
            .get(drop_layer_index as usize)
            .map(|l| l.layer_id.clone())
            .unwrap_or_default();
        let result = manifold_playback::midi_import::MidiImportService::import_to_layer(
            &notes,
            &target_layer_id,
            drop_beat,
            project,
        );

        if result.success {
            let mut action = ProjectIOAction {
                needs_clip_sync: true,
                ..Default::default()
            };
            if let Some(cmd) = result.undo_command {
                action.record_commands.push(cmd);
            }
            action
        } else {
            ProjectIOAction::default()
        }
    }

    /// Unity ProjectIOService.GetClipDurationBeats (lines 328-349).
    fn get_clip_duration_beats(
        &self,
        video_clip_id: &str,
        file_path: &str,
        project: &Project,
        seconds_per_beat: f32,
    ) -> f32 {
        let duration = project
            .video_library
            .clips
            .iter()
            .find(|vc| vc.id == video_clip_id)
            .map(|vc| vc.duration)
            .unwrap_or(0.0);

        if duration <= 0.0 {
            // Try preview metadata cache (Unity lines 332-339)
            if let Some(&preview_seconds) = self.file_drop_preview_duration_seconds.get(file_path)
                && preview_seconds > 0.0
                && seconds_per_beat > 0.0
            {
                return (preview_seconds / seconds_per_beat).max(FILE_DROP_MIN_DURATION_BEATS);
            }
            return FILE_DROP_DEFAULT_DURATION_BEATS;
        }

        if seconds_per_beat <= 0.0 {
            return FILE_DROP_DEFAULT_DURATION_BEATS;
        }

        (duration / seconds_per_beat).max(FILE_DROP_MIN_DURATION_BEATS)
    }

    /// Process a project file drop (routes through shared open_project_from_path).
    // Ported drop handler; its Application wrapper isn't wired yet.
    #[allow(dead_code)]
    pub fn process_dropped_project_file(
        &mut self,
        path: &Path,
        user_prefs: &mut UserPrefs,
    ) -> ProjectIOAction {
        self.open_project_from_path(path, user_prefs)
    }
}

/// Move `path` to the front of a most-recent-first list: removes any existing
/// occurrence (so a re-open promotes rather than duplicates), prepends, then caps
/// at `max`. Pure — the persistence/menu-refresh wrappers live on the service.
fn promote_recent(list: &mut Vec<String>, path: &str, max: usize) {
    list.retain(|p| p != path);
    list.insert(0, path.to_string());
    list.truncate(max);
}

// ── Static helpers — Unity ProjectIOService lines 453-484 ───────────

/// Unity ProjectIOService.ResolveDroppedFilePath (lines 453-463).
pub fn resolve_dropped_file_path(raw_path: &Path) -> Option<PathBuf> {
    let raw_str = raw_path.to_string_lossy();
    if raw_str.trim().is_empty() {
        return None;
    }

    // Handle file:// URIs (Unity line 457-461)
    if raw_str.starts_with("file://") {
        let stripped = raw_str.trim_start_matches("file://");
        let p = PathBuf::from(stripped);
        if p.exists() {
            return Some(p);
        }
    }

    if raw_path.is_absolute() {
        Some(raw_path.to_path_buf())
    } else {
        Some(std::fs::canonicalize(raw_path).unwrap_or_else(|_| raw_path.to_path_buf()))
    }
}

/// Unity ProjectIOService.IsSupportedVideoExtension (lines 466-473).
pub fn is_supported_video_extension(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_lowercase())
            .as_deref(),
        Some("mp4" | "mov" | "webm" | "avi")
    )
}

/// Whether `path` is a still image we can drop onto a Video layer as an
/// image clip. Must stay in sync with the format features enabled on the
/// `image` crate in `manifold-media` (ImageRenderer decodes these).
pub fn is_supported_image_extension(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_lowercase())
            .as_deref(),
        Some("png" | "jpg" | "jpeg" | "webp" | "bmp" | "gif" | "tif" | "tiff")
    )
}

/// Unity ProjectIOService.IsSupportedMidiExtension (lines 479-484).
pub fn is_supported_midi_extension(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_lowercase())
            .as_deref(),
        Some("mid" | "midi")
    )
}

/// Audio file extensions that drop onto an audio layer. Mirrors what the
/// decoder (symphonia) and kira can read. See `docs/AUDIO_LAYER_DESIGN.md`.
pub fn is_supported_audio_extension(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_lowercase())
            .as_deref(),
        Some("wav" | "mp3" | "flac" | "aif" | "aiff" | "ogg" | "m4a" | "aac")
    )
}

/// Decoded duration of an audio file expressed in beats (0 on failure). Used to
/// size a dropped audio clip. Decodes the file; drops are infrequent and the
/// playback/analysis paths re-decode through their own caches.
/// Decoded length of an audio file in seconds (0 on failure / empty). This is the
/// clip's `source_duration` — the bound for right-edge trimming — and, divided by
/// seconds-per-beat, its initial timeline length.
pub(crate) fn audio_source_duration(path: &str) -> manifold_core::Seconds {
    match manifold_playback::audio_decoder::decode_audio_to_pcm(path) {
        Ok(d) if d.channels > 0 && d.sample_rate > 0 => {
            let frames = d.samples.len() / d.channels;
            manifold_core::Seconds::from_f32(frames as f32 / d.sample_rate as f32)
        }
        Ok(_) => manifold_core::Seconds::ZERO,
        Err(e) => {
            log::warn!("[ProjectIO] audio duration decode failed for '{path}': {e}");
            manifold_core::Seconds::ZERO
        }
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use manifold_core::clip::TimelineClip;
    use manifold_core::layer::Layer;
    use manifold_core::types::LayerType;
    use manifold_core::units::Bpm;

    #[test]
    fn audio_extensions_recognized_and_others_rejected() {
        for ok in ["a.wav", "a.MP3", "a.flac", "a.aiff", "a.ogg", "a.m4a"] {
            assert!(is_supported_audio_extension(Path::new(ok)), "{ok}");
        }
        for no in ["a.mp4", "a.mid", "a.png", "a"] {
            assert!(!is_supported_audio_extension(Path::new(no)), "{no}");
        }
    }

    #[test]
    fn promote_recent_dedups_caps_and_orders_most_recent_first() {
        let mut list: Vec<String> = Vec::new();

        promote_recent(&mut list, "/a.manifold", 3);
        promote_recent(&mut list, "/b.manifold", 3);
        promote_recent(&mut list, "/c.manifold", 3);
        assert_eq!(list, ["/c.manifold", "/b.manifold", "/a.manifold"]);

        // Re-opening an existing entry promotes it without duplicating.
        promote_recent(&mut list, "/a.manifold", 3);
        assert_eq!(list, ["/a.manifold", "/c.manifold", "/b.manifold"]);

        // Cap drops the oldest.
        promote_recent(&mut list, "/d.manifold", 3);
        assert_eq!(list, ["/d.manifold", "/a.manifold", "/c.manifold"]);
    }

    #[test]
    fn audio_source_duration_is_zero_for_unreadable_path() {
        // A bad path yields a zero source length (no panic), which collapses the
        // initial clip length to zero rather than guessing.
        assert_eq!(
            audio_source_duration("/no/such/file.wav"),
            manifold_core::Seconds::ZERO
        );
    }

    #[test]
    fn dropped_video_enforces_non_overlap_immediately() {
        let temp_path =
            std::env::temp_dir().join(format!("manifold-drop-{}.mp4", std::process::id()));
        std::fs::write(&temp_path, b"test").unwrap();

        let prefs = UserPrefs::load();
        let mut service = ProjectIOService::new(&prefs);
        let mut project = Project::default();
        project.settings.bpm = Bpm(120.0);
        project
            .timeline
            .insert_layer(0, Layer::new("Video 1".into(), LayerType::Video, 0));
        project.timeline.layers[0].restore_clip(TimelineClip {
            video_clip_id: "existing".into(),
            start_beat: manifold_core::Beats::from_f32(0.0),
            duration_beats: manifold_core::Beats::from_f32(4.0),
            ..TimelineClip::default()
        });

        let action = service.process_dropped_files(
            std::slice::from_ref(&temp_path),
            0.0,
            0,
            None,
            &mut project,
            0.5,
        );

        assert!(action.needs_clip_sync);
        assert_eq!(project.timeline.layers[0].clips.len(), 1);
        assert!(!project.timeline.layers[0].has_overlapping_clips());

        let _ = std::fs::remove_file(temp_path);
    }

    // ── BUG-063 — surface silent load-repairs (PROJECT_FILE_INTEGRITY_DESIGN §3.6 P3) ──
    //
    // `manifold-app` is bin-only (no `[lib]` target), so an integration test
    // under `tests/*.rs` can't call `open_project_from_path` at all — only a
    // `#[cfg(test)]` unit test inside the crate can reach it. This one writes
    // a real V1 JSON fixture to a temp path (a known-unknown master effect +
    // an overlapping-clip layer, same repairs `load_report.rs` in
    // `manifold-io` exercises directly) and opens it through the actual
    // production seam.

    #[test]
    fn open_project_from_path_surfaces_repairs_as_a_notice() {
        use manifold_core::effects::PresetInstance;

        let mut project = Project::default();
        project.settings.bpm = Bpm(120.0);
        project
            .settings
            .master_effects
            .push(PresetInstance::new(PresetTypeId::BLOOM));
        project
            .settings
            .master_effects
            .push(PresetInstance::new(PresetTypeId::UNKNOWN));

        project
            .timeline
            .insert_layer(0, Layer::new("Video 1".into(), LayerType::Video, 0));
        project.timeline.layers[0].restore_clip(TimelineClip {
            start_beat: manifold_core::Beats::from_f32(0.0),
            duration_beats: manifold_core::Beats::from_f32(4.0),
            ..TimelineClip::default()
        });
        project.timeline.layers[0].restore_clip(TimelineClip {
            start_beat: manifold_core::Beats::from_f32(2.0),
            duration_beats: manifold_core::Beats::from_f32(4.0),
            ..TimelineClip::default()
        });

        let fixture_path = std::env::temp_dir().join(format!(
            "manifold-load-repair-notice-{}.manifold",
            std::process::id()
        ));
        manifold_io::saver::save_project_v1(&project, &fixture_path)
            .expect("write V1 fixture with repairable content");

        let mut prefs = UserPrefs::load();
        let mut service = ProjectIOService::new(&prefs);
        let action = service.open_project_from_path(&fixture_path, &mut prefs);

        let _ = std::fs::remove_file(&fixture_path);

        assert!(
            action.apply_project.is_some(),
            "a repairable-but-valid file must still open"
        );
        let notice = action
            .notice
            .expect("a repairing load must set a non-blocking notice");
        assert!(
            notice.contains("unknown effect"),
            "notice must name the unknown-effect repair: {notice}"
        );
        assert!(
            notice.contains("overlapping clip"),
            "notice must name the overlap repair: {notice}"
        );
    }

    // ── P1 (SCENE_PANEL_EXPOSURE_CONVERGENCE_DESIGN.md): load migration ──
    //
    // An old-shape scene generator graph (saved before the exposure-stamping
    // convergence, or hand-edited to strip `preset_metadata`) must gain
    // working card exposures on open, same idempotent per-layer posture as
    // `migrate_scene_object_wires` right above it in the real load path.
    // Real save→load through the production seam, same recipe as
    // `open_project_from_path_surfaces_repairs_as_a_notice` above.

    #[test]
    fn open_project_from_path_migrates_scene_exposures_for_old_shape_generator_graph() {
        use manifold_core::effect_graph_def::{BindingTarget, EffectGraphDef, EffectGraphNode, EFFECT_GRAPH_VERSION};
        use std::collections::BTreeMap;

        let mut project = Project::default();
        project.settings.bpm = Bpm(120.0);

        let mut layer = Layer::new("Scene 1".into(), LayerType::Generator, 0);
        let old_shape_def = EffectGraphDef {
            version: EFFECT_GRAPH_VERSION,
            name: None,
            description: None,
            preset_metadata: None, // pre-P1 shape: no exposures stamped at all
            nodes: vec![EffectGraphNode {
                id: 1,
                node_id: manifold_core::NodeId::new("sun"),
                type_id: "node.light".to_string(),
                handle: Some("Sun".to_string()),
                params: BTreeMap::new(),
                exposed_params: Default::default(),
                editor_pos: None,
                wgsl_source: None,
                title: None,
                output_formats: BTreeMap::new(),
                output_canvas_scales: BTreeMap::new(),
                group: None,
            }],
            wires: vec![],
        };
        layer.gen_params_or_init().graph = Some(old_shape_def);
        project.timeline.insert_layer(0, layer);

        let fixture_path = std::env::temp_dir().join(format!(
            "manifold-scene-exposure-migration-{}.manifold",
            std::process::id()
        ));
        manifold_io::saver::save_project_v1(&project, &fixture_path)
            .expect("write V1 fixture with an old-shape scene generator graph");

        let mut prefs = UserPrefs::load();
        let mut service = ProjectIOService::new(&prefs);
        let action = service.open_project_from_path(&fixture_path, &mut prefs);

        let _ = std::fs::remove_file(&fixture_path);

        let loaded = action.apply_project.expect("a valid fixture must open");
        let (_, loaded_layer) = loaded
            .timeline
            .find_layer_by_id(&loaded.timeline.layers[0].layer_id)
            .expect("the generator layer round-trips");
        let graph = loaded_layer.generator_graph().expect("the generator graph override round-trips");

        let meta = graph
            .preset_metadata
            .as_ref()
            .expect("P1 load migration stamped exposures into the def's preset_metadata");
        let sun = graph.nodes.iter().find(|n| n.type_id == "node.light").unwrap();
        assert!(
            meta.bindings.iter().any(|b| matches!(
                &b.target,
                BindingTarget::Node { node_id, .. } if *node_id == sun.node_id
            )),
            "the sun's params are exposed after load, targeting its bare NodeId"
        );
        assert!(!meta.params.is_empty(), "at least one ParamSpecDef was stamped");
    }

    // ── PRESET_LIBRARY_DESIGN D5/P2 — snapshot-on-save ──────────────────
    //
    // These exercise the actual production seam (`snapshot_and_prune_
    // embedded_presets`) rather than just the catalog-merge rule (covered
    // separately by `manifold-renderer/tests/project_preset_overlay.rs`,
    // which proves disk-wins-over-Snapshot / Snapshot-as-fallback at the
    // catalog level). A full save→delete-user-file→reload file-system test
    // isn't reachable from here: `manifold-app` is a bin-only crate (no
    // `[lib]` target), so `tests/*.rs` integration binaries can't see
    // `pub(crate)` items like this function at all — only a `#[cfg(test)]`
    // unit test inside the crate can call it directly. This is that test.

    #[test]
    fn snapshot_and_prune_captures_tracking_ids_and_prunes_stale_ones() {
        use manifold_core::effects::PresetInstance;
        use manifold_core::project::EmbeddedOrigin;

        let mut project = Project::default();
        project
            .settings
            .master_effects
            .push(PresetInstance::new(PresetTypeId::BLOOM));

        snapshot_and_prune_embedded_presets(&mut project);

        let snapshot = project
            .embedded_preset(&PresetTypeId::BLOOM)
            .expect("a tracking instance's library id must get a self-containment snapshot");
        assert_eq!(snapshot.origin, EmbeddedOrigin::Snapshot);
        let expected = manifold_renderer::node_graph::bundled_preset_def(&PresetTypeId::BLOOM)
            .expect("Bloom must resolve in the live catalog");
        assert_eq!(
            snapshot.def.preset_metadata.as_ref().map(|m| &m.id),
            expected.preset_metadata.as_ref().map(|m| &m.id),
            "snapshot must carry Bloom's current resolved def"
        );

        // The instance no longer references Bloom (deleted here; a
        // retarget would do the same) — the next save must prune the now-
        // stale snapshot rather than let it accumulate forever.
        project.settings.master_effects.clear();
        snapshot_and_prune_embedded_presets(&mut project);
        assert!(
            project.embedded_preset(&PresetTypeId::BLOOM).is_none(),
            "a Snapshot no tracking instance references anymore must be pruned"
        );
    }

    #[test]
    fn snapshot_and_prune_never_overwrites_a_saved_embedded_entry() {
        use manifold_core::effects::PresetInstance;
        use manifold_core::project::{EmbeddedOrigin, EmbeddedPreset};

        let mut project = Project::default();
        project
            .settings
            .master_effects
            .push(PresetInstance::new(PresetTypeId::BLOOM));

        // A Saved entry under the same id as the tracking instance — Saved
        // is deliberate (Save to Project / fork / import) and must never be
        // downgraded or overwritten by the auto-captured snapshot pass.
        let saved_def = manifold_renderer::node_graph::bundled_preset_def(&PresetTypeId::BLOOM)
            .expect("Bloom resolves")
            .clone();
        project.upsert_embedded_preset(EmbeddedPreset {
            kind: PresetKind::Effect,
            def: saved_def,
            origin: EmbeddedOrigin::Saved,
        });

        snapshot_and_prune_embedded_presets(&mut project);

        assert_eq!(
            project.embedded_preset(&PresetTypeId::BLOOM).unwrap().origin,
            EmbeddedOrigin::Saved,
            "a Saved entry must never be overwritten by the snapshot pass"
        );
    }

    /// P2 size gate: the self-containment snapshot must not blow up a real
    /// show's file size. Loads the canonical Liveschool fixture (52 layers,
    /// ~2828 clips, 160 effects) and saves it TWICE to the same V2 archive
    /// format a real save produces (stored, not deflated) — once as a plain
    /// re-save (baseline: captures whatever migrations/path-resolution the
    /// load→save cycle itself causes, independent of P2) and once with
    /// `snapshot_and_prune_embedded_presets` applied first. Comparing
    /// scratch-vs-scratch isolates JUST the snapshot mechanism's size cost
    /// from the confound of a raw-fixture-vs-resave comparison. Both numbers
    /// are reported (see the phase report). Skipped if the (gitignored,
    /// local) fixture isn't present.
    #[test]
    fn liveschool_snapshot_on_save_size_delta_stays_bounded() {
        let mut fixture = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        fixture.push("../../tests/fixtures/Liveschool Live Show V6 LEDS.manifold");
        if !fixture.exists() {
            return;
        }

        let original_size = std::fs::metadata(&fixture).expect("stat fixture").len();

        // Baseline: load → save, no snapshot pass. Isolates load/migration/
        // path-resolution effects on file size from P2's own contribution.
        let mut baseline_project =
            manifold_io::loader::load_project(&fixture).expect("load Liveschool fixture");
        let baseline_path = std::env::temp_dir().join(format!(
            "manifold-liveschool-baseline-{}.manifold",
            std::process::id()
        ));
        manifold_io::saver::save_project(&mut baseline_project, &baseline_path, None, false)
            .expect("save baseline (no snapshot) project");
        let baseline_size = std::fs::metadata(&baseline_path).expect("stat baseline save").len();
        let _ = std::fs::remove_file(&baseline_path);

        // P2: load → snapshot_and_prune → save.
        let mut snapshotted_project =
            manifold_io::loader::load_project(&fixture).expect("load Liveschool fixture");
        snapshot_and_prune_embedded_presets(&mut snapshotted_project);
        let snapshot_path = std::env::temp_dir().join(format!(
            "manifold-liveschool-snapshot-{}.manifold",
            std::process::id()
        ));
        manifold_io::saver::save_project(&mut snapshotted_project, &snapshot_path, None, false)
            .expect("save snapshotted project");
        let snapshot_size = std::fs::metadata(&snapshot_path).expect("stat scratch save").len();
        let _ = std::fs::remove_file(&snapshot_path);

        let isolated_delta = snapshot_size as i64 - baseline_size as i64;
        let raw_delta = snapshot_size as i64 - original_size as i64;
        eprintln!(
            "[P2 size gate] Liveschool: original={original_size} bytes, \
             baseline-resave={baseline_size} bytes, with-snapshot={snapshot_size} bytes, \
             isolated P2 delta={isolated_delta} bytes, raw original-vs-final delta={raw_delta} bytes"
        );
        assert!(
            isolated_delta < 5_000_000,
            "self-containment snapshot alone grew the file by {isolated_delta} bytes (>5MB) — \
             escalate per PRESET_LIBRARY_DESIGN P2 gate"
        );
    }
}
