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
            manifold_editing::commands::clip::AddClipCommand::new(clip, layer_id.clone(), spb),
        )));

        log::warn!(
            "[Import] Added '{file_name}' at beat {beat:.1} \
             ({duration_secs:.1}s → {duration_beats:.1} beats, {res_w}x{res_h})"
        );

        beat += duration_beats;
    }

    (video_clips, commands, failures)
}

impl Application {
    // ── Project I/O — delegates to ProjectIOService ────────────────────

    /// Persist current viewport scroll + zoom and UI collapse states into project settings.
    fn save_viewport_state(&mut self) {
        self.local_project.settings.viewport_scroll_x_beats =
            self.ws.ui_root.viewport.scroll_x_beats().as_f32();
        self.local_project.settings.viewport_scroll_y_px = self.ws.ui_root.viewport.scroll_y_px();
        self.local_project.settings.viewport_pixels_per_beat =
            self.ws.ui_root.viewport.pixels_per_beat();

        // Persist panel sizing. These read from the live UI layout (which is
        // never clobbered by content-thread snapshots) rather than relying on a
        // drag-end write into local_project.settings — that write gets wiped by
        // the next snapshot clone before save can capture it.
        self.local_project.settings.inspector_width = self.ws.ui_root.layout.inspector_width;
        self.local_project.settings.timeline_height_percent =
            self.ws.ui_root.layout.timeline_split_ratio;

        // Persist inspector collapse states
        self.local_project.settings.macros_collapsed =
            self.ws.ui_root.inspector.macros_panel().is_collapsed();
        self.local_project.settings.master_chrome_collapsed =
            self.ws.ui_root.inspector.master_chrome().is_collapsed();
        self.local_project.settings.layer_chrome_collapsed =
            self.ws.ui_root.inspector.layer_chrome().is_collapsed();
        self.local_project.settings.clip_chrome_collapsed =
            self.ws.ui_root.inspector.clip_chrome().is_collapsed();
    }

    /// Save. Delegates to ProjectIOService.save_project.
    pub(crate) fn save_project(&mut self) {
        let current_time = self.content_state.current_time;
        let current_path = self.current_project_path.clone();
        // Save the local project snapshot (best effort — authoritative is on content thread)
        self.local_project.saved_playhead_time = current_time.as_f32();
        self.save_viewport_state();
        crate::project_io::snapshot_and_prune_embedded_presets(&mut self.local_project);
        if let Some(path) = current_path.as_deref() {
            match manifold_io::saver::save_project(&mut self.local_project, path, None, false) {
                Ok(()) => {
                    self.send_content_cmd(ContentCommand::MarkClean);
                    log::info!("[ProjectIO] Saved to {}", path.display());
                    // The save pushed the previous state into history/ —
                    // keep the Revert to Snapshot menu current.
                    self.refresh_history_menu();
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
                }
            }
        } else {
            self.save_project_as();
        }
    }

    /// Save As. Delegates to ProjectIOService.save_project_as.
    pub(crate) fn save_project_as(&mut self) {
        self.send_content_cmd(ContentCommand::PauseRendering);
        let current_time = self.content_state.current_time;
        self.local_project.saved_playhead_time = current_time.as_f32();
        self.save_viewport_state();
        let action = self.project_io.save_project_as(
            &mut self.local_project,
            current_time.as_f32(),
            &mut EditingService::new(), // placeholder — mark clean via content thread
            &mut self.user_prefs,
        );
        self.send_content_cmd(ContentCommand::ResumeRendering);
        self.apply_project_io_action(action);
    }

    /// Start offline video export — opens file save dialog, then encodes.
    pub(crate) fn start_export(&mut self) {
        let project = &self.local_project;
        let (w, h) = (
            project.settings.output_width.max(1) as u32,
            project.settings.output_height.max(1) as u32,
        );

        // Pause rendering while native file dialog is open (macOS GPU contention)
        self.send_content_cmd(ContentCommand::PauseRendering);

        // Restore last-used directory and filename from persisted prefs.
        let saved_dir = crate::dialog_path_memory::get_last_directory(
            crate::dialog_path_memory::DialogContext::ExportMP4,
            &mut self.user_prefs,
        );
        let saved_name = self
            .user_prefs
            .get_string("MANIFOLD_LastExportFileName", "");
        log::debug!("[Export] Dialog prefs: dir={saved_dir:?} name={saved_name:?}");

        let default_name = if !saved_name.is_empty() {
            saved_name
        } else {
            let project_name = if project.project_name.is_empty() {
                "MANIFOLD_Export"
            } else {
                &project.project_name
            };
            format!("{project_name}.mp4")
        };

        let mut dialog = rfd::FileDialog::new()
            .set_title("Export Video")
            .add_filter("MP4 Video", &["mp4"])
            .set_file_name(&default_name);

        if !saved_dir.is_empty() {
            dialog = dialog.set_directory(&saved_dir);
        } else {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
            let desktop = std::path::Path::new(&home).join("Desktop");
            if desktop.exists() {
                dialog = dialog.set_directory(&desktop);
            }
        }

        let result = dialog.save_file();
        self.send_content_cmd(ContentCommand::ResumeRendering);

        let Some(mut path) = result else {
            return; // User cancelled
        };

        // Ensure .mp4 extension
        if path.extension().is_none_or(|e| e != "mp4") {
            path.set_extension("mp4");
        }

        // Persist directory and filename for next export dialog (survives app restart).
        let path_str = path.to_string_lossy();
        crate::dialog_path_memory::remember_directory(
            crate::dialog_path_memory::DialogContext::ExportMP4,
            &path_str,
            &mut self.user_prefs,
        );
        if let Some(name) = path.file_name() {
            self.user_prefs
                .set_string("MANIFOLD_LastExportFileName", &name.to_string_lossy());
            self.user_prefs.save();
        }
        self.last_export_path = Some(path.clone());

        let output_path = path.to_string_lossy().to_string();

        let config = manifold_media::export_config::ExportConfig {
            output_path,
            width: w,
            height: h,
            fps: project.settings.frame_rate,
            hdr: project.settings.export_hdr,
            start_beat: project.timeline.export_in_beat.as_f32(),
            end_beat: project.timeline.export_out_beat.as_f32(),
            audio_path: None, // TODO: wire from audio sync controller
            audio_start_beat: 0.0,
            audio_encoder_delay: 0.0,
        };

        log::info!(
            "[Application] Starting export: {}x{} -> {}",
            w,
            h,
            config.output_path
        );
        self.send_content_cmd(ContentCommand::StartExport(Box::new(config)));
    }

    /// Export the current composited frame as a still image (PNG or JPEG).
    /// Opens a save dialog, then asks the content thread to grab the next
    /// rendered frame. PNG keeps alpha; JPEG (chosen via the `.jpg`/`.jpeg`
    /// extension) is opaque, for cover-art upload. The frame matches whatever
    /// is on screen, including live audio-modulation state at that instant.
    pub(crate) fn export_frame(&mut self) {
        use manifold_media::still_exporter::StillFormat;

        // Pause rendering while the native file dialog is open (GPU contention).
        self.send_content_cmd(ContentCommand::PauseRendering);

        let saved_dir = crate::dialog_path_memory::get_last_directory(
            crate::dialog_path_memory::DialogContext::ExportImage,
            &mut self.user_prefs,
        );
        let project_name = if self.local_project.project_name.is_empty() {
            "MANIFOLD_Frame"
        } else {
            &self.local_project.project_name
        };
        let default_name = format!("{project_name}.png");

        let mut dialog = rfd::FileDialog::new()
            .set_title("Export Frame")
            .add_filter("PNG Image", &["png"])
            .add_filter("JPEG Image", &["jpg", "jpeg"])
            .set_file_name(&default_name);

        if !saved_dir.is_empty() {
            dialog = dialog.set_directory(&saved_dir);
        } else {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
            let desktop = std::path::Path::new(&home).join("Desktop");
            if desktop.exists() {
                dialog = dialog.set_directory(&desktop);
            }
        }

        let result = dialog.save_file();
        self.send_content_cmd(ContentCommand::ResumeRendering);

        let Some(mut path) = result else {
            return; // User cancelled
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
        crate::dialog_path_memory::remember_directory(
            crate::dialog_path_memory::DialogContext::ExportImage,
            &path_str,
            &mut self.user_prefs,
        );
        self.user_prefs.save();

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
        self.send_content_cmd(ContentCommand::PauseRendering);

        let mut dialog = rfd::FileDialog::new()
            .set_title("Import Video")
            .add_filter("Video Files", &["mp4", "mov", "webm", "avi"]);

        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        let desktop = std::path::Path::new(&home).join("Desktop");
        if desktop.exists() {
            dialog = dialog.set_directory(&desktop);
        }

        let result = dialog.pick_files();
        self.send_content_cmd(ContentCommand::ResumeRendering);

        let Some(paths) = result else {
            return; // User cancelled
        };

        self.import_video_files(&paths);
    }

    /// Import video files at the playhead position on the active layer.
    /// Shared by Cmd+I file dialog and drag-drop.
    /// Non-blocking: metadata probe runs on a background thread, commands are
    /// sent directly to the content thread. UI picks up new clips via
    /// project_snapshot sync.
    pub(crate) fn import_video_files(&mut self, paths: &[std::path::PathBuf]) {
        if paths.is_empty() {
            return;
        }

        let Some(ref content_tx) = self.content_tx else {
            return;
        };
        let content_tx = content_tx.clone();

        let bpm = self.local_project.settings.bpm.0;
        let insert_beat = self.content_state.current_beat.as_f32();
        let layer_id = self
            .active_layer_id
            .as_ref()
            .and_then(|lid| {
                self.local_project
                    .timeline
                    .layer_index_for_id(lid)
                    .and_then(|i| self.local_project.timeline.layers.get(i))
                    .map(|l| l.layer_id.clone())
            })
            .or_else(|| {
                self.local_project
                    .timeline
                    .layers
                    .first()
                    .map(|l| l.layer_id.clone())
            });

        let Some(layer_id) = layer_id else {
            return;
        };

        let paths = paths.to_vec();

        // BUG-133: the file-dialog filter deliberately stays broad (mp4/mov/
        // webm/avi) — trimming it was considered and rejected. AVFoundation
        // (the only decoder) generally can't open webm (no VP8/VP9) and has
        // patchy avi support, so the probe below is the real gate: a
        // probe-failing file must reject the import with a clear message,
        // never just a log line the user never sees (no-silent-fallbacks).
        let (failure_tx, failure_rx) = std::sync::mpsc::channel::<Vec<String>>();
        self.import_failures_rx = Some(failure_rx);

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
            let _ = content_tx.send(ContentCommand::MutateProject(Box::new(move |p| {
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

        let spb = 60.0 / self.local_project.settings.bpm.0;
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
            manifold_editing::commands::clip::AddClipCommand::new(clip, target_layer_id, spb),
        ));
        let _ = content_tx.send(cmd);
    }

    /// Import a dropped `.glb`/`.gltf` model — or `.fbx`/`.obj`/`.dae`,
    /// converted to `.glb` through the user's installed Blender first (see
    /// `crate::blender_import`; MANIFOLD is glTF-only internally) — as a new
    /// generator layer whose per-instance graph renders the model, plus a
    /// default generator clip so it plays immediately — the "drop a model in
    /// and it renders" gesture.
    ///
    /// The graph is assembled by
    /// [`manifold_renderer::node_graph::gltf_import::assemble_import_graph`]
    /// (one CPU parse here; the graph's `gltf_mesh_source`/`gltf_texture_source`
    /// nodes re-parse on their own background threads at render time). The
    /// layer install routes through [`ImportModelLayerCommand`], the clip
    /// through the shared `AddClipCommand`.
    pub(crate) fn import_model_file(
        &mut self,
        path: &std::path::Path,
        drop_beat: f32,
        layer_under_cursor: Option<usize>,
    ) {
        use manifold_editing::command::Command;

        // A model is a scene element you hold, not a one-shot — default to four
        // 4/4 bars. Trim/extend like any other clip afterwards.
        const DEFAULT_MODEL_DURATION_BEATS: f32 = 16.0;

        let Some(ref content_tx) = self.content_tx else {
            return;
        };
        let content_tx = content_tx.clone();

        // FBX/.obj/.dae (MANIFOLD is glTF-only internally, per
        // IMPORT_ANYTHING_WAVE_DESIGN.md Lane W3): convert through the
        // user's installed Blender first, then continue with the produced
        // `.glb` through the exact same path a native glTF drop takes. This
        // is a blocking subprocess call on the calling (UI) thread — the
        // same shape `assemble_import_graph` below already uses for its
        // blocking CPU parse, so it's one function seam, not new UI-thread
        // plumbing.
        let mut conversion_report_line: Option<String> = None;
        let import_path: std::borrow::Cow<std::path::Path> = match path
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
        {
            Some(ext) if crate::blender_import::is_blender_convertible_extension(&ext) => {
                let repo_root =
                    std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
                match crate::blender_import::convert_via_blender(&self.user_prefs, &repo_root, path)
                {
                    Ok(outcome) => {
                        conversion_report_line = Some(match &outcome.blender_version {
                            Some(v) => format!(
                                "converted from {} via Blender {v}",
                                crate::blender_import::source_format_label(&ext)
                            ),
                            None => format!(
                                "converted from {} via Blender",
                                crate::blender_import::source_format_label(&ext)
                            ),
                        });
                        std::borrow::Cow::Owned(outcome.glb_path)
                    }
                    Err(e) => {
                        log::warn!(
                            "[Import] Blender conversion failed for {}: {e}",
                            path.display()
                        );
                        return;
                    }
                }
            }
            _ => std::borrow::Cow::Borrowed(path),
        };
        let path = import_path.as_ref();

        // Parse + assemble on the calling thread. Errors (no geometry with
        // materials, unreadable file) abort the drop with a log rather than
        // leaving a half-built layer behind.
        let (mut graph, report) =
            match manifold_renderer::node_graph::gltf_import::assemble_import_graph(path) {
                Ok(pair) => pair,
                Err(e) => {
                    log::warn!("[Import] glTF import failed for {}: {e}", path.display());
                    return;
                }
            };

        // The assembler is code and has bugs (GRAPH_TOOLING_DESIGN D6): run its
        // output through the same validate_def pipeline the runtime loader
        // takes BEFORE it reaches the project, and abort on the existing
        // import-error path (log + return, same as the parse-failure branch
        // above) rather than let a malformed def surface later as wrong pixels
        // or a load failure far from the cause. Never a silent partial
        // import — errors here are fatal to the drop, not warnings.
        // IMPORT_RESPONSIVENESS_DESIGN.md D2/P2: never a fresh `GpuDevice`
        // per import — reuse the app's real UI-side device, or the process's
        // one lazily-created fallback if `resumed()` hasn't run yet. See
        // `Application::validation_gpu_device`'s doc comment.
        let validation_device = self.validation_gpu_device();
        let validation_registry = manifold_renderer::node_graph::PrimitiveRegistry::with_builtin();
        let validation = manifold_renderer::node_graph::validate_def(
            &graph,
            &validation_registry,
            manifold_renderer::node_graph::ValidateKind::Generator,
            &validation_device,
        );
        if !validation.is_valid() {
            let messages: Vec<String> =
                validation.errors.iter().map(|issue| issue.message.clone()).collect();
            log::warn!(
                "[Import] glTF import failed for {}: assembled graph failed validation: {}",
                path.display(),
                messages.join("; ")
            );
            return;
        }

        let Some(meta) = graph.preset_metadata.as_ref() else {
            log::warn!("[Import] assembled glTF graph carries no preset metadata — aborting");
            return;
        };
        let base_id = meta.id.clone();
        let display_name = if meta.display_name.is_empty() {
            path.file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "Model".to_string())
        } else {
            meta.display_name.clone()
        };

        // Mint a project-unique id only on collision, so the common case keeps
        // the clean sanitized stem (`azalea`) and only a genuine clash gets a
        // `#N` suffix. The model's graph becomes a project-embedded preset the
        // layer TRACKS (D9) — an id that resolves in no catalog is BUG-016, so
        // the id must be stamped onto the def, registered, and installed into
        // the catalog overlay before any frame reads it.
        let preset_id = if self.local_project.embedded_preset(&base_id).is_some() {
            self.local_project.mint_embedded_preset_id(base_id.as_str())
        } else {
            base_id
        };
        if let Some(m) = graph.preset_metadata.as_mut() {
            m.id = preset_id.clone();
        }
        let embedded = manifold_core::project::EmbeddedPreset {
            kind: manifold_core::preset_def::PresetKind::Generator,
            def: graph,
            origin: manifold_core::project::EmbeddedOrigin::Saved,
        };
        // Register + install the overlay BEFORE creating the layer. The core
        // preset-definition registry is process-global, so installing here (on
        // the UI thread) populates it for BOTH threads: the local execute below
        // and the content thread's later execute of the same command box each
        // run `new_generator` → `init_defaults`, which reads that global
        // registry to seed the curated card values. Installing after the local
        // execute (the doc's tentative option) would leave the two threads'
        // `param_values` inconsistent — this ordering is the resolved D9
        // deliverable-3 VERIFY-AT-IMPL.
        self.local_project.upsert_embedded_preset(embedded.clone());
        crate::project_io::install_project_preset_overlay(&self.local_project);

        // Insert above the layer under the cursor, or at the top when dropped
        // outside the tracks area. Generators are full-canvas — unlike images
        // they need no specific host layer type.
        let insert_index = layer_under_cursor
            .unwrap_or(0)
            .min(self.local_project.timeline.layers.len());

        // GLB_CONFORMANCE_DESIGN.md D4: import is 1:1 — object_count always
        // equals material_count, nothing is ever dropped over a cap
        // (assemble_import_graph errors instead of truncating).
        match &conversion_report_line {
            Some(line) => log::warn!(
                "[Import] Added 3D model '{display_name}' — {} object(s), {} texture(s) ({line})",
                report.object_count,
                report.textures_wired,
            ),
            None => log::warn!(
                "[Import] Added 3D model '{display_name}' — {} object(s), {} texture(s)",
                report.object_count,
                report.textures_wired,
            ),
        }

        // 1. Layer command — execute locally first so the generated LayerId is
        //    fixed in the command instance, then send that SAME instance to the
        //    content thread. Both threads then insert the identical layer (id
        //    included), which lets the clip below target it deterministically.
        //    This is the sanctioned add-layer dispatch (see the ui_bridge
        //    ContextAddGeneratorLayer path).
        let mut layer_cmd =
            manifold_editing::commands::layer::ImportModelLayerCommand::new(
                display_name,
                embedded,
                insert_index,
                None,
            );
        layer_cmd.execute(&mut self.local_project);
        let Some(layer_id) = layer_cmd.inserted_layer_id() else {
            log::error!("[Import] layer command produced no layer id — aborting clip add");
            return;
        };
        let layer_boxed: Box<dyn Command + Send> = Box::new(layer_cmd);
        let _ = content_tx.send(ContentCommand::Execute(layer_boxed));

        // 2. A default generator clip on that layer, so the model renders at
        //    once. The clip is a bare time-span; the layer's graph drives the
        //    pixels (a generator clip carries no generator of its own). Sent
        //    after the layer on the same ordered channel, so the layer exists
        //    on the content thread before the clip targets it.
        let spb = 60.0 / self.local_project.settings.bpm.0;
        let clip = manifold_core::clip::TimelineClip::new_generator(
            manifold_core::Beats::from_f32(drop_beat.max(0.0)),
            manifold_core::Beats::from_f32(DEFAULT_MODEL_DURATION_BEATS),
        );
        let mut clip_cmd =
            manifold_editing::commands::clip::AddClipCommand::new(clip, layer_id, spb);
        clip_cmd.execute(&mut self.local_project);
        let clip_boxed: Box<dyn Command + Send> = Box::new(clip_cmd);
        let _ = content_tx.send(ContentCommand::Execute(clip_boxed));
    }

    /// Open. Delegates to ProjectIOService.open_project.
    pub(crate) fn open_project(&mut self) {
        self.send_content_cmd(ContentCommand::PauseRendering);
        let action = self.project_io.open_project(&mut self.user_prefs);
        self.send_content_cmd(ContentCommand::ResumeRendering);
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
            let t_total = std::time::Instant::now();

            // PrepareForProjectSwitch — audio-layer playback resets on the content
            // thread when it applies LoadProject (evicts the old project's voices).
            // Apply saved layout before initializing
            self.ws.ui_root.apply_project_layout(&project.settings);
            let saved_time = project.saved_playhead_time;

            // Update local_project BEFORE sending to content thread so UI
            // can rebuild the timeline in this same frame.
            self.local_project = project.clone();
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
            self.needs_rebuild = true;

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

        // Structural sync
        if action.needs_structural_sync {
            self.needs_structural_sync = true;
        }

        // Non-blocking load-repair notice (BUG-063,
        // `docs/PROJECT_FILE_INTEGRITY_DESIGN.md` §3.6). Never a blocking
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

        // Clip sync
        if action.needs_clip_sync {
            self.needs_rebuild = true;
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
                    needs_structural_sync: true,
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
                    needs_structural_sync: true,
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

        let id = window.id();

        // Query headroom for the new output window immediately — don't wait
        // for an NSNotification. Without this, output_edr_headroom stays at 1.0
        // (SDR) and the blit applies ACES on top of the compositor's EDR output,
        // causing washed-out, double-tonemapped results.
        let h = crate::edr_surface::query_window_headroom(&window);
        if (h - self.output_edr_headroom).abs() > 0.01 {
            self.output_edr_headroom = h;
        }

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
            self.send_content_cmd(crate::content_command::ContentCommand::UpdateEdrHeadroom(h));
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
            h,
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
                (
                    ((s.width as f64 / sf) * 0.75) as u32,
                    ((s.height as f64 / sf) * 0.75) as u32,
                    sf,
                )
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
            manifold_gpu::GpuTextureFormat::Bgra8Unorm,
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

        let offscreen = gpu.device.create_texture(&manifold_gpu::GpuTextureDesc {
            width: size.width.max(1),
            height: size.height.max(1),
            depth: 1,
            format: manifold_gpu::GpuTextureFormat::Bgra8Unorm,
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

        let mut ws = crate::workspace::Workspace::new(crate::workspace::WorkspaceKind::GraphEditor);
        ws.ui_offscreen = Some(offscreen);
        #[cfg(target_os = "macos")]
        {
            ws.ui_display_link = Some(crate::display_link::UiDisplayLink::new(window));
        }
        self.graph_editor = Some(ws);
        self.graph_editor_window_id = Some(wid);
        self.graph_canvas = Some(crate::graph_canvas::GraphCanvas::new());

        // Populate the editor's own inspector column immediately so it isn't
        // blank until the next selection change. Same snapshot the main window's
        // inspector reads; kept in lockstep thereafter by the tick's gated
        // re-sync. See docs/GRAPH_EDITOR_INSPECTOR_UNIFICATION.md.
        let active_idx = self
            .active_layer_id
            .as_ref()
            .and_then(|id| self.local_project.timeline.find_layer_index_by_id(id));
        if let Some(ed) = self.graph_editor.as_mut() {
            crate::ui_bridge::sync_project_data(
                &mut ed.ui_root,
                &self.local_project,
                active_idx,
                &self.selection,
            );
            crate::ui_bridge::sync_inspector_data(
                &mut ed.ui_root,
                &self.local_project,
                active_idx,
                &self.selection,
                &self.content_state.automation_latched_params,
            );
        }

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
        self.graph_editor = None;
        self.graph_canvas = None;
        // Stop per-node thumbnail capture on the content thread now the editor
        // is gone, so a live show pays nothing for it. An empty visible set
        // turns the atlas dump off.
        if !self.last_atlas_visible_sent.is_empty() {
            self.send_content_cmd(ContentCommand::SetNodeAtlasVisible(Vec::new()));
            self.last_atlas_visible_sent.clear();
        }
        // Clear the Phase 4 caches alongside — `watched_graph_target`
        // is the gate for the palette being active and the sole identity
        // for every editor-card edit, so a stale value would let the user
        // trigger commands against a dead effect or generator if they
        // reopened the window on a different one.
        self.watched_graph_target = None;
        self.watched_catalog_default = None;
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
            build_video_import_batch(&[fake_webm.clone()], 120.0, 0.0, &layer_id);

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
