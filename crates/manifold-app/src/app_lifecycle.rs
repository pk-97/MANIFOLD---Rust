//! Project lifecycle methods for Application — extracted from app.rs.
//!
//! Contains save/open/new project methods, audio loading, and output window
//! management. All methods are `impl Application` blocks that operate on the
//! struct defined in app.rs.

use std::sync::Arc;

use winit::event_loop::ActiveEventLoop;

use manifold_editing::service::EditingService;
use manifold_playback::audio_decoder::DecodedAudio;

use crate::app::{Application, PendingAudioLoadResult};
use crate::content_command::ContentCommand;

use crate::project_io::ProjectIOAction;
use crate::window_registry::{WindowRole, WindowState};
use manifold_core::Seconds;

impl Application {
    // ── Project I/O — delegates to ProjectIOService ────────────────────

    /// Persist current viewport scroll + zoom and UI collapse states into project settings.
    fn save_viewport_state(&mut self) {
        self.local_project.settings.viewport_scroll_x_beats =
            self.ws.ui_root.viewport.scroll_x_beats().as_f32();
        self.local_project.settings.viewport_scroll_y_px = self.ws.ui_root.viewport.scroll_y_px();
        self.local_project.settings.viewport_pixels_per_beat =
            self.ws.ui_root.viewport.pixels_per_beat();

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
        if let Some(path) = current_path.as_deref() {
            match manifold_io::saver::save_project(&mut self.local_project, path, None, false) {
                Ok(()) => {
                    self.send_content_cmd(ContentCommand::MarkClean);
                    log::info!("[ProjectIO] Saved to {}", path.display());
                }
                Err(e) => log::error!("[ProjectIO] Save failed: {e}"),
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

        std::thread::spawn(move || {
            let spb = 60.0 / bpm;
            let mut beat = insert_beat;
            let mut video_clips: Vec<manifold_core::video::VideoClip> = Vec::new();
            let mut commands: Vec<ContentCommand> = Vec::new();

            for path in &paths {
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
                    manifold_editing::commands::clip::AddClipCommand::new(
                        clip,
                        layer_id.clone(),
                        spb,
                    ),
                )));

                log::warn!(
                    "[Import] Added '{file_name}' at beat {beat:.1} \
                     ({duration_secs:.1}s → {duration_beats:.1} beats, {res_w}x{res_h})"
                );

                beat += duration_beats;
            }

            if video_clips.is_empty() {
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
        });
    }

    /// Open. Delegates to ProjectIOService.open_project.
    pub(crate) fn open_project(&mut self) {
        self.send_content_cmd(ContentCommand::PauseRendering);
        let action = self
            .project_io
            .open_project(&mut self.user_prefs);
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

            // PrepareForProjectSwitch — clean up previous audio/waveform/stem state
            // Unity: WorkspaceController.ProjectIO.cs PrepareForProjectSwitch()
            // Audio + stem reset sent to content thread
            self.send_content_cmd(ContentCommand::ResetAudio);
            self.send_content_cmd(ContentCommand::StemReset);
            self.ws.ui_root.waveform_lane.clear_audio();
            self.ws.ui_root.stem_lanes.clear_all_stems();
            self.ws.ui_root.layout.waveform_lane_visible = false;
            self.ws.ui_root.layout.stem_lanes_expanded = false;
            self.pending_audio_load = None;
            self.loaded_audio_path = None;

            // Apply saved layout before initializing
            self.ws.ui_root.apply_project_layout(&project.settings);
            let saved_time = project.saved_playhead_time;

            // Update local_project BEFORE sending to content thread so UI
            // can rebuild the timeline in this same frame.
            self.local_project = project.clone();
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

            // Spawn background audio loading (audio decode on background thread,
            // result forwarded to content thread via AudioLoaded command)
            let mut audio_path_for_load: Option<(String, f32)> = None;
            if let Some(ref perc) = self.local_project.percussion_import
                && let Some(ref audio_path) = perc.audio_path
                && !audio_path.is_empty()
            {
                audio_path_for_load = Some((audio_path.clone(), perc.audio_start_beat.as_f32()));
                self.ws.ui_root.layout.waveform_lane_visible = true;
            }

            if let Some((audio_path, start_beat)) = audio_path_for_load {
                self.loaded_audio_path = Some(audio_path.clone());
                self.spawn_background_audio_load(audio_path, start_beat);
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
    }

    /// Poll for completed background audio load and apply results.
    /// Called each frame from tick_and_render.
    pub(crate) fn poll_pending_audio_load(&mut self) {
        let rx = match self.pending_audio_load.as_ref() {
            Some(rx) => rx,
            None => return,
        };

        match rx.try_recv() {
            Ok(result) => {
                self.pending_audio_load = None;

                // Send loaded audio to content thread
                self.send_content_cmd(ContentCommand::AudioLoaded {
                    preloaded: Box::new(result.preloaded),
                });

                if let Some(decoded) = result.waveform {
                    self.ws.ui_root.waveform_lane.set_audio_data(
                        &decoded.samples,
                        decoded.channels,
                        decoded.sample_rate,
                    );
                    self.ws.ui_root.layout.waveform_lane_visible = true;
                    // Rebuild viewport so tracks_rect accounts for waveform lane height.
                    self.needs_rebuild = true;
                    log::info!("[Waveform] Decoded audio for waveform display");
                }

                log::info!("[Audio] background load applied to UI thread");
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.pending_audio_load = None;
            }
        }
    }

    /// Spawn a background thread that decodes audio via kira (for playback) and
    /// extracts PCM samples (for waveform). Results are picked up by
    /// `poll_pending_audio_load()` each frame.
    pub(crate) fn spawn_background_audio_load(&mut self, audio_path: String, start_beat: f32) {
        let (tx, rx) = std::sync::mpsc::channel();
        self.pending_audio_load = Some(rx);

        std::thread::Builder::new()
            .name("audio-load".into())
            .spawn(move || {
                let t_audio = std::time::Instant::now();
                let preloaded = match manifold_playback::audio_sync::preload_audio(
                    &audio_path,
                    manifold_core::Beats::from_f32(start_beat),
                ) {
                    Ok(p) => p,
                    Err(e) => {
                        log::warn!("[Audio] Background audio load failed: {}", e);
                        return;
                    }
                };
                log::info!(
                    "[Audio] decode (background): {:.1}ms",
                    t_audio.elapsed().as_secs_f64() * 1000.0
                );

                let waveform = Some(DecodedAudio::from_static_sound_data(&preloaded.sound_data));

                let _ = tx.send(PendingAudioLoadResult {
                    preloaded,
                    waveform,
                });
            })
            .expect("Failed to spawn audio load thread");
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
            self.send_content_cmd(
                crate::content_command::ContentCommand::SetOutputSurface(surface),
            );
            self.send_content_cmd(
                crate::content_command::ContentCommand::UpdateEdrHeadroom(h),
            );
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

    /// Open the node-graph editor window. Sized at 75% of the primary
    /// window's logical inner size and positioned near it (winit picks
    /// the placement; the user can drag it onto a secondary monitor).
    ///
    /// No-op if the editor is already open. Phase 2 smoke test: the
    /// window renders a solid dark grey clear each frame; Phase 4
    /// will host an actual `UIRoot` here.
    pub(crate) fn open_graph_editor(&mut self, event_loop: &ActiveEventLoop) {
        if self.graph_editor.is_some() {
            log::debug!("[GraphEditor] Already open — ignoring open request");
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

        let attrs = winit::window::Window::default_attributes()
            .with_title("MANIFOLD — Graph Editor")
            .with_inner_size(winit::dpi::LogicalSize::new(logical_w, logical_h));

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
        if let Some(wid) = self.graph_editor_window_id.take() {
            self.window_registry.remove(&wid);
        }
        self.graph_editor = None;
        log::info!("[GraphEditor] Closed");
    }

}
