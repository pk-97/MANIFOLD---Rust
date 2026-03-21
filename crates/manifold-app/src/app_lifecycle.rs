//! Project lifecycle methods for Application — extracted from app.rs.
//!
//! Contains save/open/new project methods, audio loading, and output window
//! management. All methods are `impl Application` blocks that operate on the
//! struct defined in app.rs.

use std::sync::Arc;

use winit::event_loop::ActiveEventLoop;

use manifold_editing::service::EditingService;
use manifold_playback::audio_decoder::DecodedAudio;
use manifold_renderer::blit::BlitPipeline;
use manifold_renderer::surface::SurfaceWrapper;

use crate::app::{Application, PendingAudioLoadResult};
use crate::content_command::ContentCommand;
use crate::project_io::ProjectIOAction;
use crate::window_registry::{WindowRole, WindowState};

impl Application {
    // ── Project I/O — delegates to ProjectIOService ────────────────────

    /// Save. Delegates to ProjectIOService.save_project.
    pub(crate) fn save_project(&mut self) {
        let current_time = self.content_state.current_time;
        let current_path = self.current_project_path.clone();
        // Save the local project snapshot (best effort — authoritative is on content thread)
        self.local_project.saved_playhead_time = current_time;
        if let Some(path) = current_path.as_deref() {
            match manifold_io::saver::save_project(&mut self.local_project, path, None, false) {
                Ok(()) => {
                    self.send_content_cmd(ContentCommand::SetProject);
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
        self.local_project.saved_playhead_time = current_time;
        let action = self.project_io.save_project_as(
            &mut self.local_project,
            current_time,
            &mut EditingService::new(), // placeholder — mark clean via content thread
            &mut self.user_prefs,
        );
        self.send_content_cmd(ContentCommand::ResumeRendering);
        self.apply_project_io_action(action);
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
        let action = self.project_io.open_project_from_path(&path, &mut self.user_prefs);
        self.apply_project_io_action(action);
    }

    /// Apply a ProjectIOAction returned by ProjectIOService.
    /// Handles all side-effects that require Application-owned state:
    /// engine init, GPU resize, audio loading, selection reset, etc.
    pub(crate) fn apply_project_io_action(&mut self, action: ProjectIOAction) {
        // Apply loaded project (replaces host.PrepareForProjectSwitch + ApplyProject + OnProjectOpened)
        if let Some(project) = action.apply_project {
            let t_total = std::time::Instant::now();

            // PrepareForProjectSwitch — clean up previous audio/waveform state
            // Unity: WorkspaceController.ProjectIO.cs PrepareForProjectSwitch()
            // Audio reset sent to content thread
            self.send_content_cmd(ContentCommand::ResetAudio);
            self.ui_root.waveform_lane.clear_audio();
            self.ui_root.layout.waveform_lane_visible = false;
            self.pending_audio_load = None;

            // Apply saved layout before initializing
            self.ui_root.apply_project_layout(&project.settings);
            let saved_time = project.saved_playhead_time;

            // Update local_project BEFORE sending to content thread so UI
            // can rebuild the timeline in this same frame.
            self.local_project = project.clone();
            // Suppress content thread snapshots until it processes the LoadProject
            // command (which will bump data_version above current).
            self.suppress_snapshot_until = self.content_state.data_version + 1;

            self.send_content_cmd(ContentCommand::LoadProject(Box::new(project)));

            // Restore playhead position
            if saved_time > 0.0 {
                self.send_content_cmd(ContentCommand::SeekTo(saved_time));
            }

            // Resize compositor + generators to project resolution
            {
                let w = self.local_project.settings.output_width.max(1) as u32;
                let h = self.local_project.settings.output_height.max(1) as u32;
                self.send_content_cmd(ContentCommand::ResizeContent(w, h));
                log::info!("[ProjectIO] GPU resize sent: {}x{}", w, h);
            }

            // Spawn background audio loading (audio decode on background thread,
            // result forwarded to content thread via AudioLoaded command)
            let mut audio_path_for_load: Option<(String, f32)> = None;
            if let Some(ref perc) = self.local_project.percussion_import
                && let Some(ref audio_path) = perc.audio_path
                    && !audio_path.is_empty() {
                        audio_path_for_load = Some((audio_path.clone(), perc.audio_start_beat));
                        self.ui_root.layout.waveform_lane_visible = true;
                    }

            if let Some((audio_path, start_beat)) = audio_path_for_load {
                let (tx, rx) = std::sync::mpsc::channel();
                self.pending_audio_load = Some(rx);

                std::thread::Builder::new()
                    .name("audio-load".into())
                    .spawn(move || {
                        let t_audio = std::time::Instant::now();
                        let preloaded = match manifold_playback::audio_sync::preload_audio(&audio_path, start_beat) {
                            Ok(p) => p,
                            Err(e) => {
                                log::warn!("[ProjectIO] Background audio load failed: {}", e);
                                return;
                            }
                        };
                        log::info!("[Audio] decode (background): {:.1}ms", t_audio.elapsed().as_secs_f64() * 1000.0);

                        // Extract waveform PCM from kira's already-decoded frames (no second decode).
                        // Unity does the same: decode once, then AudioClip.GetData() for waveform.
                        let waveform = Some(DecodedAudio::from_static_sound_data(&preloaded.sound_data));

                        let _ = tx.send(PendingAudioLoadResult { preloaded, waveform });
                    })
                    .expect("Failed to spawn audio load thread");
            }

            self.send_content_cmd(ContentCommand::SetProject);
            self.selection.clear_selection();
            self.active_layer_index = Some(0);
            self.needs_rebuild = true;

            // Content thread renders at project FPS; UI always runs at display rate.
            // Don't sync UI frame timer to project FPS — that couples UI to render cadence.

            log::info!("[ProjectIO] load sync: {:.1}ms (audio continues in background)", t_total.elapsed().as_secs_f64() * 1000.0);
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
                    waveform: None,
                });

                if let Some(decoded) = result.waveform {
                    self.ui_root.waveform_lane.set_audio_data(
                        &decoded.samples,
                        decoded.channels,
                        decoded.sample_rate,
                    );
                    self.ui_root.layout.waveform_lane_visible = true;
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

    /// Open a decorated HDR output window (default size = project resolution).
    /// Resizable — content always renders at project resolution with letterbox/pillarbox.
    /// Native title bar allows drag-to-monitor and macOS fullscreen (green button).
    /// Surface is Rgba16Float — wgpu v28 Metal backend auto-enables EDR.
    /// Unity: NativeMonitorWindowController.cs + MonitorWindowPlugin.mm.
    pub(crate) fn open_output_window(
        &mut self,
        event_loop: &ActiveEventLoop,
        name: &str,
        display_index: Option<usize>,
    ) {
        let gpu = match &self.gpu {
            Some(g) => g,
            None => return,
        };

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
                i, n, s.width, s.height, p.x, p.y, sf,
                (s.width as f64 / sf) as u32, (s.height as f64 / sf) as u32
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
            mon_name, logical_w, logical_h, logical_x, logical_y,
            mon_phys_size.width, mon_phys_size.height, scale_factor
        );

        // Default size = project resolution. Resizable + native fullscreen supported.
        // Content always renders at project resolution with letterbox/pillarbox to fit.
        let (proj_w, proj_h) = (
            self.local_project.settings.output_width.max(1) as f64,
            self.local_project.settings.output_height.max(1) as f64,
        );
        let center_x = logical_x + (logical_w - proj_w) * 0.5;
        let center_y = logical_y + (logical_h - proj_h) * 0.5;

        let attrs = winit::window::Window::default_attributes()
            .with_title(format!("MANIFOLD - {}", name))
            .with_position(winit::dpi::Position::Logical(
                winit::dpi::LogicalPosition::new(center_x, center_y),
            ))
            .with_inner_size(winit::dpi::LogicalSize::new(proj_w, proj_h));

        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                log::error!("[OutputWindow] Failed to create window: {e}");
                return;
            }
        };

        // Window is interactive — user can drag to any monitor and use native fullscreen.

        let size = window.inner_size();
        let scale = window.scale_factor();

        // Create HDR surface (Rgba16Float) — wgpu auto-enables EDR on Metal.
        // Linear HDR values pass through directly; macOS clips at display's physical max.
        let surface = SurfaceWrapper::new_hdr(
            &gpu.instance,
            &gpu.adapter,
            &gpu.device,
            window.clone(),
            size.width,
            size.height,
            scale,
            wgpu::PresentMode::AutoNoVsync,
        );

        let surface_format = surface.format();

        // Create a blit pipeline matching the output surface format.
        // The main workspace uses Bgra8UnormSrgb; the output window uses Rgba16Float.
        // Each needs its own pipeline for the target format.
        if self.output_blit_pipeline.is_none()
            || self.output_blit_format != Some(surface_format)
        {
            self.output_blit_pipeline = Some(BlitPipeline::new(&gpu.device, surface_format));
            self.output_blit_format = Some(surface_format);
            log::info!(
                "[OutputWindow] Created blit pipeline for {:?}",
                surface_format
            );
        }

        let id = window.id();
        let resolved_index = display_index.or({
            if monitors.len() > 1 { Some(1) } else { Some(0) }
        });

        let state = WindowState {
            window,
            surface,
            role: WindowRole::Output {
                name: name.to_string(),
            },
            display_index: resolved_index,
        };

        self.window_registry.add(id, state);
        log::info!(
            "[OutputWindow] Opened '{}' on '{}' ({}x{}, {:?})",
            name, mon_name, size.width, size.height, surface_format
        );
    }
}
