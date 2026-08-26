//! Export functionality for ContentThread — extracted from content_thread.rs.
//! Contains `run_export`, `export_one_frame`, `get_metal_texture_ptr`,
//! `send_export_progress`, and `send_export_finished`.

use std::sync::Arc;

use crossbeam_channel::{Receiver, Sender};

use manifold_core::{Beats, Seconds};
use manifold_playback::engine::TickContext;

use crate::content_command::ContentCommand;
use crate::content_state::{ContentState, ExportFinishedEvent};
use crate::content_thread::ContentThread;

/// Derive export sections from section-flagged markers. Sections are
/// `[range_start, m₁)`, `[m₁, m₂)`, …, `[mₙ, range_end)` where each m is a
/// sorted, deduplicated section-boundary marker strictly inside the range.
/// Each section is named by the marker at its start; the first section
/// (starting at `range_start`, which has no marker) carries an empty name,
/// which the filename logic turns into `section-N`.
///
/// A marker exactly on `range_start` or `range_end` is excluded (it would
/// produce an empty leading section, or sit outside the half-open range).
/// Returns empty when there are no in-range section markers → the caller
/// takes the single-export path. See docs/SECTION_EXPORT_DESIGN.md D2.
#[cfg(target_os = "macos")]
fn derive_sections(
    timeline: &manifold_core::timeline::Timeline,
    range_start: Beats,
    range_end: Beats,
) -> Vec<(Beats, Beats, String)> {
    let mut cuts: Vec<(Beats, String)> = timeline
        .markers
        .iter()
        .filter(|m| m.is_section_boundary)
        .filter(|m| m.beat > range_start && m.beat < range_end)
        .map(|m| (m.beat, m.name.clone()))
        .collect();
    cuts.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    // Duplicate beats collapse to one cut (a second cut at the same beat
    // would produce an empty section).
    cuts.dedup_by(|a, b| a.0 == b.0);

    if cuts.is_empty() {
        return Vec::new();
    }

    let mut boundaries: Vec<(Beats, String)> = Vec::with_capacity(cuts.len() + 1);
    boundaries.push((range_start, String::new()));
    boundaries.extend(cuts);

    let mut sections = Vec::with_capacity(boundaries.len());
    for window in boundaries.windows(2) {
        sections.push((window[0].0, window[1].0, window[0].1.clone()));
    }
    let last = boundaries.last().expect("boundaries is non-empty");
    sections.push((last.0, range_end, last.1.clone()));
    sections
}

/// Sanitize a marker name into a filename-safe stem: every run of
/// non-alphanumeric characters (whitespace, punctuation) collapses to a
/// single `-`, with leading/trailing dashes trimmed. See D6.
#[cfg(target_os = "macos")]
fn sanitize_section_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut last_was_dash = true;
    for ch in name.chars() {
        if ch.is_alphanumeric() {
            out.push(ch);
            last_was_dash = false;
        } else if !last_was_dash {
            out.push('-');
            last_was_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out
}

/// Per-section output paths from a base `output_path`. Each section's name
/// is sanitized (empty → `section-N` counting from 1); duplicate stems get
/// `-2`, `-3`, … suffixes. The base file's directory and extension are
/// preserved: `<base>--<stem>.<ext>`. See D6.
#[cfg(target_os = "macos")]
fn section_output_paths(base_output: &str, sections: &[(Beats, Beats, String)]) -> Vec<String> {
    let path = std::path::Path::new(base_output);
    let dir = path
        .parent()
        .map(|d| format!("{}/", d.display()))
        .unwrap_or_default();
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| base_output.to_string());
    let ext = path
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();

    let mut used: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    sections
        .iter()
        .enumerate()
        .map(|(i, (_, _, name))| {
            let mut key = sanitize_section_name(name);
            if key.is_empty() {
                key = format!("section-{}", i + 1);
            }
            let count = used.entry(key.clone()).or_insert(0);
            *count += 1;
            let suffixed = if *count == 1 {
                key
            } else {
                format!("{key}-{}", *count)
            };
            format!("{dir}{stem}--{suffixed}{ext}")
        })
        .collect()
}

impl ContentThread {
    /// Run the offline video export loop.
    ///
    /// Temporarily replaces the normal content loop: ticks the engine with fixed
    /// delta, renders each frame, and encodes via the native Metal encoder at
    /// maximum GPU speed (no frame pacing / sleep).
    ///
    /// With `split_at_section_markers`, this runs one full export per derived
    /// section, sequentially (docs/SECTION_EXPORT_DESIGN.md D1). A cancelled or
    /// failed section aborts the remaining sections.
    ///
    /// Port of Unity VideoExporter.ExportCoroutine() (offline / generator-only path).
    #[cfg(target_os = "macos")]
    pub(crate) fn run_export(
        &mut self,
        config: manifold_media::export_config::ExportConfig,
        cmd_rx: &Receiver<ContentCommand>,
        state_tx: &Sender<ContentState>,
    ) {
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

        // Build base config with resolved range + audio info from content thread
        let mut base_config = config;
        base_config.start_beat = start_beat;
        base_config.end_beat = end_beat;

        // Derive sections from timeline markers. Empty when the flag is off or
        // no section markers fall inside the range → single-export path below.
        let sections: Vec<(Beats, Beats, String)> = if base_config.split_at_section_markers {
            derive_sections(
                &project.timeline,
                Beats::from_f32(start_beat),
                Beats::from_f32(end_beat),
            )
        } else {
            Vec::new()
        };

        // Enter export mode + resize once (resolution is constant across sections).
        self.engine.stop();
        self.engine.set_export_mode(true);
        let (cur_w, cur_h) = self.content_pipeline.dimensions();
        if cur_w != base_config.width || cur_h != base_config.height {
            self.content_pipeline.resize(
                &mut self.engine,
                base_config.width,
                base_config.height,
                1.0,
            );
        }

        let section_count = sections.len();
        if section_count == 0 {
            // Single export — today's behavior, unmodified output path.
            self.run_export_section(base_config.clone(), bpm, None, cmd_rx, state_tx);
        } else {
            let paths = section_output_paths(&base_config.output_path, &sections);
            for (i, ((start, end, _name), path)) in sections.iter().zip(paths.iter()).enumerate() {
                let mut sc = base_config.clone();
                sc.output_path = path.clone();
                sc.start_beat = start.as_f32();
                sc.end_beat = end.as_f32();
                // D8: audio per section uses the existing mux path — each
                // section's audio_start_beat is its start, which the muxer
                // turns into a zero-offset slice of the master audio.
                sc.audio_start_beat = start.as_f32();
                let prefix = format!("section {} of {}", i + 1, section_count);
                let aborted = self.run_export_section(sc, bpm, Some(&prefix), cmd_rx, state_tx);
                if aborted {
                    log::info!(
                        "[ContentThread] Section export aborted — stopping remaining sections"
                    );
                    break;
                }
            }
        }

        // Restore playback state (once, after all sections).
        self.engine.set_export_mode(false);
        if cur_w != base_config.width || cur_h != base_config.height {
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
    }

    /// Run one export pass for a single (possibly section) range — the original
    /// single-export body from timing through finalize. The caller owns playback
    /// save/restore and the export-mode / resize lifecycle; this does the
    /// per-range work. Returns `true` when the pass aborted (cancelled or
    /// failed) so the caller stops any remaining sections.
    #[cfg(target_os = "macos")]
    fn run_export_section(
        &mut self,
        mut export_config: manifold_media::export_config::ExportConfig,
        bpm: manifold_core::Bpm,
        progress_prefix: Option<&str>,
        cmd_rx: &Receiver<ContentCommand>,
        state_tx: &Sender<ContentState>,
    ) -> bool {
        use manifold_core::tempo::TempoMapConverter;
        use manifold_media::audio_muxer::AudioMuxer;

        // Re-fetch the project (the caller's borrow ended before entering
        // export mode). Defensive: the caller already resolved it.
        let Some(project) = self.engine.project() else {
            log::error!("[ContentThread] No project loaded, cannot export");
            self.send_export_finished(
                state_tx,
                false,
                "No project loaded".into(),
                &export_config.output_path,
            );
            return true;
        };

        let start_beat = export_config.start_beat;
        let end_beat = export_config.end_beat;

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
            return true;
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

        // BUG-130: resolve FFmpeg presence up front, before rendering a single
        // frame. `session.finalize()` used to be the only place this was
        // checked, which meant a machine without ffmpeg installed burned a
        // full (potentially multi-minute) render only to fail at the very
        // last step. Fail fast instead — and reuse the resolved path at
        // finalize time so it isn't re-resolved (and can't disagree).
        let ffmpeg_path = match Self::ffmpeg_preflight(export_config.has_audio(), || {
            AudioMuxer::resolve_ffmpeg("")
        }) {
            Ok(path) => path,
            Err(reason) => {
                log::error!(
                    "[ContentThread] {reason} — aborting export before rendering any frames"
                );
                let _ = std::fs::remove_file(&mix_wav_path);
                self.send_export_finished(
                    state_tx,
                    false,
                    format!("Export failed: {reason}"),
                    &export_config.output_path,
                );
                return true;
            }
        };

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

        // Seek to start (export mode was entered and the pipeline resized once
        // by the caller, before the section loop).
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
                let _ = std::fs::remove_file(&mix_wav_path);
                self.send_export_finished(
                    state_tx,
                    false,
                    format!("Export failed: {e}"),
                    &export_config.output_path,
                );
                return true;
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
                    progress_prefix,
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
                progress_prefix,
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
        let mut finalize_failed = false;
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
            // FFmpeg was already resolved (and its presence verified when
            // audio muxing is needed) before the frame loop started — BUG-130.
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
                    finalize_failed = true;
                }
            }
        }

        // Remove the temporary audio mixdown WAV (already muxed into the final
        // file; a no-op when no audio was rendered).
        let _ = std::fs::remove_file(&mix_wav_path);

        if failed {
            let msg = if let Some(err) = encode_error {
                format!("Export failed: {err}")
            } else {
                "Export cancelled".into()
            };
            self.send_export_finished(state_tx, false, msg, &export_config.output_path);
        }

        failed || finalize_failed
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
        progress_prefix: Option<&str>,
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
            self.send_export_progress(state_tx, session, progress_prefix);
        }

        None
    }

    /// Extract the raw Metal texture pointer from a native GpuTexture.
    /// Returns `id<MTLTexture>` as `*mut c_void` for the native encoder.
    #[cfg(target_os = "macos")]
    fn get_metal_texture_ptr(texture: &manifold_gpu::GpuTexture) -> Option<*mut std::ffi::c_void> {
        Some(texture.raw_ptr())
    }

    /// Send export progress to the UI thread. BUG-083: `is_exporting` /
    /// `export_progress` / `export_status` were deleted un-consumed by the
    /// 2026-07-09 ContentState orphan purge (UI_PROJECTION_LAYER_DESIGN.md
    /// P0) — this call kept running as a transport keep-alive into a void.
    /// Restored here WITH their UI consumer (the header export status
    /// strip, `app_render.rs`), per I1's "fields land with their consumer
    /// or not at all".
    #[cfg(target_os = "macos")]
    fn send_export_progress(
        &self,
        state_tx: &Sender<ContentState>,
        session: &manifold_media::export_session::ExportSession,
        progress_prefix: Option<&str>,
    ) {
        let status = match progress_prefix {
            Some(prefix) => format!("{prefix} — {}", session.status_text()),
            None => session.status_text(),
        };
        let state = ContentState {
            is_exporting: true,
            export_progress: session.progress(),
            export_status: Arc::from(status),
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

    /// BUG-130: decide whether an export can proceed to frame 0.
    ///
    /// When the export needs audio muxing, ffmpeg must be resolvable — if
    /// it's not, the export must abort now, before any frame is rendered or
    /// encoded, rather than discovering the absence at `finalize()` after a
    /// full (potentially multi-minute) render.
    ///
    /// `resolve` is injected (rather than calling `AudioMuxer::resolve_ffmpeg`
    /// directly) so this decision is unit-testable without depending on
    /// which ffmpeg installs happen to exist on the machine running the test.
    fn ffmpeg_preflight(
        has_audio: bool,
        resolve: impl FnOnce() -> Option<String>,
    ) -> Result<Option<String>, &'static str> {
        if !has_audio {
            return Ok(None);
        }
        match resolve() {
            Some(path) => Ok(Some(path)),
            None => Err("ffmpeg not found (required to mux audio into the export)"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // BUG-130 (a): when the export needs audio muxing, missing ffmpeg must
    // be caught before frame 0 — not discovered only at finalize after a
    // full render.

    #[test]
    fn ffmpeg_preflight_passes_through_when_no_audio() {
        // No audio in this export -> ffmpeg's presence is irrelevant, and the
        // resolver must not even be consulted.
        let mut resolver_called = false;
        let result = ContentThread::ffmpeg_preflight(false, || {
            resolver_called = true;
            None
        });
        assert_eq!(result, Ok(None));
        assert!(!resolver_called, "resolver should be skipped when there's no audio to mux");
    }

    #[test]
    fn ffmpeg_preflight_aborts_before_frame_0_when_audio_needs_missing_ffmpeg() {
        let result = ContentThread::ffmpeg_preflight(true, || None);
        assert!(result.is_err(), "must abort, not silently proceed to render frames");
        let reason = result.unwrap_err();
        assert!(reason.contains("ffmpeg not found"), "reason should be clear: {reason}");
    }

    #[test]
    fn ffmpeg_preflight_proceeds_when_audio_needs_resolvable_ffmpeg() {
        let result = ContentThread::ffmpeg_preflight(true, || Some("/opt/homebrew/bin/ffmpeg".to_string()));
        assert_eq!(result, Ok(Some("/opt/homebrew/bin/ffmpeg".to_string())));
    }

    // ── Section export (docs/SECTION_EXPORT_DESIGN.md section 4) ──

    #[cfg(target_os = "macos")]
    fn plain_marker(beat: f32, name: &str) -> manifold_core::marker::TimelineMarker {
        manifold_core::marker::TimelineMarker::new(Beats::from_f32(beat)).with_name(name)
    }

    #[cfg(target_os = "macos")]
    fn section_marker(beat: f32, name: &str) -> manifold_core::marker::TimelineMarker {
        plain_marker(beat, name).as_section()
    }

    #[cfg(target_os = "macos")]
    fn timeline_with(markers: Vec<manifold_core::marker::TimelineMarker>) -> manifold_core::timeline::Timeline {
        let mut t = manifold_core::timeline::Timeline::default();
        for m in markers {
            t.add_marker(m);
        }
        t
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn derive_sections_returns_empty_without_in_range_section_markers() {
        // Invariant (b): flag on but no in-range section markers → empty →
        // the caller takes the single-export path (one file at output_path).
        // Non-section markers and out-of-range section markers must not slice.
        let t = timeline_with(vec![
            plain_marker(2.0, "plain"),
            section_marker(20.0, "outside"),
        ]);
        assert!(derive_sections(&t, Beats::from_f32(4.0), Beats::from_f32(16.0)).is_empty());

        // No section markers at all → empty over any range.
        let t = timeline_with(vec![plain_marker(2.0, "plain")]);
        assert!(derive_sections(&t, Beats::from_f32(0.0), Beats::from_f32(100.0)).is_empty());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn derive_sections_splits_chapter_style() {
        // D2: sections are [in, m₁), [m₁, m₂), …, [mₙ, out); each section named
        // by the marker at its start; the leading [in, m₁) section is unnamed.
        let t = timeline_with(vec![
            section_marker(4.0, "Drop"),
            section_marker(8.0, "Break"),
            plain_marker(6.0, "not-a-section"),
        ]);
        let sections = derive_sections(&t, Beats::from_f32(0.0), Beats::from_f32(16.0));
        assert_eq!(
            sections,
            vec![
                (Beats::from_f32(0.0), Beats::from_f32(4.0), String::new()),
                (Beats::from_f32(4.0), Beats::from_f32(8.0), "Drop".to_string()),
                (Beats::from_f32(8.0), Beats::from_f32(16.0), "Break".to_string()),
            ]
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn derive_sections_edge_cases() {
        // Marker exactly on `in` is excluded (would produce an empty leading
        // section); marker exactly on `out` is excluded (outside the half-open
        // range). Both must not appear as cuts.
        let t = timeline_with(vec![
            section_marker(0.0, "OnIn"),
            section_marker(8.0, "Mid"),
            section_marker(16.0, "OnOut"),
        ]);
        let sections = derive_sections(&t, Beats::from_f32(0.0), Beats::from_f32(16.0));
        assert_eq!(
            sections,
            vec![
                (Beats::from_f32(0.0), Beats::from_f32(8.0), String::new()),
                (Beats::from_f32(8.0), Beats::from_f32(16.0), "Mid".to_string()),
            ]
        );

        // Duplicate beats collapse to a single cut. The surviving name is the
        // first in marker-list order (stable sort + dedup keeps the first), so
        // set the list directly to pin the order rather than relying on
        // `add_marker`'s insert-before-equal placement.
        let mut t = manifold_core::timeline::Timeline::default();
        t.markers = vec![
            section_marker(4.0, "First"),
            section_marker(4.0, "Second"),
            section_marker(8.0, "Next"),
        ];
        let sections = derive_sections(&t, Beats::from_f32(0.0), Beats::from_f32(16.0));
        assert_eq!(
            sections,
            vec![
                (Beats::from_f32(0.0), Beats::from_f32(4.0), String::new()),
                (Beats::from_f32(4.0), Beats::from_f32(8.0), "First".to_string()),
                (Beats::from_f32(8.0), Beats::from_f32(16.0), "Next".to_string()),
            ]
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn derive_sections_sorts_unsorted_markers() {
        // `add_marker` keeps `markers` sorted, but `derive_sections` must not
        // depend on that: hand it an unsorted `markers` vec directly.
        let mut t = manifold_core::timeline::Timeline::default();
        t.markers = vec![
            section_marker(8.0, "Break"),
            section_marker(4.0, "Drop"),
        ];
        let sections = derive_sections(&t, Beats::from_f32(0.0), Beats::from_f32(16.0));
        assert_eq!(
            sections,
            vec![
                (Beats::from_f32(0.0), Beats::from_f32(4.0), String::new()),
                (Beats::from_f32(4.0), Beats::from_f32(8.0), "Drop".to_string()),
                (Beats::from_f32(8.0), Beats::from_f32(16.0), "Break".to_string()),
            ]
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn section_output_paths_sanitize_empty_and_collisions() {
        use std::path::Path;
        let base = Path::new("/tmp/export")
            .join("my show.mp4")
            .to_string_lossy()
            .into_owned();

        // Sanitization + the leading unnamed section → `section-1`.
        let sections = vec![
            (Beats::from_f32(0.0), Beats::from_f32(4.0), String::new()),
            (Beats::from_f32(4.0), Beats::from_f32(8.0), "Drop Build Up!".to_string()),
            (Beats::from_f32(8.0), Beats::from_f32(16.0), "Drop Build Up!".to_string()),
        ];
        let paths = section_output_paths(&base, &sections);
        assert_eq!(
            paths,
            vec![
                "/tmp/export/my show--section-1.mp4",
                "/tmp/export/my show--Drop-Build-Up.mp4",
                "/tmp/export/my show--Drop-Build-Up-2.mp4",
            ]
        );

        // An empty-but-named fallback colliding with an explicit `section-1`.
        let sections = vec![
            (Beats::from_f32(0.0), Beats::from_f32(4.0), String::new()),
            (Beats::from_f32(4.0), Beats::from_f32(8.0), "section-1".to_string()),
        ];
        let paths = section_output_paths(&base, &sections);
        assert_eq!(
            paths,
            vec![
                "/tmp/export/my show--section-1.mp4",
                "/tmp/export/my show--section-1-2.mp4",
            ]
        );
    }
}
