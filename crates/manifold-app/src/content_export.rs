//! Export functionality for ContentThread — extracted from content_thread.rs.
//! Contains `run_export`, `export_one_frame`, `get_metal_texture_ptr`,
//! `send_export_progress`, and `send_export_finished`.

use crossbeam_channel::{Receiver, Sender};

use manifold_core::{Beats, Seconds};
use manifold_playback::engine::TickContext;

use crate::content_command::ContentCommand;
use crate::content_state::{ContentState, ExportFinishedEvent};
use crate::content_thread::ContentThread;

impl ContentThread {
    /// Run the offline video export loop.
    ///
    /// Temporarily replaces the normal content loop: ticks the engine with fixed
    /// delta, renders each frame, and encodes via the native Metal encoder at
    /// maximum GPU speed (no frame pacing / sleep).
    ///
    /// Port of Unity VideoExporter.ExportCoroutine() (offline / generator-only path).
    #[cfg(target_os = "macos")]
    pub(crate) fn run_export(
        &mut self,
        config: manifold_media::export_config::ExportConfig,
        cmd_rx: &Receiver<ContentCommand>,
        state_tx: &Sender<ContentState>,
    ) {
        use manifold_core::tempo::TempoMapConverter;
        use manifold_media::audio_muxer::AudioMuxer;

        log::info!("[ContentThread] Starting export: {:?}", config);

        // 1. Save playback state for restore
        let was_playing = self.engine.is_playing();
        let saved_beat = self.engine.current_beat();

        // 2. Resolve export range
        let Some(project) = self.engine.project() else {
            log::error!("[ContentThread] No project loaded, cannot export");
            self.send_export_finished(state_tx, false, "No project loaded".into(), &config.output_path);
            return;
        };
        let bpm = project.settings.bpm;
        let (content_start, content_end) = project.timeline.content_range_beats();
        let content_start = content_start.as_f32();
        let content_end = content_end.as_f32();

        // Use config beats if set, otherwise use content range
        let start_beat = if config.start_beat > 0.0 {
            config.start_beat
        } else {
            content_start
        };
        let end_beat = if config.end_beat > 0.0 {
            config.end_beat
        } else {
            content_end
        };

        if start_beat >= end_beat || content_start >= content_end {
            log::error!("[ContentThread] No content in export range ({start_beat}..{end_beat})");
            self.send_export_finished(state_tx, false, "No content in export range".into(), &config.output_path);
            return;
        }

        // Build final config with resolved range + audio info from content thread
        let mut export_config = config;
        export_config.start_beat = start_beat;
        export_config.end_beat = end_beat;

        // Wire audio from the content thread's audio sync controller
        if let Some(ref audio_sync) = self.audio_sync
            && audio_sync.is_ready()
            && let Some(path) = audio_sync.audio_path()
        {
            export_config.audio_path = Some(path.to_string());
            export_config.audio_start_beat = audio_sync.start_beat().as_f32();
            export_config.audio_encoder_delay = audio_sync.encoder_delay_seconds().as_f32();
        }

        // Calculate timing
        let mut tempo_map = project.tempo_map.clone();
        let start_seconds =
            TempoMapConverter::beat_to_seconds(&mut tempo_map, Beats::from_f32(start_beat), bpm);
        let end_seconds =
            TempoMapConverter::beat_to_seconds(&mut tempo_map, Beats::from_f32(end_beat), bpm);
        let duration = end_seconds - start_seconds;
        let total_frames = (duration * export_config.fps).0.round() as u32;
        let frame_dt = 1.0 / export_config.fps as f64;

        if total_frames == 0 {
            log::error!("[ContentThread] Zero frames to export");
            self.send_export_finished(state_tx, false, "Zero frames to export".into(), &export_config.output_path);
            return;
        }

        // Detect generator-only projects: no video clips means no decode
        // backpressure needed, enabling faster-than-realtime export.
        // Matches Unity's IsGeneratorOnlyProject() → Time.captureFramerate path.
        let generator_only = project.timeline.layers.iter().all(|layer| {
            layer.is_group() || layer.clips.iter().all(|c| c.video_clip_id.is_empty())
        });
        let mode_label = if generator_only { "offline" } else { "real-time" };

        log::info!(
            "[Export] {} mode: {} frames, {:.2}s, beats {:.1}-{:.1}, \
             {}x{} @ {} fps, audio={}",
            mode_label, total_frames, duration, start_beat, end_beat,
            export_config.width, export_config.height, export_config.fps,
            export_config.has_audio(),
        );

        // 3. Enter export mode
        self.engine.stop();
        self.engine.set_export_mode(true);
        // Ensure content pipeline matches export resolution.
        // Export always renders at full resolution (render_scale = 1.0) for quality.
        let (cur_w, cur_h) = self.content_pipeline.dimensions();
        if cur_w != export_config.width || cur_h != export_config.height {
            self.content_pipeline.resize(
                &mut self.engine,
                export_config.width,
                export_config.height,
                1.0,
            );
        }
        // Seek to start
        let start_time = self.engine.beat_to_timeline_time(Beats::from_f32(start_beat));
        self.engine.seek_to(start_time);
        self.engine.play();

        // 4. Create export session (initializes native Metal encoder).
        //    Share the content pipeline's Metal device to avoid cross-device GPU sync.
        let device_ptr = self.content_pipeline.native_device_ptr();
        let session_result = if let Some(ptr) = device_ptr {
            unsafe {
                manifold_media::export_session::ExportSession::new_with_device(
                    export_config.clone(), bpm.0, &mut tempo_map, ptr,
                )
            }
        } else {
            manifold_media::export_session::ExportSession::new(
                export_config.clone(), bpm.0, &mut tempo_map,
            )
        };
        let mut session = match session_result {
            Ok(s) => s,
            Err(e) => {
                log::error!("[ContentThread] Failed to create export session: {e}");
                self.engine.set_export_mode(false);
                self.engine.stop();
                let restore_time = self.engine.beat_to_timeline_time(saved_beat);
                self.engine.seek_to(restore_time);
                self.send_export_finished(state_tx, false, format!("Export failed: {e}"), &export_config.output_path);
                return;
            }
        };

        // 4b. Wait for video decoders to produce their first frame.
        // Only ticks the engine (which drives pre_render → decode result drain).
        // No GPU rendering — we re-seek afterward, clearing all temporal state.
        // Skipped for generator-only projects (no video decoders to wait for).
        if !generator_only {
            const MAX_WARMUP_TICKS: u32 = 120;
            for warmup_i in 0..MAX_WARMUP_TICKS {
                let warmup_ctx = TickContext {
                    dt_seconds: Seconds(frame_dt),
                    realtime_now: Seconds::ZERO,
                    pre_render_dt: Seconds(frame_dt),
                    frame_count: u64::MAX,
                    export_fixed_dt: Seconds(frame_dt),
                };
                let warmup_result = self.engine.tick(warmup_ctx);
                self.engine.reclaim_tick_result(warmup_result);

                if self.engine.all_active_clips_ready() {
                    break;
                }
                if warmup_i % 30 == 29 {
                    log::warn!(
                        "[Export] Still waiting for decoders after {} warmup ticks",
                        warmup_i + 1,
                    );
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            // Re-seek to start — warmup ticks advanced the engine
            let start_time = self.engine.beat_to_timeline_time(Beats::from_f32(start_beat));
            self.engine.seek_to(start_time);
        }

        // 5. Export frame loop.
        //    Each iteration is wrapped in an autoreleasepool to drain Metal's
        //    autoreleased ObjC objects per-frame.
        let mut cancelled = false;
        let mut encode_error: Option<String> = None;
        for frame_idx in 0..total_frames {
            // Check for cancel command (non-blocking drain)
            while let Ok(cmd) = cmd_rx.try_recv() {
                if matches!(cmd, ContentCommand::CancelExport) {
                    cancelled = true;
                    break;
                }
            }
            if cancelled {
                session.cancel();
                break;
            }

            #[cfg(target_os = "macos")]
            let frame_err: Option<String> = objc::rc::autoreleasepool(|| {
                self.export_one_frame(
                    &mut session, &export_config, frame_idx, total_frames,
                    frame_dt, state_tx, generator_only,
                )
            });
            #[cfg(not(target_os = "macos"))]
            let frame_err: Option<String> = self.export_one_frame(
                &mut session, &export_config, frame_idx, total_frames,
                frame_dt, state_tx, generator_only,
            );

            if let Some(err) = frame_err {
                encode_error = Some(err);
                break;
            }
        }

        // 6. Finalize
        let failed = cancelled || encode_error.is_some();
        if failed {
            if cancelled {
                log::info!("[ContentThread] Export cancelled at frame {}", session.frames_encoded());
            }
            // Clean up partial file
            let _ = std::fs::remove_file(&export_config.output_path);
            let temp_video = format!("{}.video_only.mp4", export_config.output_path);
            let _ = std::fs::remove_file(&temp_video);
        } else {
            // Resolve FFmpeg for audio muxing
            let ffmpeg_path = AudioMuxer::resolve_ffmpeg("");
            match session.finalize(ffmpeg_path.as_deref()) {
                Ok(result) => {
                    log::info!(
                        "[ContentThread] Export complete: {} frames, {:.2}s -> {}",
                        result.frames_encoded, result.duration_seconds, result.output_path,
                    );
                    self.send_export_finished(
                        state_tx, true,
                        format!("Export complete: {} frames", result.frames_encoded),
                        &result.output_path,
                    );
                }
                Err(e) => {
                    log::error!("[ContentThread] Export finalization failed: {e}");
                    self.send_export_finished(
                        state_tx, false,
                        format!("Export failed: {e}"),
                        &export_config.output_path,
                    );
                }
            }
        }

        // 7. Restore playback state
        self.engine.set_export_mode(false);
        // Restore content pipeline resolution (and render scale) after export.
        if cur_w != export_config.width || cur_h != export_config.height {
            let render_scale = self.engine.project()
                .map_or(1.0, |p| p.settings.render_scale);
            self.content_pipeline.resize(&mut self.engine, cur_w, cur_h, render_scale);
        }
        self.engine.stop();
        let restore_time = self.engine.beat_to_timeline_time(saved_beat);
        self.engine.seek_to(restore_time);
        if was_playing {
            self.engine.play();
        }

        if failed {
            let msg = if let Some(err) = encode_error {
                format!("Export failed: {err}")
            } else {
                "Export cancelled".into()
            };
            self.send_export_finished(
                state_tx, false, msg, &export_config.output_path,
            );
        }
    }

    /// Render and encode a single export frame. Returns Some(error) on failure.
    fn export_one_frame(
        &mut self,
        session: &mut manifold_media::export_session::ExportSession,
        export_config: &manifold_media::export_config::ExportConfig,
        frame_idx: u32,
        _total_frames: u32,
        frame_dt: f64,
        state_tx: &crossbeam_channel::Sender<ContentState>,
        generator_only: bool,
    ) -> Option<String> {
        let ctx = TickContext {
            dt_seconds: Seconds(frame_dt),
            realtime_now: Seconds(frame_idx as f64 * frame_dt),
            pre_render_dt: Seconds(frame_dt),
            frame_count: frame_idx as u64,
            export_fixed_dt: Seconds(frame_dt),
        };
        let tick_result = self.engine.tick(ctx);

        // Wait for any in-flight video decodes to complete before rendering.
        // At GPU speed the export outruns the async decoder — without this,
        // the same stale video frame gets encoded for dozens of frames.
        // Skipped for generator-only projects (no video decoders).
        if !generator_only {
            self.engine.flush_pending_decodes();
        }

        self.content_pipeline.render_content(
            &self.gpu, &mut self.engine, &tick_result, frame_dt, frame_idx as u64,
            true,
        );

        // Block until async effect workers complete (blob tracking, wireframe depth,
        // depth-of-field). During live playback 1-2 frame latency is acceptable, but
        // export must be frame-perfect: each frame's async results must resolve before
        // the frame is encoded.
        self.content_pipeline.flush_all_background_work();

        let tex_ptr = if export_config.hdr {
            let paper_white = 200.0f32;
            let max_nits = 10000.0f32;
            let texture = self.content_pipeline.pq_encode_for_export(
                paper_white, max_nits,
            );
            Self::get_metal_texture_ptr(texture)
        } else {
            let texture = self.content_pipeline.export_output_texture();
            Self::get_metal_texture_ptr(texture)
        };

        self.content_pipeline.wait_for_render_complete();

        match tex_ptr {
            Some(ptr) => {
                if let Err(e) = unsafe { session.encode_frame(ptr) } {
                    log::error!("[ContentThread] Encode failed at frame {frame_idx}: {e}");
                    return Some(format!("Encode failed at frame {frame_idx}: {e}"));
                }
            }
            None => {
                log::error!("[ContentThread] No Metal texture at frame {frame_idx}");
                return Some(format!("No texture at frame {frame_idx}"));
            }
        }

        if !tick_result.stopped_clips.is_empty() {
            self.content_pipeline.cleanup_stopped_clips(&tick_result.stopped_clips);
        }
        self.engine.reclaim_tick_result(tick_result);

        if frame_idx.is_multiple_of(10) {
            self.send_export_progress(state_tx, session);
        }

        None
    }

    /// Extract the raw Metal texture pointer from a native GpuTexture.
    /// Returns `id<MTLTexture>` as `*mut c_void` for the native encoder.
    #[cfg(target_os = "macos")]
    fn get_metal_texture_ptr(texture: &manifold_gpu::GpuTexture) -> Option<*mut std::ffi::c_void> {
        Some(texture.raw_ptr())
    }

    /// Send export progress to the UI thread.
    #[cfg(target_os = "macos")]
    fn send_export_progress(
        &self,
        state_tx: &Sender<ContentState>,
        session: &manifold_media::export_session::ExportSession,
    ) {
        let state = ContentState {
            is_exporting: true,
            export_progress: session.progress(),
            export_status: session.status_text(),
            current_beat: self.engine.current_beat(),
            current_time: self.engine.current_time(),
            is_playing: self.engine.is_playing(),
            ..ContentState::default()
        };
        let _ = state_tx.try_send(state);
    }

    /// Send export finished event to the UI thread.
    pub(crate) fn send_export_finished(
        &self,
        state_tx: &Sender<ContentState>,
        success: bool,
        message: String,
        output_path: &str,
    ) {
        let state = ContentState {
            export_finished: Some(ExportFinishedEvent {
                success,
                message,
                output_path: output_path.to_string(),
            }),
            ..ContentState::default()
        };
        let _ = state_tx.try_send(state);
    }
}
