//! Project lifecycle methods for Application — extracted from app.rs.
//!
//! Contains save/open/new project methods, audio loading, and output window
//! management. All methods are `impl Application` blocks that operate on the
//! struct defined in app.rs.

use std::sync::Arc;

use winit::event_loop::ActiveEventLoop;

use manifold_editing::service::EditingService;

use crate::app::Application;
use crate::content_command::ContentCommand;

use crate::project_io::ProjectIOAction;
use crate::window_registry::{WindowRole, WindowState};
use manifold_core::{LayerId, Seconds};

/// Pure worker-thread logic for one video-import batch: probes each path and
/// builds either a playable `AddClipCommand` (+ `VideoClip` library entry) or
/// a human-readable failure message (BUG-133) for files the decoder can't
/// open — e.g. `.webm`/`.avi` files AVFoundation has no codec for. Extracted
/// from `import_video_files`'s spawned thread so the probe-fail path is
/// unit-testable without a live `Application`/content-thread.
fn build_video_import_batch(
    paths: &[std::path::PathBuf],
    bpm: f32,
    insert_beat: f32,
    layer_id: &LayerId,
) -> (
    Vec<manifold_core::video::VideoClip>,
    Vec<ContentCommand>,
    Vec<String>,
) {
    let spb = 60.0 / bpm;
    let mut beat = insert_beat;
    let mut video_clips: Vec<manifold_core::video::VideoClip> = Vec::new();
    let mut commands: Vec<ContentCommand> = Vec::new();
    let mut failures: Vec<String> = Vec::new();

    for path in paths {
        let path_str = path.to_string_lossy().to_string();
        let file_name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();

        let meta = manifold_media::metadata::probe_video_metadata(&path_str);
        let (duration_secs, res_w, res_h) = match meta {
            Some(m) => (m.duration, m.width, m.height),
            None => {
                log::warn!("[Import] Probe failed: {path_str}");
                failures.push(format!(
                    "{file_name} — codec/container not supported by MANIFOLD's \
                     decoder (AVFoundation). Re-encode to H.264/HEVC in an .mp4 \
                     or .mov and re-import."
                ));
                continue;
            }
        };

        let video_clip_id = manifold_core::short_id();
        let file_size = std::fs::metadata(path).map(|m| m.len() as i64).unwrap_or(0);

        video_clips.push(manifold_core::video::VideoClip {
            id: video_clip_id.clone(),
            file_path: path_str.clone(),
            relative_file_path: None,
            file_name: file_name.clone(),
            duration: duration_secs,
            resolution_width: res_w,
            resolution_height: res_h,
            file_size,
            last_modified_ticks: 0,
        });

        let duration_beats = (duration_secs / spb).max(0.25);

        let clip = manifold_core::clip::TimelineClip::new_video(
            video_clip_id,
            manifold_core::Beats::from_f32(beat),
            manifold_core::Beats::from_f32(duration_beats),
            manifold_core::Seconds::ZERO,
        );

        commands.push(ContentCommand::Execute(Box::new(
            manifold_editing::commands::clip::AddClipCommand::new(clip, layer_id.clone()),
        )));

        log::warn!(
            "[Import] Added '{file_name}' at beat {beat:.1} \
             ({duration_secs:.1}s → {duration_beats:.1} beats, {res_w}x{res_h})"
        );

        beat += duration_beats;
    }

    (video_clips, commands, failures)
}

/// The one video import path, shared by Cmd+I, Finder drag-drop and Finder
/// paste: places the files at `insert_beat` on the active layer (the first
/// layer when none is active). Probing runs off-thread, and the library
/// entries reach the content thread before the clips that use them, so a
/// clip never plays black for want of its source. Returns the receiver for
/// probe failures, or `None` when there is nothing to import into.
pub(crate) fn start_video_import(
    paths: &[std::path::PathBuf],
    project: &manifold_core::project::Project,
    active_layer: Option<&LayerId>,
    insert_beat: f32,
    content_tx: &crossbeam_channel::Sender<ContentCommand>,
) -> Option<std::sync::mpsc::Receiver<Vec<String>>> {
    if paths.is_empty() {
        return None;
    }
    let layer_id = active_layer
        .and_then(|lid| {
            project
                .timeline
                .layer_index_for_id(lid)
                .and_then(|i| project.timeline.layers.get(i))
                .map(|l| l.layer_id.clone())
        })
        .or_else(|| project.timeline.layers.first().map(|l| l.layer_id.clone()))?;

    let bpm = project.settings.bpm.0;
    let paths = paths.to_vec();
    let content_tx = content_tx.clone();

    // BUG-133: the file-dialog filter deliberately stays broad (mp4/mov/
    // webm/avi) — trimming it was considered and rejected. AVFoundation
    // (the only decoder) generally can't open webm (no VP8/VP9) and has
    // patchy avi support, so the probe below is the real gate: a
    // probe-failing file must reject the import with a clear message,
    // never just a log line the user never sees (no-silent-fallbacks).
    let (failure_tx, failure_rx) = std::sync::mpsc::channel::<Vec<String>>();

    std::thread::spawn(move || {
        let (video_clips, commands, failures) =
            build_video_import_batch(&paths, bpm, insert_beat, &layer_id);

        if video_clips.is_empty() {
            // Every file in this batch failed to probe — nothing to add,
            // but the caller still needs to know (never a silent no-op).
            let _ = failure_tx.send(failures);
            return;
        }

        // Send library update FIRST so content thread has the VideoClip
        // entries before AddClipCommand triggers sync_clips_to_time.
        let _ = content_tx.send(ContentCommand::mutate(crate::content_command::touch::MEDIA, Box::new(move |p| {
            for vc in video_clips {
                p.video_library.add_clip(vc);
            }
        })));

        // Then send AddClipCommands
        for cmd in commands {
            let _ = content_tx.send(cmd);
        }

        // Surface any partial-batch probe failures alongside the
        // successful imports (BUG-133) — never dropped just because
        // some files in the same drop/dialog batch succeeded.
        if !failures.is_empty() {
            let _ = failure_tx.send(failures);
        }
    });
    Some(failure_rx)
}

/// A model file's material warnings, where import warnings go.
fn log_model_warnings(file: &str, warnings: &[String]) {
    for warning in warnings {
        log::warn!("[Import] {file}: {warning}");
    }
}

impl Application {
    // ── Project I/O — delegates to ProjectIOService ────────────────────

    /// Persist current viewport scroll + zoom and UI collapse states into the
    /// save snapshot's settings.
    fn save_viewport_state(&mut self) {
        let settings = &mut self.local_project.intake().settings;
        settings.viewport_scroll_x_beats =
            self.ws.ui_root.viewport.scroll_x_beats().as_f32();
        settings.viewport_scroll_y_px = self.ws.ui_root.viewport.scroll_y_px();
        settings.viewport_pixels_per_beat =
            self.ws.ui_root.viewport.pixels_per_beat();

        // Persist panel sizing. These read from the live UI layout (which is
        // never clobbered by content-thread snapshots) rather than relying on a
        // drag-end write into local_project.settings — that write gets wiped by
        // the next snapshot clone before save can capture it.
        settings.inspector_width = self.ws.ui_root.layout.inspector_width;
        settings.timeline_height_percent =
            self.ws.ui_root.layout.timeline_split_ratio;

        // Persist inspector collapse states
        settings.macros_collapsed =
            self.ws.ui_root.inspector.macros_panel().is_collapsed();
        settings.master_chrome_collapsed =
            self.ws.ui_root.inspector.master_chrome().is_collapsed();
        settings.layer_chrome_collapsed =
            self.ws.ui_root.inspector.layer_chrome().is_collapsed();
        settings.clip_chrome_collapsed =
            self.ws.ui_root.inspector.clip_chrome().is_collapsed();
        settings.audio_triggers_collapsed =
            self.ws.ui_root.inspector.audio_trigger_section().collapsed_setting();
    }

    /// Save. Delegates to ProjectIOService.save_project.
    /// Save only a content-owned snapshot after queued edits and recording have
    /// reached the undo service. Never silently serialize a stale UI replica.
    fn prepare_recording_save(&mut self) -> Option<u64> {
        // The content thread owns the project while it renders an export and
        // would drop the request; say why instead of timing out.
        if self.content_state.is_exporting {
            crate::alerts::error("Export Running", "An export is running, so the project cannot be saved now. Cancel the export, then save.");
            return None;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        self.send_content_cmd(ContentCommand::PrepareProjectSave(tx));
        match rx.recv_timeout(std::time::Duration::from_secs(5)) {
            Ok(Some((project, version))) => {
                *self.local_project.intake() = project;
                self.ui_edits.stamps.project_replaced();
                Some(version)
            }
            result => {
                log::error!("Cannot obtain authoritative save snapshot: {result:?}");
                crate::alerts::error("Save Failed", "The content engine did not finish preparing the project. Your work has not been saved; please try again.");
                None
            }
        }
    }

    /// Save to the open project's file; an untitled project goes to Save As.
    /// True once the project is on disk. False when the save failed (its
    /// error is already shown) or the Save As dialog was cancelled.
    pub(crate) fn save_project(&mut self) -> bool {
        let Some(path) = self.current_project_path.clone() else { return self.save_project_as(); };
        let Some(save_version) = self.prepare_recording_save() else { return false; };
        let current_time = self.content_state.current_time;
        // Stamp UI layout onto the authoritative snapshot.
        self.local_project.intake().saved_playhead_time = current_time.as_f32();
        self.save_viewport_state();
        crate::project_io::snapshot_and_prune_embedded_presets(self.local_project.intake());
        match manifold_io::saver::save_project(self.local_project.intake(), &path, None, false) {
            Ok(()) => {
                self.send_content_cmd(ContentCommand::MarkCleanAt(save_version));
                log::info!("[ProjectIO] Saved to {}", path.display());
                // The save pushed the previous state into history/ —
                // keep the Revert to Snapshot menu current.
                self.refresh_history_menu();
                true
            }
            Err(e) => {
                // G4: a silent save failure means believing work is on
                // disk when it isn't. Log AND surface it.
                log::error!("[ProjectIO] Save failed: {e}");
                crate::alerts::error(
                    "Save Failed",
                    &format!(
                        "MANIFOLD couldn't save to\n{}\n\n{e}\n\n\
                         Your work is NOT on disk — check free space and try again.",
                        path.display()
                    ),
                );
                false
            }
        }
    }

    /// Save As. Delegates to ProjectIOService.save_project_as. True once the
    /// project is on disk; false when it failed or the dialog was cancelled.
    pub(crate) fn save_project_as(&mut self) -> bool {
        let Some(save_version) = self.prepare_recording_save() else { return false; };
        let current_time = self.content_state.current_time;
        self.local_project.intake().saved_playhead_time = current_time.as_f32();
        self.save_viewport_state();
        let host = crate::file_dialogs::host(
            &self.window_registry,
            self.dialog_window_id,
            self.primary_window_id,
            self.current_project_path.as_deref(),
        );
        let mut action = self.project_io.save_project_as(
            self.local_project.intake(),
            current_time.as_f32(),
            &mut EditingService::new(), // placeholder — mark clean via content thread
            host,
            &mut self.user_prefs,
        );
        let saved = action.mark_clean;
        if saved {
            self.send_content_cmd(ContentCommand::MarkCleanAt(save_version));
            action.mark_clean = false;
        }
        self.apply_project_io_action(action);
        saved
    }

    /// Collect All and Save (PROJECT_FOLDERS_DESIGN D6): copy every external
    /// asset into the project folder's `Media/` family subfolders, re-point the
    /// stored paths to the in-folder files, and save. Runs on the UI thread
    /// against `local_project` — the same replica manual save serializes — so
    /// the re-pointed paths land in the on-disk file. The copy work runs inline
    /// (synchronous): P4 defers the background-thread split until a large-media
    /// collect shows copy cost worth offloading.
    pub(crate) fn collect_all_and_save(&mut self) {
        let Some(save_version) = self.prepare_recording_save() else { return; };
        let current_time = self.content_state.current_time;
        let Some(current_path) = self.current_project_path.clone() else {
            // Untitled: no folder to collect into. Save As first (creates the
            // project folder), then Collect can run against it.
            self.save_project_as();
            return;
        };

        self.local_project.intake().saved_playhead_time = current_time.as_f32();
        self.save_viewport_state();
        crate::project_io::snapshot_and_prune_embedded_presets(self.local_project.intake());

        match manifold_io::collect::collect_all_and_save(self.local_project.intake(), &current_path) {
            Ok(report) => {
                // The content thread owns the project the next save reads, so
                // it takes the same re-points (a non-undoable write; the copies
                // are already on disk).
                use manifold_core::change::{ChangeSet, Part};
                let moves = report.moves.clone();
                let path = current_path.clone();
                let touches = ChangeSet::of(&[Part::Media, Part::Layers, Part::Clips, Part::Effects, Part::Overrides]);
                self.send_content_cmd(ContentCommand::mutate(
                    touches,
                    Box::new(move |project| manifold_io::collect::re_point_moves(project, &moves, &path)),
                ));
                self.send_content_cmd(ContentCommand::MarkCleanAt(save_version));
                log::info!(
                    "[ProjectIO] Collect All and Save: {} copied, {} bytes, {} re-pointed, {} missing",
                    report.copied,
                    report.bytes_copied,
                    report.re_pointed,
                    report.missing,
                );
                // The save pushed the previous state into history/ — keep the
                // Revert to Snapshot menu current.
                self.refresh_history_menu();
            }
            Err(e) => {
                log::error!("[ProjectIO] Collect All and Save failed: {e}");
                crate::alerts::error(
                    "Collect Failed",
                    &format!(
                        "MANIFOLD couldn't collect media into\n{}\n\n{e}\n\n\
                         Your work is NOT on disk — check free space and try again.",
                        current_path.display()
                    ),
                );
            }
        }
    }

    /// Start offline video export — opens file save dialog, then encodes.
    pub(crate) fn start_export(&mut self) {
        if self.ws.ui_root.export_progress.is_open() || self.content_tx.is_none() {
            return;
        }
        if self.text_input.active {
            let (field, text) = self.text_input.commit();
            self.handle_text_input_commit(field, &text);
        }
        let project = &self.local_project;
        let (w, h) = (
            project.settings.output_width.max(1) as u32,
            project.settings.output_height.max(1) as u32,
        );

        let project_name = if project.project_name.is_empty() {
            "MANIFOLD_Export"
        } else {
            &project.project_name
        };
        let host = crate::file_dialogs::host(
            &self.window_registry,
            self.dialog_window_id,
            self.primary_window_id,
            self.current_project_path.as_deref(),
        );
        let Some(mut path) = crate::file_dialogs::save_file(
            crate::file_dialogs::DialogAction::ExportVideo,
            host,
            &mut self.user_prefs,
            project_name,
        ) else {
            return;
        };

        if path.extension().is_none_or(|e| e != "mp4") {
            path.set_extension("mp4");
        }
        self.last_export_path = Some(path.clone());

        let output_path = path.to_string_lossy().to_string();

        let config = manifold_media::export_config::ExportConfig {
            output_path,
            width: w,
            height: h,
            fps: project.settings.frame_rate,
            hdr: project.settings.export_hdr,
            start_beat: project.timeline.export_in_beat.0,
            end_beat: project.timeline.export_out_beat.0,
            audio_path: None, // TODO: wire from audio sync controller
            audio_start_beat: 0.0,
            audio_encoder_delay: 0.0,
            split_at_markers: project.settings.split_at_markers,
            include_gizmos: project.settings.export_gizmos,
            metadata: Vec::new(),
        };

        log::info!(
            "[Application] Starting export: {}x{} -> {}",
            w,
            h,
            config.output_path
        );
        self.ws.ui_root.export_progress.begin(
            path.file_name().map(|name| name.to_string_lossy()).as_deref().unwrap_or("Video"),
        );
        self.ws.ui_root.mark_overlay_dirty();
        self.send_content_cmd(ContentCommand::StartExport(Box::new(config)));
    }

    /// Export the current composited frame as a still image (PNG or JPEG).
    /// Opens a save dialog, then asks the content thread to grab the next
    /// rendered frame. PNG keeps alpha; JPEG (chosen via the `.jpg`/`.jpeg`
    /// extension) is opaque, for cover-art upload. The frame matches whatever
    /// is on screen, including live audio-modulation state at that instant.
    pub(crate) fn export_frame(&mut self) {
        use manifold_media::still_exporter::StillFormat;

        // Grab the frame on screen now; playback keeps running behind the dialog.
        self.send_content_cmd(ContentCommand::CaptureStill);

        let project_name = if self.local_project.project_name.is_empty() {
            "MANIFOLD_Frame"
        } else {
            &self.local_project.project_name
        };
        let host = crate::file_dialogs::host(
            &self.window_registry,
            self.dialog_window_id,
            self.primary_window_id,
            self.current_project_path.as_deref(),
        );
        let Some(mut path) = crate::file_dialogs::save_file(
            crate::file_dialogs::DialogAction::ExportFrame,
            host,
            &mut self.user_prefs,
            project_name,
        ) else {
            self.send_content_cmd(ContentCommand::DiscardStill);
            return;
        };

        // Pick format from the chosen extension; default to PNG.
        let format = match path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .as_deref()
        {
            Some("jpg") | Some("jpeg") => StillFormat::Jpeg { quality: 95 },
            Some("png") => StillFormat::Png,
            _ => {
                // No / unknown extension — default to PNG and append it.
                path.set_extension("png");
                StillFormat::Png
            }
        };

        let path_str = path.to_string_lossy().to_string();
        log::info!("[Application] Exporting frame -> {path_str}");
        self.send_content_cmd(ContentCommand::ExportFrame {
            path: path_str,
            format,
        });
    }

    /// Import a video file and place it on the timeline at the current playhead.
    /// Opens a native file dialog, probes metadata, adds to VideoLibrary, and
    /// creates a TimelineClip at the playhead beat on the active layer.
    pub(crate) fn import_video_clip(&mut self) {
        let host = crate::file_dialogs::host(
            &self.window_registry,
            self.dialog_window_id,
            self.primary_window_id,
            self.current_project_path.as_deref(),
        );
        let Some(paths) = crate::file_dialogs::pick_files(
            crate::file_dialogs::DialogAction::ImportVideo,
            host,
            &mut self.user_prefs,
        ) else {
            return;
        };
        self.import_video_files(&paths);
    }

    /// Import video files at the playhead position on the active layer, for
    /// Cmd+I and Finder drag-drop (`start_video_import`).
    pub(crate) fn import_video_files(&mut self, paths: &[std::path::PathBuf]) {
        let Some(ref content_tx) = self.content_tx else {
            return;
        };
        if let Some(rx) = start_video_import(
            paths,
            &self.local_project,
            self.active_layer_id.as_ref(),
            self.content_state.current_beat.as_f32(),
            content_tx,
        ) {
            self.import_failures_rx = Some(rx);
        }
    }

    /// Poll the in-flight video-import probe-failure channel (BUG-133) and
    /// surface any failures via the same blocking `alerts::error` dialog used
    /// for save/load/autosave failures — ticked alongside `tick_autosave`
    /// from the content-state drain (editor mode only; imports don't happen
    /// in perform mode).
    pub(crate) fn tick_import_failures(&mut self) {
        let Some(rx) = self.import_failures_rx.as_ref() else {
            return;
        };
        match rx.try_recv() {
            Ok(failures) => {
                self.import_failures_rx = None;
                if !failures.is_empty() {
                    log::error!("[Import] {} file(s) failed to import: {failures:?}", failures.len());
                    crate::alerts::error(
                        "Couldn't Import Video",
                        &format!(
                            "MANIFOLD couldn't import {} file(s):\n\n{}",
                            failures.len(),
                            failures.join("\n\n")
                        ),
                    );
                }
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.import_failures_rx = None;
            }
        }
    }

    /// Drop a still image onto the timeline as an image clip. Image clips are
    /// allowed on Video layers only: if the cursor is over a non-video layer
    /// the drop is rejected. When dropped outside the tracks area, the clip
    /// goes to the active layer (if it is a video layer) or the first video
    /// layer in the project. The image displays, aspect-fit, for the clip's
    /// duration — no decode happens here; `ImageRenderer` loads it on demand.
    pub(crate) fn import_image_file(
        &mut self,
        path: &std::path::Path,
        drop_beat: f32,
        layer_under_cursor: Option<usize>,
    ) {
        // Default image-clip length when dropped (one 4/4 bar). Trim/extend
        // afterwards like any other clip.
        const DEFAULT_IMAGE_DURATION_BEATS: f32 = 4.0;

        let Some(ref content_tx) = self.content_tx else {
            return;
        };
        let content_tx = content_tx.clone();

        let layers = &self.local_project.timeline.layers;
        let target_layer_id = if let Some(i) = layer_under_cursor {
            match layers.get(i) {
                Some(l) if l.is_video() => l.layer_id.clone(),
                Some(_) => {
                    log::info!("[Import] Image clips can only be dropped on Video layers");
                    return;
                }
                None => return,
            }
        } else {
            // Outside the tracks area → active layer if it is a video layer,
            // otherwise the first video layer in the project.
            let active_video = self
                .active_layer_id
                .as_ref()
                .and_then(|id| self.local_project.timeline.layer_index_for_id(id))
                .and_then(|i| layers.get(i))
                .filter(|l| l.is_video())
                .map(|l| l.layer_id.clone());
            match active_video.or_else(|| {
                layers
                    .iter()
                    .find(|l| l.is_video())
                    .map(|l| l.layer_id.clone())
            }) {
                Some(id) => id,
                None => {
                    log::info!("[Import] No Video layer to drop image onto");
                    return;
                }
            }
        };

        let path_str = path.to_string_lossy().to_string();
        let file_name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let clip = manifold_core::clip::TimelineClip::new_image(
            path_str,
            manifold_core::Beats::from_f32(drop_beat.max(0.0)),
            manifold_core::Beats::from_f32(DEFAULT_IMAGE_DURATION_BEATS),
        );
        log::warn!("[Import] Added image '{file_name}' at beat {drop_beat:.1}");

        let cmd = ContentCommand::Execute(Box::new(
            manifold_editing::commands::clip::AddClipCommand::new(clip, target_layer_id),
        ));
        let _ = content_tx.send(cmd);
    }

    /// Import a dropped `.glb`/`.gltf` model — or `.fbx`/`.obj`/`.dae`,
    /// converted to `.glb` through the user's installed Blender first (see
    /// `crate::blender_import`; MANIFOLD is glTF-only internally) — as a Group in
    /// a scene layer (a new one when the project has none) — the "drop a model
    /// in and it renders" gesture.
    ///
    /// `IMPORT_RESPONSIVENESS_DESIGN.md` D3: this is spawn+enqueue only. The
    /// blocking work (optional Blender convert and the CPU glTF parse) runs on a background thread
    /// (`crate::import_worker::run_import_worker`); this function never
    /// blocks the UI thread. A drop while a worker is already in flight is
    /// queued (`import_queue`), never run in parallel — one worker at a
    /// time, drained FIFO by `drain_import_progress`.
    pub(crate) fn import_model_file(
        &mut self,
        path: &std::path::Path,
        drop_beat: f32,
        layer_under_cursor: Option<usize>,
    ) {
        let under = layer_under_cursor
            .and_then(|i| self.local_project.timeline.layers.get(i))
            .map(|layer| layer.layer_id.clone());
        self.queue_import(path, crate::import_worker::ImportTarget::Drop { beat: drop_beat, under });
    }

    /// Start the worker on `path` for `target`, or queue it behind the one
    /// in flight.
    pub(crate) fn queue_import(&mut self, path: &std::path::Path, target: crate::import_worker::ImportTarget) {
        if self.content_tx.is_none() {
            return;
        }
        let req = crate::import_worker::ImportRequest {
            path: path.to_path_buf(),
            target,
            blender_path_pref: self
                .user_prefs
                .get_string(crate::blender_import::BLENDER_PATH_PREF_KEY, ""),
        };
        if self.import_worker_busy {
            self.import_queue.push_back(req);
            return;
        }
        self.spawn_import_worker(req);
    }

    /// Spawn `import_worker::run_import_worker` on a background thread for
    /// `req` and show the first stage toast immediately (D4) — never wait
    /// for the worker's own first `Stage` event, so the toast appears in
    /// the same frame the drop lands, not one frame later.
    pub(crate) fn spawn_import_worker(&mut self, req: crate::import_worker::ImportRequest) {
        self.import_worker_busy = true;
        let repo_root = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        let tx = self.import_progress_tx.clone();
        let file_name = req
            .path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "model".to_string());
        let first_stage = if req
            .path
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
            .is_some_and(|ext| crate::blender_import::is_blender_convertible_extension(&ext))
        {
            crate::import_worker::ImportStage::Converting
        } else {
            crate::import_worker::ImportStage::Parsing
        };
        self.ws
            .ui_root
            .toast
            .show(format!("Importing {file_name} — {}…", first_stage.verb()));
        std::thread::Builder::new()
            .name("manifold-import-worker".into())
            .spawn(move || crate::import_worker::run_import_worker(req, repo_root, tx))
            .expect("failed to spawn import worker thread");
    }

    /// Put `path`'s facts on the scene documents that need them, on both
    /// threads (`crate::scene_models::attach`).
    fn attach_model_facts(&mut self, layer: Option<manifold_core::LayerId>, path: String, facts: manifold_core::scene_doc::ModelFacts) {
        let Some(content_tx) = self.content_tx.clone() else {
            return;
        };
        self.local_project.edit(&mut self.ui_edits, &content_tx).mutate(manifold_editing::commands::WHOLE_INSTANCES, move |project| {
            crate::scene_models::attach(project, layer.as_ref(), &path, &facts)
        });
    }

    /// Give every scene document the facts of the model files it names,
    /// when a layer or an instance's structure moved: from the store, or
    /// read on a worker.
    pub(crate) fn sweep_model_facts(&mut self) {
        let Some(sweep) = self.model_store.sweep(&self.local_project, &self.ui_edits.stamps.parts()) else {
            return;
        };
        for (path, facts) in sweep.attach {
            self.attach_model_facts(None, path, facts);
        }
        if !sweep.read.is_empty() {
            crate::scene_models::read_on_worker(sweep.read, self.import_progress_tx.clone());
        }
    }

    /// Drain the background-import progress channel — called once per frame
    /// from the same site `tick_import_failures` is (`app_render.rs`).
    /// `Stage` re-shows the toast; `Done` runs the (unchanged) UI-thread
    /// command-dispatch tail; `Failed` shows an error toast — never silent.
    /// On `Done`/`Failed`, starts the next queued import if any (D3).
    pub(crate) fn drain_import_progress(&mut self) {
        loop {
            match self.import_progress_rx.try_recv() {
                Ok(crate::import_worker::ImportProgress::Stage { path, stage }) => {
                    let file_name = path
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| "model".to_string());
                    self.ws
                        .ui_root
                        .toast
                        .show(format!("Importing {file_name} — {}…", stage.verb()));
                }
                Ok(crate::import_worker::ImportProgress::Done {
                    path,
                    file,
                    model,
                    facts,
                    warnings,
                    conversion_report_line,
                    target,
                }) => {
                    let file_str = file.to_string_lossy().into_owned();
                    log_model_warnings(&file_str, &warnings);
                    self.model_store.arrived(file_str, facts.clone());
                    self.finish_import_model(&path, &file, *model, facts, conversion_report_line, target);
                    self.import_worker_busy = false;
                    if let Some(next) = self.import_queue.pop_front() {
                        self.spawn_import_worker(next);
                    }
                }
                Ok(crate::import_worker::ImportProgress::Facts { path, read }) => match read {
                    Ok((facts, warnings)) => {
                        log_model_warnings(&path, &warnings);
                        self.model_store.arrived(path.clone(), facts.clone());
                        self.attach_model_facts(None, path, facts);
                    }
                    // The parts keep drawing as plain meshes.
                    Err(message) => log::warn!("[scene] {path}: {message}"),
                },
                Ok(crate::import_worker::ImportProgress::Failed { path, message }) => {
                    let file_name = path
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| "model".to_string());
                    log::warn!("[Import] {} failed: {message}", path.display());
                    self.ws.ui_root.toast.show_with_accent(
                        format!("Couldn't import {file_name}: {message}"),
                        manifold_ui::color::RED_BASE,
                    );
                    self.import_worker_busy = false;
                    if let Some(next) = self.import_queue.pop_front() {
                        self.spawn_import_worker(next);
                    }
                }
                Err(crossbeam_channel::TryRecvError::Empty) => break,
                Err(crossbeam_channel::TryRecvError::Disconnected) => break,
            }
        }
    }

    /// The UI-thread tail of a successful parse (SCENE_DOCUMENT_DESIGN.md
    /// section 7.7 (Imports)). A swap reconciles the import's parts. A new
    /// model joins its scene layer as one undo step and is selected, as every
    /// Add is; a drop with no scene layer to take it makes a new one holding
    /// the base scene's camera, sun, sky and world around the model, with a
    /// default clip so it plays at once. A layer that went while the file
    /// parsed drops the import with a toast.
    fn finish_import_model(
        &mut self,
        path: &std::path::Path,
        file: &std::path::Path,
        model: manifold_core::scene_doc::ImportedModel,
        facts: manifold_core::scene_doc::ModelFacts,
        conversion_report_line: Option<String>,
        target: crate::import_worker::ImportTarget,
    ) {
        use crate::import_worker::ImportTarget;
        // A model is a scene element you hold, not a one-shot — default to four
        // 4/4 bars. Trim/extend like any other clip afterwards.
        const DEFAULT_MODEL_DURATION_BEATS: f32 = 16.0;

        let Some(ref content_tx) = self.content_tx else {
            return;
        };
        let content_tx = content_tx.clone();

        let file_name = path.file_name().map_or_else(|| "model".to_string(), |n| n.to_string_lossy().to_string());
        let file_str = file.to_string_lossy().into_owned();
        let layer_gone = |app: &mut Self| {
            log::warn!("[Import] {file_name} was dropped: its layer is gone");
            app.ws.ui_root.toast.show_with_accent(
                format!("Couldn't import {file_name}: its layer is gone"),
                manifold_ui::color::RED_BASE,
            );
        };
        let is_scene = |layer: &manifold_core::layer::Layer| layer.gen_params().is_some_and(|fx| fx.scene.is_some());

        let (into, insert_index, drop_beat) = match target {
            ImportTarget::Swap { layer, item } => {
                // The facts first, so the swap compiles once, with them.
                self.attach_model_facts(Some(layer.clone()), file_str.clone(), facts);
                let swapped = crate::scene_import::swap_model_file(&self.local_project, &layer, &item, &file_str, &model);
                match swapped {
                    Ok(swap) => {
                        self.local_project.edit(&mut self.ui_edits, &content_tx).execute_batch(swap.commands, "Swap Model File".to_string());
                        if let Some(toast) = swap.toast {
                            self.ws.ui_root.toast.show(toast);
                        }
                    }
                    Err(e) => {
                        log::warn!("[Scene Setup] Model File swap failed for {}: {e}", path.display());
                        self.ws
                            .ui_root
                            .toast
                            .show_with_accent(format!("Couldn't swap the model file: {e}"), manifold_ui::color::RED_BASE);
                    }
                }
                return;
            }
            ImportTarget::Into { layer, parent } => {
                let Some(doc) = self.local_project.scene_of(&layer) else {
                    return layer_gone(self);
                };
                // Under the selected Group, else at the top, as every Add.
                let parent = parent
                    .and_then(|id| doc.find(&id))
                    .filter(|item| manifold_core::scene_doc::can_hold(Some(item.kind()), manifold_core::scene_doc::ItemKind::Group))
                    .map(|item| item.id.clone());
                (Some((layer, parent)), 0, 0.0)
            }
            ImportTarget::Drop { beat, under } => {
                let layers = &self.local_project.timeline.layers;
                let under_index = match &under {
                    Some(id) => match layers.iter().position(|layer| layer.layer_id == *id) {
                        Some(index) => Some(index),
                        None => return layer_gone(self),
                    },
                    None => None,
                };
                let into = under_index
                    .and_then(|i| layers.get(i))
                    .filter(|layer| is_scene(layer))
                    .or_else(|| {
                        self.active_layer_id
                            .as_ref()
                            .and_then(|id| layers.iter().find(|layer| layer.layer_id == *id))
                            .filter(|layer| is_scene(layer))
                    })
                    .map(|layer| (layer.layer_id.clone(), None));
                (into, under_index.unwrap_or(0), beat)
            }
        };

        match &conversion_report_line {
            Some(line) => log::warn!("[Import] Added 3D model '{file_name}' ({line})"),
            None => log::warn!("[Import] Added 3D model '{file_name}'"),
        }

        let group = manifold_core::scene_doc::ItemId::new(manifold_core::math::short_id());
        if let Some((layer_id, parent)) = into {
            self.attach_model_facts(Some(layer_id.clone()), file_str.clone(), facts);
            let commands =
                manifold_editing::commands::scene_doc::import_model_commands(&layer_id, parent, usize::MAX, group.clone(), &file_str, &model);
            let command = Box::new(manifold_editing::command::CompositeCommand::new(commands, "Import 3D Model".to_string()));
            ContentCommand::send(
                &content_tx,
                ContentCommand::ExecuteSelecting(command, crate::edit_selection::SelectAfterEdit::NewItem(layer_id, group)),
            );
            return;
        }

        let mut local = self.local_project.edit(&mut self.ui_edits, &content_tx);
        // Insert above the layer under the cursor, or at the top when dropped
        // outside the tracks area.
        let insert_index = insert_index.min(local.timeline.layers.len());
        let mut doc = model.scene_doc(&file_str, group.clone());
        doc.models.insert(file_str.clone(), facts);
        // The model keeps the file's size and place: every item starts at
        // the file's values, as a drop into a scene does.
        let values = model
            .starting_values(&group)
            .into_iter()
            .map(|(item, property, value)| (manifold_core::scene_doc::param_id(&item, property), value))
            .collect();
        let display_name = model.name.clone();

        // Execute the layer command locally first so the generated LayerId is
        // fixed in the command instance, then send that SAME instance to the
        // content thread. Both threads then insert the identical layer (id
        // included), which lets the clip below target it deterministically.
        use manifold_editing::command::Command;
        let mut layer_cmd = manifold_editing::commands::layer::ImportModelLayerCommand::new(
            display_name,
            doc,
            values,
            insert_index,
            None,
        );
        let inserted = local.executed_by(|project| {
            layer_cmd.execute(project);
            if let Some(reason) = layer_cmd.rejection_reason() {
                log::error!("[Import] {file_name} was refused: {reason}");
                return None;
            }
            let layer_id = layer_cmd.inserted_layer_id()?;
            Some((Box::new(layer_cmd) as Box<dyn Command>, layer_id))
        });
        let Some(layer_id) = inserted else {
            self.ws.ui_root.toast.show_with_accent(
                format!("Couldn't import {file_name}: no scene layer could be made"),
                manifold_ui::color::RED_BASE,
            );
            return;
        };

        // A default generator clip on that layer, so the model renders at
        // once. Sent after the layer on the same ordered channel, so the
        // layer exists on the content thread before the clip targets it.
        let clip = manifold_core::clip::TimelineClip::new_generator(
            manifold_core::Beats::from_f32(drop_beat.max(0.0)),
            manifold_core::Beats::from_f32(DEFAULT_MODEL_DURATION_BEATS),
        );
        local.execute(Box::new(manifold_editing::commands::clip::AddClipCommand::new(clip, layer_id)));
    }

    /// Open. Delegates to ProjectIOService.open_project.
    pub(crate) fn open_project(&mut self) {
        let host = crate::file_dialogs::host(
            &self.window_registry,
            self.dialog_window_id,
            self.primary_window_id,
            self.current_project_path.as_deref(),
        );
        let action = self.project_io.open_project(host, &mut self.user_prefs);
        self.apply_project_io_action(action);
    }

    /// Open Recent. Delegates to ProjectIOService.open_recent_project.
    pub(crate) fn open_recent_project(&mut self) {
        let action = self.project_io.open_recent_project(&mut self.user_prefs);
        self.apply_project_io_action(action);
    }

    /// Shared project-load logic — called by open, open recent, and file drop.
    /// Delegates load+persist to ProjectIOService, then handles GPU/audio side-effects.
    pub(crate) fn open_project_from_path(&mut self, path: std::path::PathBuf) {
        let action = self
            .project_io
            .open_project_from_path(&path, &mut self.user_prefs);
        self.apply_project_io_action(action);
    }

    /// Apply a ProjectIOAction returned by ProjectIOService.
    /// Handles all side-effects that require Application-owned state:
    /// engine init, GPU resize, audio loading, selection reset, etc.
    pub(crate) fn apply_project_io_action(&mut self, action: ProjectIOAction) {
        // Apply loaded project (replaces host.PrepareForProjectSwitch + ApplyProject + OnProjectOpened)
        if let Some(project) = action.apply_project {
            self.autosave.project_changed();
            let t_total = std::time::Instant::now();

            // PrepareForProjectSwitch — audio-layer playback resets on the content
            // thread when it applies LoadProject (evicts the old project's voices).
            // Apply saved layout before initializing
            self.ws.ui_root.apply_project_layout(&project.settings);
            let saved_time = project.saved_playhead_time;

            // Update local_project BEFORE sending to content thread so UI
            // can rebuild the timeline in this same frame.
            *self.local_project.intake() = project.clone();
            self.ui_edits.stamps.project_replaced();
            // An open reads every model file again, a missing one included.
            self.model_store = Default::default();
            // Sync the global preset overlay to THIS project's embedded
            // presets on every apply — open, snapshot restore, and New
            // Project all pass here, so a previous project's forks never
            // leak into the next one (New Project used to skip the install).
            crate::project_io::install_project_preset_overlay(&self.local_project);
            // Suppress content thread snapshots until it processes the LoadProject
            // command (which will bump data_version above current).
            self.suppress_snapshot_until = self.content_state.data_version + 1;
            self.suppress_snapshot_set_at = self.frame_count;

            self.send_content_cmd(ContentCommand::LoadProject(Box::new(project)));

            // Restore playhead position
            if saved_time > 0.0 {
                self.send_content_cmd(ContentCommand::SeekTo(Seconds::from_f32(saved_time)));
            }

            // Resize compositor + generators to project resolution
            {
                let w = self.local_project.settings.output_width.max(1) as u32;
                let h = self.local_project.settings.output_height.max(1) as u32;
                let rs = self.local_project.settings.render_scale;
                self.send_content_cmd(ContentCommand::ResizeContent(w, h, rs));
                log::info!(
                    "[ProjectIO] GPU resize sent: {}x{} @ {:.2}x render scale",
                    w,
                    h,
                    rs
                );
            }

            self.send_content_cmd(ContentCommand::SetProject);
            self.selection.clear_selection();
            self.active_layer_id = self
                .local_project
                .timeline
                .layers
                .first()
                .map(|l| l.layer_id.clone());

            // Content thread renders at project FPS; UI always runs at display rate.
            // Don't sync UI frame timer to project FPS — that couples UI to render cadence.

            log::info!(
                "[ProjectIO] load sync: {:.1}ms (audio continues in background)",
                t_total.elapsed().as_secs_f64() * 1000.0
            );
        }

        // Set project path
        if let Some(path) = action.set_project_path {
            self.current_project_path = if path.as_os_str().is_empty() {
                None
            } else {
                Some(path)
            };
        }

        // Non-blocking load-repair notice (BUG-063,
        // `docs/PROJECT_FILE_INTEGRITY_DESIGN.md` section 3.6). Never a blocking
        // modal — that's D1's `alerts::error` refusal path for a too-new file.
        if let Some(notice) = action.notice {
            self.ws.ui_root.toast.show(notice);
        }

        // Mark clean on content thread (save succeeded)
        if action.mark_clean {
            self.send_content_cmd(ContentCommand::MarkClean);
        }

        // Send record_commands to content thread so clips exist on both sides.
        // Without this, clips added directly to local_project (e.g. MIDI import)
        // are invisible to the content thread and vanish on next sync.
        // They ran on local_project through `ProjectRef::with_sent`, which
        // stamped them.
        if !action.record_commands.is_empty() {
            if action.record_commands.len() == 1 {
                let cmd = action.record_commands.into_iter().next().unwrap();
                self.send_content_cmd(ContentCommand::Execute(cmd));
            } else {
                self.send_content_cmd(ContentCommand::ExecuteBatch(
                    action.record_commands,
                    "Drop clips".to_string(),
                ));
            }
        }

        // A load/save may have changed the recent-projects list — refresh the
        // File → Open Recent submenu so it reflects the latest order.
        self.refresh_recent_menu();
        // Same for the archive's history entries (File → Revert to Snapshot).
        self.refresh_history_menu();
    }

    /// Rebuild the File → Open Recent submenu from the current recent-projects
    /// list. Cheap (≤12 native items) and only runs on project operations, never
    /// per frame. No-op until the native menu exists.
    pub(crate) fn refresh_recent_menu(&mut self) {
        let paths = self.project_io.recent_projects();
        if let Some(menu) = self.app_menu.as_mut() {
            menu.set_recent_projects(&paths);
        }
    }

    /// How many history snapshots the Revert to Snapshot menu shows. The
    /// archive may hold more (all manual saves + the autosave cap); the menu
    /// shows the newest slice — a native menu is a browser, not an archive.
    const HISTORY_MENU_CAP: usize = 30;

    /// Rebuild the File → Revert to Snapshot submenu from the current
    /// archive's manifest. Manifest-only read (fast — no project deserialize);
    /// runs on project operations and autosave completion, never per frame.
    /// No-op until the native menu exists; empty for unsaved projects.
    pub(crate) fn refresh_history_menu(&mut self) {
        let entries: Vec<crate::menu::HistoryMenuEntry> = self
            .current_project_path
            .as_ref()
            .and_then(|p| manifold_io::archive::read_manifest(&p.to_string_lossy()))
            .map(|manifest| {
                manifest
                    .history
                    .iter()
                    // The newest entry IS the current project.json — nothing
                    // to revert to, and its history blob doesn't exist yet.
                    .filter(|e| e.hash != manifest.current_hash)
                    .take(Self::HISTORY_MENU_CAP)
                    .map(|e| crate::menu::HistoryMenuEntry {
                        hash: e.hash.clone(),
                        display: format!(
                            "{} — {}",
                            friendly_timestamp(&e.timestamp),
                            e.label.as_deref().unwrap_or(if e.is_auto {
                                "autosave"
                            } else {
                                "manual save"
                            })
                        ),
                    })
                    .collect()
            })
            .unwrap_or_default();
        if let Some(menu) = self.app_menu.as_mut() {
            menu.set_history_snapshots(&entries);
        }
    }

    /// Restore a history snapshot in place: the archive path stays current,
    /// the in-memory project becomes the snapshot. The state on disk is
    /// untouched until the next save (which journals whatever it replaces —
    /// a restore can itself be reverted).
    pub(crate) fn restore_history_snapshot(&mut self, hash: &str) {
        let Some(path) = self.current_project_path.clone() else {
            return;
        };
        // The install hook runs after a successful deserialize (D2) — a
        // failed load never touches the overlay, so there is no rollback
        // window to guard (see `open_project_from_path`).
        match manifold_io::loader::load_project_snapshot_with(
            &path,
            hash,
            crate::project_io::install_embedded_presets,
        ) {
            Ok(project) => {
                log::info!("[ProjectIO] Restored history snapshot {hash}");
                self.apply_project_io_action(ProjectIOAction {
                    apply_project: Some(project),
                    ..Default::default()
                });
            }
            Err(e) => {
                log::error!("[ProjectIO] Snapshot restore failed: {e}");
                crate::alerts::error(
                    "Restore Failed",
                    &format!("Couldn't restore the snapshot:\n{e}"),
                );
            }
        }
    }

    /// Open a history snapshot as a detached copy: untitled (no project
    /// path), so saving it asks for a location instead of overwriting the
    /// original archive.
    pub(crate) fn open_history_snapshot_copy(&mut self, hash: &str) {
        let Some(path) = self.current_project_path.clone() else {
            return;
        };
        match manifold_io::loader::load_project_snapshot_with(
            &path,
            hash,
            crate::project_io::install_embedded_presets,
        ) {
            Ok(mut project) => {
                project.project_name = format!("{} (snapshot)", project.project_name);
                // Detach from the source archive so a save can't clobber it.
                project.last_saved_path = String::new();
                log::info!("[ProjectIO] Opened history snapshot {hash} as a copy");
                self.apply_project_io_action(ProjectIOAction {
                    apply_project: Some(project),
                    // Empty path → apply_project_io_action clears
                    // current_project_path (untitled).
                    set_project_path: Some(std::path::PathBuf::new()),
                    ..Default::default()
                });
            }
            Err(e) => {
                log::error!("[ProjectIO] Snapshot copy failed: {e}");
                crate::alerts::error(
                    "Open Copy Failed",
                    &format!("Couldn't open the snapshot:\n{e}"),
                );
            }
        }
    }

    /// Open a decorated HDR output window (default size = project resolution).
    /// Resizable — content always renders at project resolution with letterbox/pillarbox.
    /// Native title bar allows drag-to-monitor and macOS fullscreen (green button).
    /// Surface is Rgba16Float with EDR enabled via GpuSurface.
    /// Unity: NativeMonitorWindowController.cs + MonitorWindowPlugin.mm.
    pub(crate) fn open_output_window(
        &mut self,
        event_loop: &ActiveEventLoop,
        name: &str,
        display_index: Option<usize>,
        presentation: bool,
    ) {
        // Guard: don't open output window if GPU isn't initialized.
        if self.gpu.is_none() {
            return;
        }

        // Resolve target monitor.
        // If display_index is given, use that. Otherwise pick first non-primary monitor.
        // If only one monitor, use it (user's primary display).
        let monitors: Vec<_> = event_loop.available_monitors().collect();
        let target_monitor = if let Some(idx) = display_index {
            monitors.get(idx).cloned()
        } else {
            // Pick non-primary: primary is usually index 0
            if monitors.len() > 1 {
                Some(monitors[1].clone())
            } else {
                monitors.first().cloned()
            }
        };

        let monitor = match target_monitor {
            Some(m) => m,
            None => {
                log::error!("[OutputWindow] No monitors available");
                return;
            }
        };

        let mon_phys_size = monitor.size();
        let mon_pos = monitor.position();
        let scale_factor = monitor.scale_factor();
        let mon_name = monitor.name().unwrap_or_else(|| "Unknown".to_string());

        // Log all available monitors
        for (i, m) in monitors.iter().enumerate() {
            let s = m.size();
            let p = m.position();
            let sf = m.scale_factor();
            let n = m.name().unwrap_or_else(|| "?".to_string());
            log::debug!(
                "[OutputWindow] Monitor {}: '{}' physical={}x{} pos=({},{}) scale={:.2} logical={}x{}",
                i,
                n,
                s.width,
                s.height,
                p.x,
                p.y,
                sf,
                (s.width as f64 / sf) as u32,
                (s.height as f64 / sf) as u32
            );
        }

        // Use logical coordinates — macOS window placement uses logical (point) coords.
        // Physical pixels / scale_factor = logical points.
        let logical_w = mon_phys_size.width as f64 / scale_factor;
        let logical_h = mon_phys_size.height as f64 / scale_factor;
        let logical_x = mon_pos.x as f64 / scale_factor;
        let logical_y = mon_pos.y as f64 / scale_factor;

        log::debug!(
            "[OutputWindow] Target '{}': logical={:.0}x{:.0} at ({:.0},{:.0}), physical={}x{}, scale={:.2}",
            mon_name,
            logical_w,
            logical_h,
            logical_x,
            logical_y,
            mon_phys_size.width,
            mon_phys_size.height,
            scale_factor
        );

        let (proj_w, proj_h) = (
            self.local_project.settings.output_width.max(1) as f64,
            self.local_project.settings.output_height.max(1) as f64,
        );
        let center_x = logical_x + (logical_w - proj_w) * 0.5;
        let center_y = logical_y + (logical_h - proj_h) * 0.5;

        let attrs = if presentation {
            winit::window::Window::default_attributes()
                .with_title(format!("MANIFOLD - {}", name))
                .with_decorations(false)
                .with_resizable(false)
                .with_position(winit::dpi::Position::Logical(
                    winit::dpi::LogicalPosition::new(logical_x, logical_y),
                ))
                .with_inner_size(winit::dpi::LogicalSize::new(logical_w, logical_h))
        } else {
            winit::window::Window::default_attributes()
                .with_title(format!("MANIFOLD - {}", name))
                .with_position(winit::dpi::Position::Logical(
                    winit::dpi::LogicalPosition::new(center_x, center_y),
                ))
                .with_inner_size(winit::dpi::LogicalSize::new(proj_w, proj_h))
        };

        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                log::error!("[OutputWindow] Failed to create window: {e}");
                return;
            }
        };

        // No window level override. Setting setLevel:25 triggers macOS
        // Direct Display mode when the window covers the full display,
        // which destabilizes CVDisplayLink callback cadence (12-20ms
        // instead of steady 16.67ms). A borderless window at full display
        // size is visually identical — the menu bar only appears on the
        // display the cursor is on, not on a dedicated output TV.

        // Tahoe draws a 1px border around shadowed windows even when
        // borderless — presentation windows stay flush without it.
        #[cfg(target_os = "macos")]
        if presentation {
            crate::edr_surface::set_window_shadow(&window, false);
        }

        let id = window.id();

        let capabilities = crate::edr_surface::query_window_capabilities(&window);
        self.output_display_capabilities = Some(capabilities);

        // Direct present: content thread acquires drawables and presents
        // in its own command buffer. displaySyncEnabled handles vsync.
        // No CVDisplayLink, no IOSurface intermediary.
        #[cfg(target_os = "macos")]
        if let Some(gpu) = &self.gpu {
            let size = window.inner_size();
            let surface = gpu.device.create_surface(
                &*window,
                size.width.max(1),
                size.height.max(1),
                manifold_gpu::GpuTextureFormat::Rgba16Float,
                true,
            );
            self.send_content_cmd(crate::content_command::ContentCommand::SetOutputSurface(
                surface,
            ));
            self.send_content_cmd(crate::content_command::ContentCommand::UpdateDisplayCapabilities {
                destination: manifold_compositor::presentation::DisplayDestination::Output, capabilities,
            });
        }

        let state = WindowState {
            window,
            surface: None, // NativeOutputPresenter owns the CAMetalLayer.
            role: WindowRole::Output { presentation },
        };

        self.window_registry.add(id, state);

        let (proj_w_u32, proj_h_u32) = (proj_w as u32, proj_h as u32);
        log::info!(
            "[OutputWindow] Opened '{}' on '{}' (drawable={}x{}, Rgba16Float, EDR={:.2}x, pixel-perfect, presentation={})",
            name,
            mon_name,
            proj_w_u32,
            proj_h_u32,
            capabilities.current().value(),
            presentation,
        );
    }

    /// Open the node-graph editor window — or, if it is already open,
    /// summon it to the front.
    ///
    /// First open: sized at 75% of the primary window's logical inner
    /// size, placed by winit. Subsequent opens restore the position and
    /// size the window had when it was last closed
    /// ([`Self::graph_editor_geometry`]), so it lands where the user left
    /// it. The window stays first-class (AltTab / Cmd-` see it as its own
    /// window); this method just guarantees the toggle always brings it
    /// forward instead of no-opping, which is what stops it getting lost
    /// behind the main window.
    pub(crate) fn open_graph_editor(&mut self, event_loop: &ActiveEventLoop) {
        if self.graph_editor.is_some() {
            // Already open — bring it to the front rather than no-op. It is
            // a first-class window, so a click on the main window can leave
            // it behind; re-pressing Cmd+Shift+G (or clicking the card
            // button) must always re-summon it.
            if let Some(wid) = self.graph_editor_window_id
                && let Some(ws) = self.window_registry.get(&wid)
            {
                ws.window.set_minimized(false);
                ws.window.focus_window();
            }
            return;
        }

        let primary_id = match self.primary_window_id {
            Some(id) => id,
            None => {
                log::warn!("[GraphEditor] No primary window — cannot open editor");
                return;
            }
        };

        let (logical_w, logical_h, scale) = self
            .window_registry
            .get(&primary_id)
            .map(|ws| {
                let s = ws.window.inner_size();
                let sf = ws.window.scale_factor();
                let (w, h) = crate::editor_frame::first_open_logical_size(
                    s.width as f64 / sf,
                    s.height as f64 / sf,
                );
                (w, h, sf)
            })
            .unwrap_or((960, 540, 1.0));

        let mut attrs = winit::window::Window::default_attributes()
            .with_title("MANIFOLD — Graph Editor")
            .with_inner_size(winit::dpi::LogicalSize::new(logical_w, logical_h));
        // Reopen where the user last left it (position + size), if known.
        if let Some((pos, size)) = self.graph_editor_geometry {
            attrs = attrs.with_position(pos).with_inner_size(size);
        }

        let window = match event_loop.create_window(attrs) {
            Ok(w) => std::sync::Arc::new(w),
            Err(e) => {
                log::error!("[GraphEditor] Failed to create window: {e}");
                return;
            }
        };

        let size = window.inner_size();
        let wid = window.id();

        let Some(gpu) = &self.gpu else {
            log::error!("[GraphEditor] GPU not initialized");
            return;
        };

        // Phase 3: per-window CVDisplayLink drives editor pacing, so we
        // can enable displaySyncEnabled — `next_drawable()` blocks until
        // the editor's own vsync, which our display link wakes us on.
        let surface = gpu.device.create_surface(
            &*window,
            size.width.max(1),
            size.height.max(1),
            manifold_compositor::presentation::UI_FORMAT,
            true,
        );
        surface.set_maximum_drawable_count(3);
        surface.set_presents_with_transaction(false);
        // EDR colorspace: the UI renderer writes linear-light values
        // (`Color32::srgb_to_linear` in node.rs) into this surface, same as
        // the main window. Without `configure_edr()`, macOS treats those
        // bytes as already gamma-encoded and skips the sRGB transfer
        // function on display — every color in this window reads uniformly
        // darker/flatter than the main window. Mirrors `app.rs`'s primary
        // window setup.
        surface.configure_edr();
        let capabilities = crate::edr_surface::query_window_capabilities(&window);
        self.graph_display_capabilities = capabilities;
        self.send_content_cmd(crate::content_command::ContentCommand::UpdateDisplayCapabilities {
            destination: manifold_compositor::presentation::DisplayDestination::GraphEditor,
            capabilities,
        });

        let Some(gpu) = &self.gpu else { return; };
        let offscreen = gpu.device.create_texture(&manifold_gpu::GpuTextureDesc {
            width: size.width.max(1),
            height: size.height.max(1),
            depth: 1,
            format: manifold_compositor::presentation::UI_FORMAT,
            dimension: manifold_gpu::GpuTextureDimension::D2,
            usage: manifold_gpu::GpuTextureUsage::RENDER_TARGET_FULL,
            label: "Graph Editor Offscreen",
            mip_levels: 1,
        });

        self.window_registry.add(
            wid,
            WindowState {
                window: std::sync::Arc::clone(&window),
                surface: Some(surface),
                role: WindowRole::Workspace,
            },
        );

        // The window-free half of the open, shared with the ui-snap script
        // runner (`editor_frame.rs`): the workspace, the canvas and the
        // inspector column projected from the same snapshot the main window
        // reads (docs/GRAPH_EDITOR_INSPECTOR_UNIFICATION.md).
        let active_idx = self
            .active_layer_id
            .as_ref()
            .and_then(|id| self.local_project.timeline.find_layer_index_by_id(id));
        #[cfg_attr(not(target_os = "macos"), allow(unused_mut))]
        let (mut ws, canvas) = crate::editor_frame::open_editor_workspace(
            Some(offscreen),
            &self.local_project,
            active_idx,
            &self.selection,
            &self.content_state,
        );
        #[cfg(target_os = "macos")]
        {
            ws.ui_display_link = Some(crate::display_link::UiDisplayLink::new(window));
        }
        self.graph_editor = Some(ws);
        self.graph_editor_window_id = Some(wid);
        self.graph_canvas = Some(canvas);

        log::info!(
            "[GraphEditor] Opened ({}x{} logical, scale {:.2})",
            logical_w,
            logical_h,
            scale,
        );
    }

    /// Tear down the graph editor window. Drops its workspace and
    /// removes the window from the registry. Safe to call from
    /// `WindowEvent::CloseRequested` for the editor's `WindowId`.
    pub(crate) fn close_graph_editor(&mut self) {
        // Stop display link FIRST — its callback may call CFRunLoopWakeUp
        // and the cleanup thread can outlive the window. CVDisplayLinkStop
        // blocks until the in-flight callback finishes, so we drop the
        // link before tearing down the surface.
        #[cfg(target_os = "macos")]
        if let Some(ws) = self.graph_editor.as_mut() {
            ws.ui_display_link = None;
        }
        // Remember where the user left the window so a later reopen lands
        // in the same place and size instead of winit's default cascade.
        if let Some(wid) = self.graph_editor_window_id {
            let geom = self.window_registry.get(&wid).and_then(|ws| {
                ws.window
                    .outer_position()
                    .ok()
                    .map(|pos| (pos, ws.window.inner_size()))
            });
            if geom.is_some() {
                self.graph_editor_geometry = geom;
            }
        }
        if let Some(wid) = self.graph_editor_window_id.take() {
            self.window_registry.remove(&wid);
        }
        let gesture = self.graph_editor.as_mut().and_then(|ed| ed.viewport_gizmo_drag.take());
        self.end_gizmo_gesture(gesture);
        self.graph_editor = None;
        self.graph_canvas = None;
        // Stop per-node thumbnail capture on the content thread now the editor
        // is gone, so a live show pays nothing for it. An empty visible set
        // turns the atlas dump off.
        if !self.last_atlas_visible_sent.is_empty() {
            self.send_content_cmd(ContentCommand::SetNodeAtlasVisible(Vec::new()));
            self.last_atlas_visible_sent.clear();
        }
        self.atlas_visible_snap = None;
        self.atlas_visible_scope.clear();
        // Clear the Phase 4 caches alongside — `watched_graph_target`
        // is the gate for the palette being active and the sole identity
        // for every editor-card edit, so a stale value would let the user
        // trigger commands against a dead effect or generator if they
        // reopened the window on a different one.
        self.watched_graph_target = None;
        self.watched_catalog_default = None;
        self.editor_graph.catalog_default_changed();
        // Tell the content thread to stop snapshotting any graph —
        // saves the per-frame walk while no editor is open. Cover
        // both the effect-graph and generator-graph watchers since
        // either could be active when the editor closes.
        self.send_content_cmd(ContentCommand::WatchEffectGraph(None));
        self.send_content_cmd(ContentCommand::WatchGeneratorGraph(None));
        log::info!("[GraphEditor] Closed");
    }
}

/// Menu-friendly form of the archive's ISO-8601 UTC timestamps:
/// "2026-07-03T04:12:33.123Z" → "2026-07-03 04:12 UTC". Anything that
/// doesn't look like ISO-8601 is shown as-is — never hide an entry over
/// a timestamp format.
fn friendly_timestamp(iso: &str) -> String {
    if iso.len() >= 16 && iso.as_bytes().get(10) == Some(&b'T') {
        format!("{} {} UTC", &iso[..10], &iso[11..16])
    } else {
        iso.to_string()
    }
}

#[cfg(test)]
mod bug_133_import_probe_rejection_tests {
    use super::build_video_import_batch;

    /// BUG-133: `SUPPORTED_EXTENSIONS` deliberately still lists `.webm`/`.avi`
    /// (Peter rejected trimming the list) — the probe is the real gate. A
    /// file with a `.webm` extension that AVFoundation can't actually decode
    /// (here: garbage bytes, standing in for a VP8/VP9 webm with no matching
    /// codec) must be REJECTED with a clear message, never silently dropped
    /// with only a log line.
    #[test]
    fn probe_failing_webm_file_yields_a_rejection_message_not_a_silent_drop() {
        let dir = std::env::temp_dir().join(format!(
            "manifold-bug133-test-{}-{}",
            std::process::id(),
            manifold_core::short_id()
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let fake_webm = dir.join("not_actually_a_video.webm");
        std::fs::write(&fake_webm, b"this is not video data, just plain bytes")
            .expect("write fixture file");

        let layer_id = manifold_core::LayerId::new("test-layer");
        let (video_clips, commands, failures) =
            build_video_import_batch(std::slice::from_ref(&fake_webm), 120.0, 0.0, &layer_id);

        // Not a silent success: no clip, no library entry, no AddClipCommand.
        assert!(
            video_clips.is_empty(),
            "a probe-failing file must not produce a VideoClip library entry"
        );
        assert!(
            commands.is_empty(),
            "a probe-failing file must not produce an AddClipCommand"
        );

        // Not silently dropped: exactly one clear, user-facing rejection.
        assert_eq!(
            failures.len(),
            1,
            "a probe-failing file must yield exactly one rejection message, got: {failures:?}"
        );
        assert!(
            failures[0].contains("not_actually_a_video.webm"),
            "rejection message must name the file: {}",
            failures[0]
        );
        assert!(
            failures[0].to_lowercase().contains("codec")
                || failures[0].to_lowercase().contains("not supported"),
            "rejection message must explain it's a codec/support issue, not a mystery: {}",
            failures[0]
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A batch with both a good and a bad file must reject the bad one and
    /// still import the good one — a partial-batch failure is not silent
    /// either (the whole point of routing failures through their own
    /// channel send after the successful-import send).
    #[test]
    fn probe_failure_does_not_swallow_the_rest_of_the_batch() {
        let dir = std::env::temp_dir().join(format!(
            "manifold-bug133-test-mixed-{}-{}",
            std::process::id(),
            manifold_core::short_id()
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let fake_webm = dir.join("garbage.webm");
        std::fs::write(&fake_webm, b"not video").expect("write fixture file");
        let also_missing = dir.join("does_not_exist.avi");

        let layer_id = manifold_core::LayerId::new("test-layer");
        let (video_clips, commands, failures) = build_video_import_batch(
            &[fake_webm, also_missing],
            120.0,
            0.0,
            &layer_id,
        );

        assert!(video_clips.is_empty());
        assert!(commands.is_empty());
        assert_eq!(
            failures.len(),
            2,
            "every probe-failing file in the batch must be individually rejected: {failures:?}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
