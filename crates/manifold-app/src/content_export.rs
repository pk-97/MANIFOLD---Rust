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
            self.send_export_finished(
                state_tx,
                false,
                "No project loaded".into(),
                &config.output_path,
            );
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
            self.send_export_finished(
                state_tx,
                false,
                "No content in export range".into(),
                &config.output_path,
            );
            return;
        }

        // Build final config with resolved range + audio info from content thread
        let mut export_config = config;
        export_config.start_beat = start_beat;
        export_config.end_beat = end_beat;

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
            self.send_export_finished(
                state_tx,
                false,
                "Zero frames to export".into(),
                &export_config.output_path,
            );
            return;
        }

        // Render the audio-layer mix for the export range into a temp WAV, then
        // wire it as the export's audio track. Mirrors live playback exactly
        // (warp / gain / solo); see manifold_playback::audio_mixdown. Aligned to
        // the export start, so audio_start_beat = start_beat → mux offset 0.
        //
        // P2 (docs/OFFLINE_AUDIO_REACTIVE_EXPORT_DESIGN.md): the same render
        // also produces the mono buffers the offline audio-mod driver
        // analyzes — "one render, two consumers, no drift between what is
        // heard and what is analyzed" (design seam brief). `tapped_layers` is
        // every layer any consumed send reads (union over
        // `AudioSend::layers()`), so the mixdown renders exactly the taps the
        // driver will need and no more.
        let consumed_sends = project.analysis_consumed_sends();
        let mut tapped_layers_set: ahash::AHashSet<manifold_core::id::LayerId> =
            ahash::AHashSet::new();
        for send in &project.audio_setup.sends {
            if consumed_sends.contains(&send.id) {
                tapped_layers_set.extend(send.layers().iter().cloned());
            }
        }
        let tapped_layers: Vec<manifold_core::id::LayerId> =
            tapped_layers_set.into_iter().collect();

        let mix_wav_path = format!("{}.mixdown.wav", export_config.output_path);
        // Declared before `offline_audio_mod` so it outlives the driver that
        // borrows its buffers (Rust drops locals in reverse declaration order).
        let export_audio = match manifold_playback::audio_mixdown::render_export_audio(
            project,
            Beats::from_f32(start_beat),
            Beats::from_f32(end_beat),
            bpm,
            &mut tempo_map,
            &tapped_layers,
        ) {
            Ok(audio) => {
                // Byte-identical WAV semantics to the old `render_export_mix`
                // wrapper (P1-guaranteed): same Ok(true)/Ok(false)/Err handling.
                match manifold_playback::audio_mixdown::write_export_wav(&audio, &mix_wav_path) {
                    Ok(true) => {
                        export_config.audio_path = Some(mix_wav_path.clone());
                        export_config.audio_start_beat = start_beat;
                        export_config.audio_encoder_delay = 0.0;
                    }
                    Ok(false) => {
                        log::info!("[Export] No audio-layer clips in range — video-only export");
                    }
                    Err(e) => {
                        log::warn!(
                            "[Export] Audio mixdown WAV write failed ({e}) — exporting video-only"
                        );
                    }
                }
                Some(audio)
            }
            Err(e) => {
                log::warn!("[Export] Audio mixdown failed ({e}) — exporting video-only");
                None
            }
        };
        // The offline audio-mod driver analyzes the rendered buffer directly —
        // independent of whether the WAV muxing above succeeded, since that's
        // a disk-write concern and this is the in-memory render (P2).
        let mut offline_audio_mod = export_audio
            .as_ref()
            .and_then(|audio| {
                crate::offline_audio_mod::OfflineAudioModDriver::new(
                    project,
                    audio,
                    export_config.fps as f64,
                )
            });

        // Detect generator-only projects: no video clips means no decode
        // backpressure needed, enabling faster-than-realtime export.
        // Matches Unity's IsGeneratorOnlyProject() → Time.captureFramerate path.
        let generator_only = project.timeline.layers.iter().all(|layer| {
            layer.is_group() || layer.clips.iter().all(|c| c.video_clip_id.is_empty())
        });
        let mode_label = if generator_only {
            "offline"
        } else {
            "real-time"
        };

        log::info!(
            "[Export] {} mode: {} frames, {:.2}s, beats {:.1}-{:.1}, \
             {}x{} @ {} fps, audio={}",
            mode_label,
            total_frames,
            duration,
            start_beat,
            end_beat,
            export_config.width,
            export_config.height,
            export_config.fps,
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
        let start_time = self
            .engine
            .beat_to_timeline_time(Beats::from_f32(start_beat));
        self.engine.seek_to(start_time);
        self.engine.play();

        // 4. Create export session (initializes native Metal encoder).
        //    Share the content pipeline's Metal device to avoid cross-device GPU sync.
        let device_ptr = self.content_pipeline.native_device_ptr();
        let session_result = if let Some(ptr) = device_ptr {
            unsafe {
                manifold_media::export_session::ExportSession::new_with_device(
                    export_config.clone(),
                    bpm.0,
                    &mut tempo_map,
                    ptr,
                )
            }
        } else {
            manifold_media::export_session::ExportSession::new(
                export_config.clone(),
                bpm.0,
                &mut tempo_map,
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
                self.send_export_finished(
                    state_tx,
                    false,
                    format!("Export failed: {e}"),
                    &export_config.output_path,
                );
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
            let start_time = self
                .engine
                .beat_to_timeline_time(Beats::from_f32(start_beat));
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
            let frame_err: Option<String> = objc2::rc::autoreleasepool(|_| {
                self.export_one_frame(
                    &mut session,
                    &export_config,
                    frame_idx,
                    total_frames,
                    frame_dt,
                    state_tx,
                    generator_only,
                    offline_audio_mod.as_mut(),
                )
            });
            #[cfg(not(target_os = "macos"))]
            let frame_err: Option<String> = self.export_one_frame(
                &mut session,
                &export_config,
                frame_idx,
                total_frames,
                frame_dt,
                state_tx,
                generator_only,
                offline_audio_mod.as_mut(),
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
                log::info!(
                    "[ContentThread] Export cancelled at frame {}",
                    session.frames_encoded()
                );
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
                        result.frames_encoded,
                        result.duration_seconds,
                        result.output_path,
                    );
                    self.send_export_finished(
                        state_tx,
                        true,
                        format!("Export complete: {} frames", result.frames_encoded),
                        &result.output_path,
                    );
                }
                Err(e) => {
                    log::error!("[ContentThread] Export finalization failed: {e}");
                    self.send_export_finished(
                        state_tx,
                        false,
                        format!("Export failed: {e}"),
                        &export_config.output_path,
                    );
                }
            }
        }

        // Remove the temporary audio mixdown WAV (already muxed into the final
        // file; a no-op when no audio was rendered).
        let _ = std::fs::remove_file(&mix_wav_path);

        // 7. Restore playback state
        self.engine.set_export_mode(false);
        // Restore content pipeline resolution (and render scale) after export.
        if cur_w != export_config.width || cur_h != export_config.height {
            let render_scale = self
                .engine
                .project()
                .map_or(1.0, |p| p.settings.render_scale);
            self.content_pipeline
                .resize(&mut self.engine, cur_w, cur_h, render_scale);
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
            self.send_export_finished(state_tx, false, msg, &export_config.output_path);
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
        offline_audio_mod: Option<&mut crate::offline_audio_mod::OfflineAudioModDriver>,
    ) -> Option<String> {
        let ctx = TickContext {
            dt_seconds: Seconds(frame_dt),
            realtime_now: Seconds(frame_idx as f64 * frame_dt),
            pre_render_dt: Seconds(frame_dt),
            frame_count: frame_idx as u64,
            export_fixed_dt: Seconds(frame_dt),
        };
        // P2 (docs/OFFLINE_AUDIO_REACTIVE_EXPORT_DESIGN.md): feed this frame's
        // export-rendered audio through the analyzer chain and write the
        // resulting features into the engine's audio snapshot BEFORE the
        // tick that consumes them for param modulation, param triggers, and
        // live clip triggers — deterministic audio reactivity in the export.
        // No restore after export: `AudioModRuntime::update` overwrites
        // `snap.sends` unconditionally on every live tick (including its
        // `active == false` branch, which still clears+resizes), so
        // export-written features cannot leak into subsequent live playback.
        if let Some(driver) = offline_audio_mod {
            driver.feed_frame(frame_idx, &mut self.engine);
        }
        let tick_result = self.engine.tick(ctx);

        // Wait for any in-flight video decodes to complete before rendering.
        // At GPU speed the export outruns the async decoder — without this,
        // the same stale video frame gets encoded for dozens of frames.
        // Skipped for generator-only projects (no video decoders).
        if !generator_only {
            self.engine.flush_pending_decodes();
        }

        self.content_pipeline.render_content(
            &self.gpu,
            &mut self.engine,
            &tick_result,
            frame_dt,
            frame_idx as u64,
            true,
            self.editing_service.data_version(),
        );

        // Block until async effect workers complete (blob tracking, wireframe depth,
        // depth-of-field). During live playback 1-2 frame latency is acceptable, but
        // export must be frame-perfect: each frame's async results must resolve before
        // the frame is encoded.
        self.content_pipeline.flush_all_background_work();

        let tex_ptr = if export_config.hdr {
            let paper_white = 200.0f32;
            let max_nits = 10000.0f32;
            let texture = self
                .content_pipeline
                .pq_encode_for_export(paper_white, max_nits);
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
            self.content_pipeline
                .cleanup_stopped_clips(&tick_result.stopped_clips);
        }
        self.engine.reclaim_tick_result(tick_result);

        if frame_idx.is_multiple_of(10) {
            self.send_export_progress(state_tx);
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
    fn send_export_progress(&self, state_tx: &Sender<ContentState>) {
        // Transport keep-alive while the export loop owns the content thread.
        // The progress fields this used to carry (is_exporting / export_progress /
        // export_status, from ExportSession) were never read by any UI code —
        // deleted 2026-07-09 in the ContentState orphan purge
        // (UI_PROJECTION_LAYER_DESIGN.md P0). An export progress display is
        // BUG-083; restore the fields WITH their consumer from this commit's parent.
        let state = ContentState {
            current_beat: self.engine.current_beat(),
            current_time: self.engine.current_time(),
            is_playing: self.engine.is_playing(),
            ..ContentState::default()
        };
        if let Err(e) = state_tx.send(state) {
            log::error!("[ContentThread] Export progress channel disconnected: {e}");
        }
    }

    /// Submit a pending still-frame export's GPU readback, if one is waiting and
    /// hasn't been submitted yet. Called right after `render_content` so the blit
    /// reads a fully-rendered frame. Records the captured dimensions on the job.
    #[cfg(target_os = "macos")]
    pub(crate) fn submit_still_export_if_pending(&mut self) {
        if let Some(job) = self.still_export.as_mut()
            && job.dims.is_none()
        {
            let dims = self.content_pipeline.submit_still_readback();
            // Re-borrow: submit_still_readback took &mut self.content_pipeline.
            if let Some(job) = self.still_export.as_mut() {
                job.dims = Some(dims);
            }
        }
    }

    /// Read back a submitted still-frame export, then convert colour, encode,
    /// and write to disk on a detached thread (decoding linear f16, sRGB
    /// encoding, and PNG-ing a 4000×4000 frame is far too heavy for the content
    /// thread). The finished event is sent from that thread. No-op until the
    /// readback has been submitted (`dims` set) and the GPU copy is readable.
    #[cfg(target_os = "macos")]
    pub(crate) fn poll_still_export(&mut self, state_tx: &Sender<ContentState>) {
        // Only act once the readback has been submitted (dims set on the prior tick).
        if self.still_export.as_ref().is_none_or(|j| j.dims.is_none()) {
            return;
        }
        let Some(packed_f16) = self.content_pipeline.take_still_readback() else {
            return;
        };
        let job = self.still_export.take().expect("checked above");
        let (w, h) = job.dims.expect("checked above");
        let path = job.path;
        let format = job.format;
        let tx = state_tx.clone();

        std::thread::Builder::new()
            .name("still-export-encode".into())
            .spawn(move || {
                // Linear Rgba16Float → sRGB-encoded RGBA8 (faithful: no highlight
                // rolloff, matching the on-screen image). Then encode to disk.
                let encode = manifold_media::still_exporter::linear_f16_rgba_to_srgb8(
                    &packed_f16,
                    w,
                    h,
                    /* rolloff */ false,
                )
                .and_then(|rgba8| {
                    manifold_media::still_exporter::save_still(
                        &rgba8,
                        w,
                        h,
                        std::path::Path::new(&path),
                        format,
                    )
                });
                let (success, message) = match encode {
                    Ok(()) => {
                        log::info!("[ContentThread] Exported frame to {path}");
                        (true, format!("Exported frame to {path}"))
                    }
                    Err(e) => {
                        log::error!("[ContentThread] Frame export failed: {e}");
                        (false, e)
                    }
                };
                let state = ContentState {
                    export_finished: Some(ExportFinishedEvent {
                        success,
                        message,
                        output_path: path,
                    }),
                    ..ContentState::default()
                };
                let _ = tx.send(state);
            })
            .expect("failed to spawn still-export thread");
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
        if let Err(e) = state_tx.send(state) {
            log::error!("[ContentThread] Export finished channel disconnected: {e}");
        }
    }
}
