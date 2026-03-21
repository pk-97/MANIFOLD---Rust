//! ProjectIOService — plain struct owning all project lifecycle logic.
//!
//! 1:1 port of Unity `ProjectIOService.cs` (527 lines).
//! All project file management (new, open, open recent, save, save as)
//! and file drag-and-drop processing live here. Application delegates
//! to this service via thin wrapper methods.
//!
//! Unity's IProjectIOHost callback interface maps to return values —
//! ProjectIOAction tells the caller (Application) what side-effects to apply.
#![allow(dead_code)]

use std::path::{Path, PathBuf};

use manifold_core::clip::TimelineClip;
use manifold_core::project::Project;
use manifold_core::types::GeneratorType;
use manifold_core::video::VideoClip;
use manifold_editing::command::{Command, CompositeCommand};
use manifold_editing::commands::clip::AddClipCommand;
use manifold_editing::service::EditingService;

use crate::dialog_path_memory::{self, DialogContext};
use crate::user_prefs::UserPrefs;

// ── Constants — Unity ProjectIOService lines 25-28 ──────────────────

const FILE_DROP_DEFAULT_DURATION_BEATS: f32 = 4.0;
const FILE_DROP_MIN_DURATION_BEATS: f32 = 0.125;
const LAST_OPENED_PROJECT_PREF_KEY: &str = "MANIFOLD_LastOpenedProjectPath";

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
    pub flash_save: bool,
    /// Commands to record in the undo stack.
    pub record_commands: Vec<Box<dyn Command>>,
}

// ── ProjectIOService ────────────────────────────────────────────────

/// Plain struct (not a trait object) that owns all project I/O state.
/// Unity ProjectIOService.cs lines 19-527.
pub struct ProjectIOService {
    /// Last opened project path — persisted across sessions via UserPrefs.
    /// Unity field: lastOpenedProjectPath (line 41).
    last_opened_project_path: Option<String>,

    /// Preview metadata cache for file drop duration estimation.
    /// Unity: fileDropPreviewDurationSecondsByPath (line 42).
    file_drop_preview_duration_seconds: std::collections::HashMap<String, f32>,
}

impl ProjectIOService {
    pub fn new(user_prefs: &UserPrefs) -> Self {
        let last_path = user_prefs.get_string(LAST_OPENED_PROJECT_PREF_KEY, "");
        Self {
            last_opened_project_path: if last_path.is_empty() {
                None
            } else {
                Some(last_path)
            },
            file_drop_preview_duration_seconds: std::collections::HashMap::new(),
        }
    }

    /// Unity ProjectIOService.LastOpenedProjectPath (line 49).
    pub fn last_opened_project_path(&self) -> Option<&str> {
        self.last_opened_project_path.as_deref()
    }

    // ── New Project ─────────────────────────────────────────────────

    /// Unity ProjectIOService.OnNewProject (lines 81-90).
    pub fn new_project(&self) -> ProjectIOAction {
        let mut new_project = Project { project_name: "New Project".to_string(), ..Default::default() };
        new_project.timeline.add_layer(
            "Layer 0",
            manifold_core::types::LayerType::Video,
            GeneratorType::None,
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
        let last_dir = dialog_path_memory::get_last_directory(
            DialogContext::ProjectOpen,
            user_prefs,
        );

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

        let load_result = manifold_io::loader::load_project(path);

        match load_result {
            Ok(project) => {
                let path_str = path.to_string_lossy().to_string();
                let was_v1 = !manifold_io::archive::is_v2_archive(&path_str);
                let name = project.project_name.clone();

                // Persist last opened path (Unity lines 157-159)
                self.last_opened_project_path = Some(path_str.clone());
                user_prefs.set_string(LAST_OPENED_PROJECT_PREF_KEY, &path_str);
                user_prefs.save();

                if was_v1 {
                    log::info!(
                        "[ProjectIO] Opened V1 project (will save as V2): {} from {}",
                        name, path_str
                    );
                } else {
                    log::info!("[ProjectIO] Opened project: {} from {}", name, path_str);
                }

                ProjectIOAction {
                    apply_project: Some(project),
                    needs_structural_sync: true,
                    set_project_path: Some(path.to_path_buf()),
                    ..Default::default()
                }
            }
            Err(e) => {
                log::error!("[ProjectIO] Failed to open project: {e}");
                ProjectIOAction::default()
            }
        }
    }

    // ── Save Project ────────────────────────────────────────────────

    /// Unity ProjectIOService.OnSaveProject (lines 175-194).
    pub fn save_project(
        &self,
        project: &mut Project,
        current_path: Option<&Path>,
        current_time: f32,
        editing_service: &mut EditingService,
        user_prefs: &mut UserPrefs,
    ) -> ProjectIOAction {
        // Sync playhead before save (Unity line 179)
        project.saved_playhead_time = current_time;

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
        &self,
        project: &mut Project,
        current_time: f32,
        editing_service: &mut EditingService,
        user_prefs: &mut UserPrefs,
    ) -> ProjectIOAction {
        // Sync playhead before save (Unity line 205)
        project.saved_playhead_time = current_time;

        let last_dir = dialog_path_memory::get_last_directory(
            DialogContext::ProjectSave,
            user_prefs,
        );

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

            match manifold_io::saver::save_project(project, &path, None, false) {
                Ok(()) => {
                    // Update project name from filename (Unity line 217)
                    if let Some(stem) = path.file_stem() {
                        project.project_name = stem.to_string_lossy().into_owned();
                    }

                    // Persist paths (Unity lines 218-221)
                    let path_str = path.to_string_lossy().to_string();
                    user_prefs.set_string(LAST_OPENED_PROJECT_PREF_KEY, &path_str);
                    dialog_path_memory::remember_directory(
                        DialogContext::ProjectSave,
                        &path_str,
                        user_prefs,
                    );
                    user_prefs.save();

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
                    log::error!("[ProjectIO] Save failed: {e}");
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
            if let Some(ref fp) = file_path {
                if fp.exists() && is_supported_midi_extension(fp) {
                    let midi_action = self.process_dropped_midi_file(fp, drop_beat, drop_layer_index, project);
                    if midi_action.needs_clip_sync {
                        action.needs_clip_sync = true;
                    }
                    action.record_commands.extend(midi_action.record_commands);
                }
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
                    GeneratorType::None,
                );
            }

            let duration_beats = self.get_clip_duration_beats(
                &video_clip_id,
                &file_path_str,
                project,
                seconds_per_beat,
            );

            // Create timeline clip (Unity lines 301-307)
            let timeline_clip = TimelineClip {
                video_clip_id: video_clip_id.clone(),
                layer_index: drop_layer_index,
                start_beat: placement_beat,
                duration_beats,
                in_point: 0.0,
                generator_type: GeneratorType::None,
                ..TimelineClip::default()
            };

            // Add clip to layer (Unity lines 309-312)
            let layer_idx = drop_layer_index as usize;
            if layer_idx < project.timeline.layers.len() {
                project.timeline.layers[layer_idx].clips.push(timeline_clip.clone());
                drop_commands.push(Box::new(AddClipCommand::new(timeline_clip, drop_layer_index)));
            }

            placement_beat += duration_beats;
            imported_count += 1;
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
            log::warn!("[ProjectIO] MIDI file contained no notes: {}", file_path_str);
            return ProjectIOAction::default();
        }

        let result = manifold_playback::midi_import::MidiImportService::import_to_layer(
            &notes,
            drop_layer_index as usize,
            drop_beat,
            project,
        );

        if result.success {
            let mut action = ProjectIOAction { needs_clip_sync: true, ..Default::default() };
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
            if let Some(&preview_seconds) = self.file_drop_preview_duration_seconds.get(file_path) {
                if preview_seconds > 0.0 && seconds_per_beat > 0.0 {
                    return (preview_seconds / seconds_per_beat).max(FILE_DROP_MIN_DURATION_BEATS);
                }
            }
            return FILE_DROP_DEFAULT_DURATION_BEATS;
        }

        if seconds_per_beat <= 0.0 {
            return FILE_DROP_DEFAULT_DURATION_BEATS;
        }

        (duration / seconds_per_beat).max(FILE_DROP_MIN_DURATION_BEATS)
    }

    /// Process a project file drop (routes through shared open_project_from_path).
    pub fn process_dropped_project_file(
        &mut self,
        path: &Path,
        user_prefs: &mut UserPrefs,
    ) -> ProjectIOAction {
        self.open_project_from_path(path, user_prefs)
    }
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
