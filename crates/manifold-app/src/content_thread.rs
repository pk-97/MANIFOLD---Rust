//! Content thread — runs PlaybackEngine, EditingService, and ContentPipeline
//! on a dedicated thread. Communicates with the UI thread via crossbeam channels.
//!
//! The content thread owns all authoritative state: the engine (which owns the
//! project), the editing service (undo/redo), audio sync, percussion, and the
//! GPU content pipeline (generators + compositor).
use crossbeam_channel::{Receiver, Sender};

use manifold_core::math::BeatQuantizer;
use manifold_core::types::{ClockAuthority, PlaybackState, TempoPointSource};
use manifold_editing::service::EditingService;
use manifold_playback::audio_sync::ImportedAudioSyncController;
use manifold_playback::stem_audio::StemAudioController;
use manifold_playback::clip_launcher::ClipLauncher;
use manifold_playback::engine::{PlaybackEngine, TickContext};
use manifold_playback::midi_input::MidiInputController;
use manifold_playback::osc_receiver::OscReceiver;
use manifold_playback::osc_sender::OscPositionSender;
use manifold_playback::osc_sync::OscSyncController;
use manifold_playback::percussion_orchestrator::PercussionImportOrchestrator;
use manifold_playback::sync::{SyncArbiter, SyncTargetSnapshot};
use manifold_playback::tempo_recorder::TempoRecorder;
use manifold_playback::transport_controller::TransportController;
use manifold_renderer::gpu::GpuContext;

use crate::content_command::ContentCommand;
use crate::content_pipeline::ContentPipeline;
use crate::content_state::{ContentState, ExportFinishedEvent};
use crate::frame_timer::FrameTimer;

/// Owns all content-side state and runs the content loop.
pub struct ContentThread {
    pub engine: PlaybackEngine,
    pub editing_service: EditingService,
    pub content_pipeline: ContentPipeline,
    pub audio_sync: Option<ImportedAudioSyncController>,
    pub stem_audio: Option<StemAudioController>,
    pub percussion_orchestrator: PercussionImportOrchestrator,
    pub transport_controller: TransportController,
    pub gpu: GpuContext,
    pub frame_count: u64,
    pub time_since_start: f32,
    pub last_data_version: u64,
    /// MIDI device input — routes hardware note events to ClipLauncher.
    pub midi_input: MidiInputController,
    /// Bridges MIDI note events to LiveClipManager.
    pub clip_launcher: ClipLauncher,
    /// When true, skip tick+render but still drain commands.
    /// Used while native file dialogs are open on macOS.
    pub rendering_paused: bool,
    /// Content frame timer — target FPS synced from project settings.
    pub timer: FrameTimer,

    // ── Sync infrastructure ──
    /// Authority gatekeeper — only the active ClockAuthority can issue transport commands.
    pub sync_arbiter: SyncArbiter,
    /// OSC UDP listener — background thread receives, main thread dispatches.
    pub osc_receiver: OscReceiver,
    /// OSC timecode sync controller (LiveMTC bridge).
    pub osc_sync: OscSyncController,
    /// OSC position sender — sends transport state to DAW (LateUpdate equivalent).
    pub osc_sender: OscPositionSender,
    /// OSC parameter router — maps incoming OSC floats to effect/generator params.
    /// Port of Unity's MasterEffectOscBridge + LayerOscBridge + LayerEffectOscBridge
    /// + GeneratorOscBridge as a single data-driven unit.
    pub osc_param_router: manifold_playback::osc_param_router::OscParamRouter,

    // ── Tempo recording (port of C# PlaybackController fields) ──
    /// Tempo recording/provenance — tracks external tempo for tempo automation.
    /// Port of C# PlaybackController.tempoRecorder field.
    pub tempo_recorder: TempoRecorder,
    /// Offset between Link's absolute beat epoch and MANIFOLD's timeline beat 0.
    /// Cached ONLY at Play()/Seek() sync points. NOT refreshed periodically —
    /// Link's cumulative beat counter keeps the offset valid across BPM changes.
    /// Port of C# PlaybackController.linkBeatOffset field (line 74).
    pub link_beat_offset: f64,

    // ── LED output ──
    /// LED/ArtNet output controller. None when not initialized.
    pub led_controller: Option<manifold_led::LedOutputController>,

    // ── MIDI device cache ──
    /// Cached MIDI device names, refreshed every ~2 seconds.
    pub cached_midi_device_names: Vec<String>,
    pub last_midi_device_scan_time: f32,

    // ── Cached project snapshot (Arc avoids deep clone every modulation frame) ──
    pub cached_project_snapshot: Option<std::sync::Arc<manifold_core::project::Project>>,

    // ── Cached ContentState strings (only rebuilt when changed) ──
    pub cached_midi_clock_position: String,
    pub cached_midi_clock_device: String,
    pub cached_osc_timecode: String,
    pub cached_perc_message: String,
    /// Last-sent MIDI device names — only clone when the list changes.
    pub last_sent_midi_device_names: Vec<String>,

    // ── Profiling ──
    /// Active profiling session (only present when feature = "profiling").
    #[cfg(feature = "profiling")]
    pub profiler: Option<manifold_profiler::ProfileSession>,
}

impl ContentThread {
    /// Run the content loop. Blocks until Shutdown is received.
    pub fn run(
        mut self,
        cmd_rx: Receiver<ContentCommand>,
        state_tx: Sender<ContentState>,
    ) {
        log::info!("[ContentThread] started");

        // Set content thread to real-time scheduling priority.
        // Priority 47 is high but leaves headroom for the audio thread (max=48).
        // Reduces context switch latency and sleep overshoot.
        #[cfg(target_os = "macos")]
        {
            let pthread = unsafe { libc::pthread_self() };
            let mut param: libc::sched_param = unsafe { std::mem::zeroed() };
            param.sched_priority = 47;
            let ret = unsafe { libc::pthread_setschedparam(pthread, libc::SCHED_RR, &param) };
            if ret != 0 {
                log::warn!(
                    "[ContentThread] Failed to set real-time priority (err={}), \
                     continuing with default priority",
                    ret,
                );
            } else {
                log::info!("[ContentThread] Real-time priority set (SCHED_RR, priority=47)");
            }
        }

        // Set stable device pointer on renderers that cache a *const GpuDevice.
        // This must happen here (after all moves into ContentThread are complete)
        // so the pointer targets the final heap location inside content_pipeline.
        {
            let native_device_ref = self.content_pipeline.native_device().unwrap();
            let (renderers, _) = self.engine.split_renderer_project();
            for renderer in renderers.iter_mut() {
                if let Some(gen_renderer) = renderer
                    .as_any_mut()
                    .downcast_mut::<manifold_renderer::generator_renderer::GeneratorRenderer>()
                {
                    gen_renderer.set_device(native_device_ref);
                }
                #[cfg(target_os = "macos")]
                if let Some(vid_renderer) = renderer
                    .as_any_mut()
                    .downcast_mut::<manifold_media::video_renderer::VideoRenderer>()
                {
                    vid_renderer.set_device(native_device_ref);
                }
            }
        }

        // Auto-initialize LED output with default settings (native Metal).
        // Can be reconfigured at runtime via InitLedOutput command.
        {
            let settings = manifold_led::LedSettings::default();
            let mut ctrl = manifold_led::LedOutputController::new();
            let native_device = self.content_pipeline.native_device()
                .expect("native device required for LED init");
            if ctrl.initialize(native_device, &settings) {
                self.led_controller = Some(ctrl);
                eprintln!("[LED] Auto-initialized: {}x{} LEDs, target={}:{}",
                    settings.strip_count, settings.leds_per_strip,
                    settings.artnet_ip, settings.artnet_port);
            } else {
                eprintln!("[LED] Auto-init FAILED");
            }
        }

        loop {
            // 1. Drain ALL pending commands
            loop {
                match cmd_rx.try_recv() {
                    Ok(ContentCommand::StartExport(config)) => {
                        self.run_export(*config, &cmd_rx, &state_tx);
                    }
                    Ok(cmd) => {
                        if self.handle_command(cmd) {
                            log::info!("[ContentThread] shutdown received");
                            return;
                        }
                    }
                    Err(crossbeam_channel::TryRecvError::Empty) => break,
                    Err(crossbeam_channel::TryRecvError::Disconnected) => {
                        log::info!("[ContentThread] command channel disconnected, shutting down");
                        return;
                    }
                }
            }

            // 2. Wait for next content frame (skip tick+render when paused)
            if self.rendering_paused {
                std::thread::sleep(std::time::Duration::from_millis(16));
                continue;
            }
            if !self.timer.should_tick() {
                // Precise sleep: compute exact time to next frame, sleep most of it,
                // then spin-wait for the final stretch to avoid macOS sleep overshoot.
                // macOS thread::sleep overshoots by 2-4ms under load. A 3ms spin margin
                // keeps the content thread hitting its target FPS consistently.
                let remaining = self.timer.time_until_next_tick();
                if remaining > std::time::Duration::from_millis(4) {
                    // Sleep for most of the remaining time, leaving 3ms margin for spin-wait
                    std::thread::sleep(remaining - std::time::Duration::from_millis(3));
                } else if remaining > std::time::Duration::from_micros(100) {
                    // Close to deadline — yield to OS scheduler instead of sleeping
                    std::thread::yield_now();
                }
                // Below 100μs: fall through to re-check should_tick() immediately
                continue;
            }
            // Drain autoreleased ObjC Metal objects at the end of each frame,
            // preventing memory accumulation and random GC-like pauses.
            #[cfg(target_os = "macos")]
            objc::rc::autoreleasepool(|| {
                self.tick_frame(&state_tx);
            });
            #[cfg(not(target_os = "macos"))]
            self.tick_frame(&state_tx);
        }
    }

    /// Execute one content frame: tick engine, render, send state to UI.
    /// Separated from the main loop to allow wrapping in autoreleasepool on macOS.
    fn tick_frame(&mut self, state_tx: &Sender<ContentState>) {
            let dt = self.timer.consume_tick();
            let realtime = self.timer.realtime_since_start();
            self.time_since_start = realtime as f32;

            // Refresh MIDI device list every ~2 seconds
            if self.time_since_start - self.last_midi_device_scan_time >= 2.0 {
                self.cached_midi_device_names =
                    manifold_playback::midi_clock_sync::MidiClockSyncController::available_source_names();
                self.last_midi_device_scan_time = self.time_since_start;
            }

            // Profiling: frame start timestamp
            #[cfg(feature = "profiling")]
            let _frame_start = std::time::Instant::now();

            // 3. Process MIDI input (before engine tick — matches Unity Update() ordering).
            // Drains hardware note events and routes them to ClipLauncher → LiveClipManager.
            #[cfg(feature = "profiling")]
            let _t0 = std::time::Instant::now();

            self.engine.tick_midi_input(
                &mut self.midi_input,
                &mut self.clip_launcher,
                realtime,
            );

            #[cfg(feature = "profiling")]
            let _midi_input_ms = _t0.elapsed().as_secs_f64() * 1000.0;

            // 3b. Sync controller updates (before engine tick — Unity execution order -100).
            // Link, MidiClock, and OSC poll their sources and issue gated transport
            // commands via SyncArbiter. Snapshot read-only state before mutable borrows.
            #[cfg(feature = "profiling")]
            let _t0 = std::time::Instant::now();

            self.tick_sync_controllers();

            // 3c. External beat derivation + tempo recording/resolution.
            // Port of C# PlaybackController.Update lines 1064-1099.
            // Must run AFTER sync controllers (which set live external tempo)
            // and BEFORE engine.tick() (which uses the derived beat).
            let authority = self.engine.project()
                .map_or(ClockAuthority::Internal, |p| p.settings.clock_authority);
            self.derive_external_beat(authority);
            self.update_recording_session_state(authority);
            self.apply_resolved_tempo(authority);

            #[cfg(feature = "profiling")]
            let _sync_controllers_ms = _t0.elapsed().as_secs_f64() * 1000.0;

            // 4. Tick engine
            #[cfg(feature = "profiling")]
            let _t0 = std::time::Instant::now();

            let ctx = TickContext {
                dt_seconds: dt,
                realtime_now: realtime,
                pre_render_dt: dt as f32,
                frame_count: self.frame_count as i32,
                export_fixed_dt: 0.0,
            };
            let tick_result = self.engine.tick(ctx);

            // 4b. OscPositionSender (LateUpdate equivalent — after engine tick).
            if self.transport_controller.osc_sender_enabled {
                let bpm = self.engine.project().map_or(120.0_f32, |p| p.settings.bpm);
                let seconds_per_beat = if bpm > 0.0 { 60.0 / bpm } else { 0.5 };
                self.osc_sender.late_update(
                    self.engine.is_playing(),
                    self.engine.current_beat(),
                    seconds_per_beat,
                    realtime,
                    &mut self.sync_arbiter,
                );
            }

            // 5. Audio sync
            if let Some(ref mut audio_sync) = self.audio_sync {
                audio_sync.update_sync(&mut self.engine);
            }

            // 5b. Stem audio sync (after master — matches Unity Update() ordering).
            if let Some(ref mut stem_audio) = self.stem_audio
                && let Some(ref audio_sync) = self.audio_sync {
                    stem_audio.update_sync(audio_sync, &self.engine);
                }

            // 6. Percussion tick
            let beat = self.engine.current_beat();
            if let Some(p) = self.engine.project_mut() {
                self.percussion_orchestrator.tick(
                    self.time_since_start,
                    p,
                    &mut self.editing_service,
                    beat,
                );
            }

            // 6b. Video prewarm — pass lookahead candidates to VideoRenderer
            //     so decoders are opened before clips become active (prevents
            //     black frames at clip start). Port of Unity WorkspaceController
            //     → VideoPlayerPool.WarmCache(candidates).
            if let Some(ref candidates) = tick_result.prewarm_candidates {
                for renderer in self.engine.renderers_mut() {
                    if let Some(vid) = renderer
                        .as_any_mut()
                        .downcast_mut::<manifold_media::video_renderer::VideoRenderer>()
                    {
                        vid.pre_warm_from_candidates(candidates);
                        break;
                    }
                }
            }

            #[cfg(feature = "profiling")]
            let _engine_tick_ms = _t0.elapsed().as_secs_f64() * 1000.0;

            // 7. Render content
            #[cfg(feature = "profiling")]
            let _t0 = std::time::Instant::now();

            let render_work_start = std::time::Instant::now();
            self.content_pipeline.render_content(
                &self.gpu, &mut self.engine, &tick_result, dt, self.frame_count,
                false,
            );
            let _render_work_ms = render_work_start.elapsed().as_secs_f64() * 1000.0;

            #[cfg(feature = "profiling")]
            let _render_content_ms = _t0.elapsed().as_secs_f64() * 1000.0;
            #[cfg(feature = "profiling")]
            let _gpu_poll_ms = self.content_pipeline.last_gpu_poll_ms();

            // 7b. Clean up per-owner effect state for clips that stopped this tick.
            // Releases GPU textures/buffers (Feedback, Bloom, PixelSort, etc.)
            // to prevent unbounded GPU memory growth.
            #[cfg(feature = "profiling")]
            let _t0 = std::time::Instant::now();

            if !tick_result.stopped_clips.is_empty() {
                self.content_pipeline.cleanup_stopped_clips(&tick_result.stopped_clips);
            }

            // 7c. LED output — native Metal: dispatch edge-extend compute on
            // compositor output, readback tiny pixel grid, send DMX/ArtNet.
            // Uses a dedicated encoder (separate from the content frame).
            if let Some(ref mut led) = self.led_controller {
                // Poll previous frame's readback (send DMX if ready).
                led.poll_readback();
                // Submit new frame: edge-extend compute + readback copy.
                let native_device = self.content_pipeline.native_device().unwrap();
                let source = self.content_pipeline.led_source_texture();
                led.process_frame(
                    native_device,
                    source,
                    tick_result.ready_clips.len(),
                    self.engine.project().map_or(1.0, |p| p.settings.led_brightness),
                );
            }

            #[cfg(feature = "profiling")]
            let _cleanup_ms = _t0.elapsed().as_secs_f64() * 1000.0;

            self.frame_count += 1;

            // Profiling: record frame data
            #[cfg(feature = "profiling")]
            if let Some(ref mut profiler) = self.profiler
                && profiler.is_recording()
            {
                let frame_wall_ms = _frame_start.elapsed().as_secs_f64() * 1000.0;
                let current_beat = self.engine.current_beat();
                let time_sig = self.engine.project()
                    .map_or(4, |p| p.settings.time_signature_numerator.max(1));
                let bar = (current_beat / time_sig as f32).floor() as u32;
                let budget_ms = 1000.0 / self.timer.target_fps();
                let active_layers = self.engine.project()
                    .map_or(0, |p| p.timeline.layers.len());

                // Collect GPU pass timing results with resolution + absolute timestamps
                let gpu_pass_results = self.content_pipeline.last_gpu_pass_results();
                let gpu_pass_count = gpu_pass_results.len() as u32;
                let gpu_total_ms: f64 = gpu_pass_results.iter()
                    .map(|p| p.duration_ms).sum();
                let gpu_passes: Vec<manifold_profiler::GpuPassRecord> =
                    gpu_pass_results.iter()
                        .map(|p| manifold_profiler::GpuPassRecord {
                            name: p.label.clone(),
                            ms: p.duration_ms,
                            begin_ns: p.begin_ns,
                            end_ns: p.end_ns,
                            width: p.width,
                            height: p.height,
                            is_compute: p.is_compute,
                        })
                        .collect();

                // Helper: build named params from values + registry
                fn build_effect_params(fx: &manifold_core::effects::EffectInstance) -> Vec<manifold_profiler::NamedParam> {
                    let def = manifold_core::effect_definition_registry::try_get(fx.effect_type());
                    fx.param_values.iter().enumerate().map(|(i, &v)| {
                        let name = def.and_then(|d| d.param_defs.get(i))
                            .map_or_else(|| format!("param_{}", i), |pd| pd.name.clone());
                        manifold_profiler::NamedParam { name, value: v }
                    }).collect()
                }

                fn build_gen_params(gen_type: &manifold_core::GeneratorTypeId, values: &[f32]) -> Vec<manifold_profiler::NamedParam> {
                    let def = manifold_core::generator_definition_registry::try_get(gen_type);
                    values.iter().enumerate().map(|(i, &v)| {
                        let name = def.and_then(|d| d.param_defs.get(i))
                            .map_or_else(|| format!("param_{}", i), |pd| pd.name.clone());
                        manifold_profiler::NamedParam { name, value: v }
                    }).collect()
                }

                // Get anim_progress from generator_renderer (mutable borrow, done first)
                let anim_map: Vec<(String, f32)> = {
                    let (renderers, _) = self.engine.split_renderer_project();
                    let gen_renderer = renderers.iter().find_map(|r| {
                        r.as_any().downcast_ref::<manifold_renderer::generator_renderer::GeneratorRenderer>()
                    });
                    tick_result.ready_clips.iter().map(|clip| {
                        let progress = gen_renderer
                            .map_or(0.0, |gr| gr.get_clip_anim_progress(clip.id.as_str()));
                        (clip.id.to_string(), progress)
                    }).collect()
                };

                // Now borrow project immutably for layers, effects, params
                let layers = self.engine.project()
                    .map(|p| p.timeline.layers.as_slice())
                    .unwrap_or(&[]);

                let active_clip_info: Vec<manifold_profiler::ActiveClipInfo> =
                    tick_result.ready_clips.iter().enumerate().map(|(i, clip)| {
                        let clip_layer_idx = self.engine.project()
                            .and_then(|p| p.timeline.layer_index_for_id(&clip.layer_id))
                            .unwrap_or(0);
                        let gen_param_values = layers.get(clip_layer_idx)
                            .and_then(|l| l.gen_params());
                        let gen_params = gen_param_values
                            .map(|gp| build_gen_params(&clip.generator_type, &gp.param_values))
                            .unwrap_or_default();
                        let anim_progress = anim_map.get(i).map_or(0.0, |a| a.1);
                        manifold_profiler::ActiveClipInfo {
                            clip_id: clip.id.to_string(),
                            generator_type: clip.generator_type.to_string(),
                            layer_index: clip_layer_idx as i32,
                            anim_progress,
                            gen_params,
                        }
                    }).collect();

                // Collect active effect info with named live params + group_id
                let mut active_effects: Vec<manifold_profiler::ActiveEffectInfo> = Vec::new();
                for clip in &tick_result.ready_clips {
                    for fx in &clip.effects {
                        if fx.enabled {
                            active_effects.push(manifold_profiler::ActiveEffectInfo {
                                effect_type: fx.effect_type().to_string(),
                                scope: format!("clip:{}", clip.id),
                                group_id: fx.group_id.as_ref().map(|g| g.to_string()),
                                params: build_effect_params(fx),
                            });
                        }
                    }
                }
                for layer in layers {
                    if let Some(layer_fxs) = layer.effects.as_deref() {
                        for fx in layer_fxs {
                            if fx.enabled {
                                active_effects.push(manifold_profiler::ActiveEffectInfo {
                                    effect_type: fx.effect_type().to_string(),
                                    scope: format!("layer:{}", layer.index),
                                    group_id: fx.group_id.as_ref().map(|g| g.to_string()),
                                    params: build_effect_params(fx),
                                });
                            }
                        }
                    }
                }
                if let Some(p) = self.engine.project() {
                    for fx in &p.settings.master_effects {
                        if fx.enabled {
                            active_effects.push(manifold_profiler::ActiveEffectInfo {
                                effect_type: fx.effect_type().to_string(),
                                scope: "master".to_string(),
                                group_id: fx.group_id.as_ref().map(|g| g.to_string()),
                                params: build_effect_params(fx),
                            });
                        }
                    }
                }

                // Layer states (opacity, mute, solo)
                let layer_states: Vec<manifold_profiler::LayerState> = layers.iter()
                    .map(|l| manifold_profiler::LayerState {
                        index: l.index,
                        opacity: l.opacity,
                        is_muted: l.is_muted,
                        is_solo: l.is_solo,
                    })
                    .collect();

                // Memory estimate: compositor dimensions × 16 bytes (Rgba16Float) × buffer count
                let (comp_w, comp_h) = self.content_pipeline.dimensions();
                let bytes_per_pixel = 8u64; // Rgba16Float
                let rt_count = tick_result.ready_clips.len() as u32 + 4; // clips + main + ping/pong + tonemap
                let estimated_tex_bytes = comp_w as u64 * comp_h as u64 * bytes_per_pixel * rt_count as u64;

                profiler.record_frame(manifold_profiler::FrameRecord {
                    index: self.frame_count - 1,
                    beat: current_beat,
                    bar,
                    wall_time_ms: frame_wall_ms,
                    budget_exceeded: frame_wall_ms > budget_ms,
                    content_thread: manifold_profiler::ContentTimings {
                        total_ms: frame_wall_ms,
                        midi_input_ms: _midi_input_ms,
                        sync_controllers_ms: _sync_controllers_ms,
                        engine_tick_ms: _engine_tick_ms,
                        render_content_ms: _render_content_ms,
                        gpu_poll_ms: _gpu_poll_ms,
                        cleanup_ms: _cleanup_ms,
                    },
                    gpu_passes,
                    active_clips: active_clip_info,
                    active_effects,
                    active_layer_count: active_layers,
                    gpu_pass_count,
                    gpu_total_ms,
                    layer_states,
                    missed_frames: self.timer.missed_ticks(),
                    profiler_overhead_ms: self.content_pipeline.profiler_overhead_ms(),
                    memory: manifold_profiler::MemorySnapshot {
                        estimated_texture_bytes: estimated_tex_bytes,
                        render_target_count: rt_count,
                    },
                });
            }

            // 8. Push state to UI
            let version = self.editing_service.data_version();
            let version_changed = version != self.last_data_version;
            if version_changed {
                self.last_data_version = version;
            }
            // Send a project snapshot when data_version changes (editing commands)
            // OR when modulation is active (LFO/envelope writes to param_values
            // without bumping data_version — UI needs live modulated values).
            let modulation_active = tick_result.modulation_active;

            // Reclaim tick_result buffers (ready_clips, stopped_clips) for reuse
            // on the next tick — avoids per-frame Vec allocation.
            self.engine.reclaim_tick_result(tick_result);

            // Arc<Project> snapshot: only deep-clone when data_version changes.
            // Modulation frames send the same Arc (zero-cost pointer clone).
            let snapshot = if version_changed {
                // Structural change — create a new Arc with a fresh clone.
                let arc = self.engine.project()
                    .map(|p| std::sync::Arc::new(p.clone()));
                self.cached_project_snapshot = arc.clone();
                arc
            } else if modulation_active {
                // Modulation only — reuse the existing Arc (no deep clone).
                // If no cached snapshot exists yet, create one.
                if self.cached_project_snapshot.is_none() {
                    self.cached_project_snapshot = self.engine.project()
                        .map(|p| std::sync::Arc::new(p.clone()));
                }
                self.cached_project_snapshot.clone()
            } else {
                None
            };

            // Update cached strings only when underlying values change.
            let new_pos = self.transport_controller.midi_clock_sync.as_ref()
                .map_or("", |s| s.current_position_display());
            if new_pos != self.cached_midi_clock_position {
                self.cached_midi_clock_position.clear();
                self.cached_midi_clock_position.push_str(new_pos);
            }
            let new_dev = self.transport_controller.midi_clock_sync.as_ref()
                .map_or_else(String::new, |s| s.selected_source_name());
            if new_dev != self.cached_midi_clock_device {
                self.cached_midi_clock_device = new_dev;
            }
            if self.osc_sync.current_timecode_display != self.cached_osc_timecode {
                self.cached_osc_timecode.clone_from(&self.osc_sync.current_timecode_display);
            }
            let new_perc = self.percussion_orchestrator.status_message();
            if new_perc != self.cached_perc_message {
                self.cached_perc_message.clear();
                self.cached_perc_message.push_str(new_perc);
            }
            if self.cached_midi_device_names != self.last_sent_midi_device_names {
                self.last_sent_midi_device_names.clone_from(&self.cached_midi_device_names);
            }

            let perc_progress = self.percussion_orchestrator.status_progress01();
            let perc_show = self.percussion_orchestrator.show_progress_bar()
                && !self.cached_perc_message.is_empty();

            let state = ContentState {
                current_beat: self.engine.current_beat(),
                current_time: self.engine.current_time(),
                is_playing: self.engine.is_playing(),
                is_recording: self.engine.is_recording(),
                content_fps: self.timer.current_fps() as f32,
                content_frame_time_ms: (self.timer.last_dt() * 1000.0) as f32,
                active_clips: self.engine.active_clip_count(),
                data_version: version,
                editing_is_dirty: self.editing_service.is_dirty(),
                bpm: self.engine.project().map_or(120.0, |p| p.settings.bpm as f64),
                frame_rate: self.engine.project().map_or(60.0, |p| p.settings.frame_rate as f64),
                clock_authority: self.engine.project()
                    .map_or(manifold_core::types::ClockAuthority::Internal, |p| p.settings.clock_authority),
                time_signature_numerator: self.engine.project()
                    .map_or(4, |p| p.settings.time_signature_numerator),
                link_enabled: self.transport_controller.link_sync.as_ref()
                    .is_some_and(|s| s.is_link_enabled()),
                link_tempo: self.transport_controller.link_sync.as_ref()
                    .map_or(120.0, |s| s.link_tempo),
                link_peers: self.transport_controller.link_sync.as_ref()
                    .map_or(0, |s| s.num_peers),
                link_is_playing: self.transport_controller.link_sync.as_ref()
                    .is_some_and(|s| s.link_is_playing),
                midi_clock_enabled: self.transport_controller.midi_clock_sync.as_ref()
                    .is_some_and(|s| s.is_midi_clock_enabled()),
                midi_clock_bpm: self.transport_controller.midi_clock_sync.as_ref()
                    .map_or(120.0, |s| s.current_clock_bpm()),
                midi_clock_position_display: self.cached_midi_clock_position.clone(),
                midi_clock_receiving: self.transport_controller.midi_clock_sync.as_ref()
                    .is_some_and(|s| s.is_receiving_clock()),
                midi_clock_device_name: self.cached_midi_clock_device.clone(),
                midi_device_names: self.last_sent_midi_device_names.clone(),
                osc_sender_enabled: self.transport_controller.osc_sender_enabled,
                osc_receiving_timecode: self.osc_sync.is_receiving_timecode,
                osc_timecode_display: self.cached_osc_timecode.clone(),
                stem_expanded: self.stem_audio.as_ref().is_some_and(|s| s.is_expanded()),
                stem_ready: self.stem_audio.as_ref().is_some_and(|s| s.stems_ready()),
                stem_muted: self.stem_audio.as_ref().map_or([false; manifold_playback::stem_audio::STEM_COUNT], |s| {
                    core::array::from_fn(|i| s.is_muted(i))
                }),
                stem_soloed: self.stem_audio.as_ref().map_or([false; manifold_playback::stem_audio::STEM_COUNT], |s| {
                    core::array::from_fn(|i| s.is_soloed(i))
                }),
                stem_available: self.stem_audio.as_ref().map_or([false; manifold_playback::stem_audio::STEM_COUNT], |s| {
                    core::array::from_fn(|i| s.is_stem_available(i))
                }),
                percussion_importing: self.percussion_orchestrator.is_import_in_progress(),
                percussion_status_message: self.cached_perc_message.clone(),
                percussion_progress: if perc_progress < 0.0 { 0.0 } else { perc_progress.clamp(0.0, 1.0) },
                percussion_show_progress: perc_show,
                profiling_active: {
                    #[cfg(feature = "profiling")]
                    { self.profiler.as_ref().is_some_and(|p| p.is_recording()) }
                    #[cfg(not(feature = "profiling"))]
                    { false }
                },
                profiling_frame_count: {
                    #[cfg(feature = "profiling")]
                    { self.profiler.as_ref().map_or(0, |p| p.frame_count()) }
                    #[cfg(not(feature = "profiling"))]
                    { 0 }
                },
                led_enabled: self.led_controller.as_ref().is_some_and(|c| c.is_enabled()),
                led_initialized: self.led_controller.as_ref().is_some_and(|c| c.is_initialized()),
                is_exporting: false,
                export_progress: 0.0,
                export_status: String::new(),
                export_finished: None,
                project_snapshot: snapshot,
            };

            // Non-blocking send — if the UI is behind, drop the oldest state.
            let _ = state_tx.try_send(state);
    }

    /// Tick all sync controllers once per frame. Called before engine tick.
    /// Handles the borrow-split problem: snapshot read-only engine state first,
    /// then pass &mut engine for transport commands via SyncArbiter.
    fn tick_sync_controllers(&mut self) {
        let now = self.time_since_start;

        // Auto-determine clock authority BEFORE sync controllers run.
        // Uses previous frame's receiving/peer state (updated by sync controllers
        // last frame). This ensures the SyncArbiter gates are consistent with
        // the authority — prevents one-frame mismatch where external_time_sync
        // or transport commands are incorrectly rejected.
        let authority = {
            let auto = if self.transport_controller.midi_clock_sync.as_ref()
                .is_some_and(|s| s.is_midi_clock_enabled() && s.is_receiving_clock())
            {
                ClockAuthority::MidiClock
            } else if self.osc_sync.is_receiving_timecode {
                ClockAuthority::Osc
            } else if self.transport_controller.link_sync.as_ref()
                .is_some_and(|s| s.is_link_enabled() && s.has_active_peers())
            {
                ClockAuthority::Link
            } else {
                ClockAuthority::Internal
            };
            if let Some(project) = self.engine.project_mut() {
                project.settings.clock_authority = auto;
            }
            auto
        };

        // Link sync — poll beat/phase/tempo from Ableton Link network.
        let link_has_tempo = if let Some(ref mut link) = self.transport_controller.link_sync {
            link.update(
                &mut self.sync_arbiter,
                &mut self.engine,
                authority,
            );
            // Link provides the most accurate BPM when peers are connected.
            if link.is_link_enabled() && link.has_active_peers() {
                self.engine.set_live_external_tempo(
                    true,
                    link.link_tempo as f32,
                    TempoPointSource::Link,
                );
                true
            } else {
                false
            }
        } else {
            false
        };

        // MIDI Clock sync — poll clock/SPP from midir.
        // Snapshot SyncTarget state before passing &mut engine as SyncArbiterTarget.
        if let Some(ref mut clk) = self.transport_controller.midi_clock_sync {
            let snap = SyncTargetSnapshot::from_engine(&self.engine);
            clk.update(
                now,
                &mut self.sync_arbiter,
                &mut self.engine,
                &snap,
                authority,
            );
            // Feed live MIDI Clock BPM to engine — but Link takes priority
            // when available (more accurate, network-synced tempo).
            if !link_has_tempo && clk.is_midi_clock_enabled() && clk.is_receiving_clock() {
                self.engine.set_live_external_tempo(
                    true,
                    clk.current_clock_bpm(),
                    TempoPointSource::MidiClock,
                );
            }
        }

        // OSC receiver — drain queued UDP messages and dispatch to subscribers.
        self.osc_receiver.update();

        // OSC parameter router — apply any pending param writes from OSC messages.
        if let Some(p) = self.engine.project_mut() {
            self.osc_param_router.apply(p);
        }

        // OSC timecode sync — process pending timecode, manage transport.
        {
            let snap = SyncTargetSnapshot::from_engine(&self.engine);
            self.osc_sync.update(
                now,
                &snap,
                &mut self.sync_arbiter,
                &mut self.engine,
                authority,
            );
        }
    }

    // ═══════════════════════════════════════════════════════════════
    // Phase 1 — External beat derivation
    // Port of C# PlaybackController.Update lines 1064-1096.
    // ═══════════════════════════════════════════════════════════════

    /// When playing with an external beat authority (Link or MidiClock),
    /// override the engine's beat from the sync controller's current position.
    fn derive_external_beat(&mut self, authority: ClockAuthority) {
        if self.engine.current_state() != PlaybackState::Playing {
            return;
        }

        match authority {
            ClockAuthority::Link => {
                if let Some(ref link) = self.transport_controller.link_sync
                    && link.is_link_enabled()
                        && link.has_active_peers()
                        && !self.link_beat_offset.is_nan()
                    {
                        self.engine
                            .set_beat((link.current_beat - self.link_beat_offset) as f32);
                    }
            }
            ClockAuthority::MidiClock => {
                if !self.sync_arbiter.manifold_owns_playback
                    && let Some(ref clk) = self.transport_controller.midi_clock_sync
                        && clk.is_midi_clock_enabled() && clk.is_receiving_clock() {
                            self.engine.set_beat(clk.current_clock_beat());
                        }
                        // else: beat derived from time (engine handles this in advance_time)
            }
            // ClockAuthority::Internal | Osc: beat derived from time (engine handles this)
            _ => {}
        }
    }

    /// Cache the offset between Link's absolute beat epoch and MANIFOLD's timeline beat 0.
    /// Called at Play() and Seek() sync points.
    /// Port of C# PlaybackController.CacheLinkBeatOffset lines 352-360.
    fn cache_link_beat_offset(&mut self) {
        if let Some(ref link) = self.transport_controller.link_sync {
            if link.is_link_enabled() {
                let manifold_beat =
                    self.engine.time_to_timeline_beat(self.engine.current_time()) as f64;
                self.link_beat_offset = link.current_beat - manifold_beat;
            } else {
                self.link_beat_offset = 0.0;
            }
        } else {
            self.link_beat_offset = 0.0;
        }
    }

    // ═══════════════════════════════════════════════════════════════
    // Phase 2 — Tempo recording / resolution
    // Port of C# PlaybackController.UpdateRecordingSessionState
    // and PlaybackController.ApplyResolvedTempo.
    // ═══════════════════════════════════════════════════════════════

    /// Arm/disarm the tempo recording session based on transport state.
    /// Port of C# PlaybackController.UpdateRecordingSessionState lines 1098.
    fn update_recording_session_state(&mut self, authority: ClockAuthority) {
        let should_record = self.engine.is_recording()
            && self.engine.current_state() == PlaybackState::Playing
            && authority != ClockAuthority::Osc;

        let default_bpm = self
            .engine
            .project()
            .map_or(120.0, |p| p.settings.bpm);

        // Capture live tempo source for the get_source_at_beat callback.
        let live_tempo = self.engine.try_get_live_external_tempo();
        let get_source_at_beat = |_beat: f32| -> TempoPointSource {
            if let Some((_, source)) = live_tempo {
                source
            } else {
                TempoPointSource::Unknown
            }
        };

        if let Some(project) = self.engine.project_mut() {
            self.tempo_recorder.update_session_state(
                should_record,
                &mut project.recording_provenance,
                &mut project.tempo_map,
                default_bpm,
                &get_source_at_beat,
            );
        }
    }

    /// Apply resolved external tempo to tempo map (recording) or global BPM (non-recording).
    /// Port of C# PlaybackController.ApplyResolvedTempo lines 1099.
    fn apply_resolved_tempo(&mut self, authority: ClockAuthority) {
        // Guard: no project → clear live tempo state.
        // Port of C# ApplyResolvedTempo lines 260-264.
        if self.engine.project().is_none() {
            self.engine
                .set_live_external_tempo(false, 0.0, TempoPointSource::Unknown);
            return;
        }

        let should_record = self.engine.is_recording()
            && self.engine.current_state() == PlaybackState::Playing;

        if !should_record {
            self.tempo_recorder.reset_tracking();
        }

        // TryResolveExternalTempo — already resolved by tick_sync_controllers()
        // and stored in engine via set_live_external_tempo().
        let (bpm, source) = match self.engine.try_get_live_external_tempo() {
            Some((b, s)) => (b.clamp(20.0, 300.0), s),
            None => {
                // No external tempo — nothing to apply.
                return;
            }
        };

        let current_beat = self.engine.current_beat();
        let current_time = self.engine.current_time();

        let mut tempo_map_changed = false;

        if let Some(project) = self.engine.project_mut()
            && authority != ClockAuthority::Osc {
                if should_record {
                    // Studio recording: append tempo automation points over time.
                    // Port of C# ApplyResolvedTempo lines 1117-1122.
                    tempo_map_changed = self.tempo_recorder.try_record_tempo_point(
                        &mut project.tempo_map,
                        current_beat,
                        current_time,
                        bpm,
                        source,
                    );
                    if tempo_map_changed {
                        self.tempo_recorder.append_tempo_change(
                            &mut project.recording_provenance,
                            current_time,
                            current_beat,
                            bpm,
                            source,
                        );
                    }
                } else if project.tempo_map.point_count() <= 1
                    && authority == ClockAuthority::Internal
                {
                    // No automation lane authored and no external position source:
                    // treat tempo as a global master value.
                    // Compare quantized values so raw float jitter doesn't trigger writes.
                    // Port of C# ApplyResolvedTempo lines 1127-1134.
                    //
                    // When MidiClock or Link is active, do NOT write to the tempo map —
                    // the project BPM is updated via sync_project_bpm_from_current_beat()
                    // for display only. Writing the tempo map causes beat re-derivation
                    // from stale time values, which makes the timeline stutter.
                    let map_bpm =
                        project.tempo_map.get_bpm_at_beat(0.0, project.settings.bpm);
                    let q_resolved_bpm = BeatQuantizer::quantize_bpm(bpm);
                    if (map_bpm - q_resolved_bpm).abs() >= TempoRecorder::BPM_THRESHOLD {
                        project.tempo_map.add_or_replace_point(
                            0.0, bpm, source, 0.001,
                        );
                        tempo_map_changed = true;
                    }
                }
            }

        if tempo_map_changed {
            // Re-derive beat from time after tempo map change.
            // Port of C# ApplyResolvedTempo line 1139.
            let new_beat = self.engine.time_to_timeline_beat(current_time);
            self.engine.set_beat(new_beat);
        }
    }

    /// End the tempo recording session if active (called from Pause/Stop).
    /// Port of C# PlaybackController.Pause/Stop → tempoRecorder.EndSessionIfActive.
    fn end_tempo_recording_session(&mut self) {
        if !self.tempo_recorder.is_session_active() {
            return;
        }

        let default_bpm = self
            .engine
            .project()
            .map_or(120.0, |p| p.settings.bpm);
        let live_tempo = self.engine.try_get_live_external_tempo();
        let get_source_at_beat = |_beat: f32| -> TempoPointSource {
            if let Some((_, source)) = live_tempo {
                source
            } else {
                TempoPointSource::Unknown
            }
        };

        if let Some(project) = self.engine.project_mut() {
            self.tempo_recorder.end_session_if_active(
                &mut project.recording_provenance,
                &mut project.tempo_map,
                default_bpm,
                &get_source_at_beat,
            );
        }
    }

    /// Handle a single command. Returns true if Shutdown.
    fn handle_command(&mut self, cmd: ContentCommand) -> bool {
        match cmd {
            ContentCommand::Shutdown => return true,

            // ── Transport ──────────────────────────────────────────
            ContentCommand::Play => {
                // Align transport to active external beat source BEFORE
                // the first sync pass. Port of C# PlaybackController.Play() lines 631-643.
                let authority = self.engine.project()
                    .map_or(ClockAuthority::Internal, |p| p.settings.clock_authority);
                if authority == ClockAuthority::MidiClock
                    && !self.sync_arbiter.manifold_owns_playback
                    && let Some(ref clk) = self.transport_controller.midi_clock_sync
                        && clk.is_midi_clock_enabled() {
                            let midi_beat = clk.current_clock_beat();
                            self.engine.set_beat(midi_beat);
                            let time = self.engine.beat_to_timeline_time(midi_beat);
                            self.engine.set_time(time.max(0.0) as f64);
                        }
                self.engine.play();
                self.cache_link_beat_offset();
            }
            ContentCommand::Pause => {
                // End tempo recording session on pause.
                // Port of C# PlaybackController.Pause → tempoRecorder.EndSessionIfActive.
                self.end_tempo_recording_session();
                self.engine.pause();
            }
            ContentCommand::Stop => {
                // End tempo recording session on stop.
                self.end_tempo_recording_session();
                self.engine.stop();
                self.link_beat_offset = f64::NAN;
            }
            ContentCommand::TogglePlayback => {
                if self.engine.is_playing() {
                    self.end_tempo_recording_session();
                    self.engine.pause();
                } else {
                    let authority = self.engine.project()
                        .map_or(ClockAuthority::Internal, |p| p.settings.clock_authority);
                    if authority == ClockAuthority::MidiClock
                        && !self.sync_arbiter.manifold_owns_playback
                        && let Some(ref clk) = self.transport_controller.midi_clock_sync
                            && clk.is_midi_clock_enabled() {
                                let midi_beat = clk.current_clock_beat();
                                self.engine.set_beat(midi_beat);
                                let time = self.engine.beat_to_timeline_time(midi_beat);
                                self.engine.set_time(time.max(0.0) as f64);
                            }
                    self.engine.play();
                    self.cache_link_beat_offset();
                }
            }
            ContentCommand::SeekTo(t) => {
                self.engine.seek_to(t);
                self.cache_link_beat_offset();
            }
            ContentCommand::SeekToBeat(beat) => {
                let time = self.engine.beat_to_timeline_time(beat);
                self.engine.seek_to(time);
                self.cache_link_beat_offset();
            }
            ContentCommand::SetRecording(rec) => {
                self.engine.set_recording(rec);
            }

            // ── Editing ────────────────────────────────────────────
            ContentCommand::Execute(cmd) => {
                if let Some(p) = self.engine.project_mut() {
                    self.editing_service.execute(cmd, p);
                }
                // Editing commands may add/remove clips — sync on next tick.
                self.engine.mark_sync_dirty();
                // Rebuild OSC routes — command may have added/removed layers or effects.
                if let Some(p) = self.engine.project() {
                    self.osc_param_router.rebuild(p, &mut self.osc_receiver);
                }
            }
            ContentCommand::ExecuteBatch(cmds, desc) => {
                if let Some(p) = self.engine.project_mut() {
                    self.editing_service.execute_batch(cmds, desc, p);
                }
                self.engine.mark_sync_dirty();
                if let Some(p) = self.engine.project() {
                    self.osc_param_router.rebuild(p, &mut self.osc_receiver);
                }
            }
            ContentCommand::Undo => {
                // Capture pre-undo settings so we can detect resolution/FPS changes.
                // Port of Unity WorkspaceController.OnUndoRedo() which calls
                // ApplyProjectResolutionFromFooter() + ApplyProjectFpsFromFooter().
                let pre = self.engine.project().map(|p| {
                    (p.settings.output_width, p.settings.output_height, p.settings.frame_rate)
                });
                if let Some(p) = self.engine.project_mut() {
                    let _ = self.editing_service.undo(p);
                }
                self.engine.mark_compositor_dirty(0.0);
                self.engine.mark_sync_dirty();
                // Apply resolution/FPS changes if the undo altered project settings.
                let post = self.engine.project().map(|p| {
                    (p.settings.output_width, p.settings.output_height, p.settings.frame_rate)
                });
                if let (Some((pre_w, pre_h, pre_fps)), Some((post_w, post_h, post_fps))) = (pre, post) {
                    if post_w != pre_w || post_h != pre_h {
                        self.content_pipeline.resize(
                            &mut self.engine,
                            post_w as u32, post_h as u32,
                        );
                    }
                    if (post_fps - pre_fps).abs() > 0.01 {
                        self.timer.set_target_fps(post_fps as f64);
                    }
                }
                if let Some(p) = self.engine.project() {
                    self.osc_param_router.rebuild(p, &mut self.osc_receiver);
                }
            }
            ContentCommand::Redo => {
                // Same pre/post settings detection as Undo.
                let pre = self.engine.project().map(|p| {
                    (p.settings.output_width, p.settings.output_height, p.settings.frame_rate)
                });
                if let Some(p) = self.engine.project_mut() {
                    let _ = self.editing_service.redo(p);
                }
                self.engine.mark_compositor_dirty(0.0);
                self.engine.mark_sync_dirty();
                // Apply resolution/FPS changes if the redo altered project settings.
                let post = self.engine.project().map(|p| {
                    (p.settings.output_width, p.settings.output_height, p.settings.frame_rate)
                });
                if let (Some((pre_w, pre_h, pre_fps)), Some((post_w, post_h, post_fps))) = (pre, post) {
                    if post_w != pre_w || post_h != pre_h {
                        self.content_pipeline.resize(
                            &mut self.engine,
                            post_w as u32, post_h as u32,
                        );
                    }
                    if (post_fps - pre_fps).abs() > 0.01 {
                        self.timer.set_target_fps(post_fps as f64);
                    }
                }
                if let Some(p) = self.engine.project() {
                    self.osc_param_router.rebuild(p, &mut self.osc_receiver);
                }
            }
            ContentCommand::SetProject => {
                self.editing_service.set_project();
            }
            ContentCommand::MarkClean => {
                self.editing_service.mark_clean();
            }

            // ── Project lifecycle ──────────────────────────────────
            ContentCommand::LoadProject(project) => {
                if let Some(ref mut audio) = self.audio_sync {
                    audio.reset_audio();
                }
                if let Some(ref mut stem) = self.stem_audio {
                    stem.reset_stems(self.audio_sync.as_mut());
                }
                // Reset link beat offset and tempo recorder on project load.
                // Port of C# PlaybackController.OnProjectLoading lines 550-551.
                self.link_beat_offset = f64::NAN;
                self.tempo_recorder.reset();
                self.engine.initialize(*project);
                // Resize content pipeline to project dims
                if let Some(p) = self.engine.project() {
                    let w = p.settings.output_width.max(1) as u32;
                    let h = p.settings.output_height.max(1) as u32;
                    self.content_pipeline.resize(&mut self.engine, w, h);
                }
                // Sync frame timer to loaded project's frame rate.
                if let Some(p) = self.engine.project() {
                    self.timer.set_target_fps(p.settings.frame_rate as f64);
                }
                // Update MIDI mapping config from the newly loaded project.
                // Port of C# PlaybackController.OnProjectLoaded → midiInputController.SetMidiConfig().
                if let Some(p) = self.engine.project() {
                    self.midi_input.set_midi_config(p.midi_config.clone());
                }
                // Rebuild OSC parameter routes for the loaded project.
                // Port of C# WorkspaceController: creates/registers OSC bridges on project load.
                if let Some(p) = self.engine.project() {
                    self.osc_param_router.rebuild(p, &mut self.osc_receiver);
                }
            }
            // ── Settings ───────────────────────────────────────────
            ContentCommand::SetBpm(bpm) => {
                if let Some(p) = self.engine.project_mut() {
                    p.settings.bpm = bpm as f32;
                }
            }
            ContentCommand::SetFrameRate(fps) => {
                if let Some(p) = self.engine.project_mut() {
                    p.settings.frame_rate = fps as f32;
                }
                self.timer.set_target_fps(fps);
            }

            // ── GPU ────────────────────────────────────────────────
            ContentCommand::ResizeContent(w, h) => {
                self.content_pipeline.resize(&mut self.engine, w, h);
            }

            // ── Transport/sync ─────────────────────────────────────
            ContentCommand::CycleClockAuthority => {
                // No longer used — authority is auto-determined from enabled sources.
                // Kept for backwards compatibility with any pending commands.
            }
            ContentCommand::ToggleLink => {
                self.transport_controller.toggle_link(&mut self.engine);
            }
            ContentCommand::ToggleMidiClock => {
                self.transport_controller.toggle_midi_clock(&mut self.engine);
            }
            ContentCommand::ToggleSyncOutput => {
                self.transport_controller.toggle_sync_output(&mut self.engine);
            }
            ContentCommand::SetMidiClockDevice(index) => {
                if let Some(ref mut clk) = self.transport_controller.midi_clock_sync {
                    clk.change_source(index);
                    log::info!("[ContentThread] MIDI clock device changed to index {}", index);
                }
            }
            ContentCommand::ResetBpm => {
                TransportController::reset_bpm(
                    &mut self.engine, &mut self.editing_service,
                );
            }

            // ── Audio ──────────────────────────────────────────────
            ContentCommand::AudioLoaded { preloaded, waveform: _ } => {
                if let Some(ref mut audio_sync) = self.audio_sync
                    && let Err(e) = audio_sync.apply_preloaded(*preloaded) {
                        log::warn!("[ContentThread] Failed to apply loaded audio: {}", e);
                    }
            }
            ContentCommand::ResetAudio => {
                if let Some(ref mut audio_sync) = self.audio_sync {
                    audio_sync.reset_audio();
                }
            }

            // ── Stem audio ────────────────────────────────────────
            ContentCommand::StemAudioLoaded(preloaded) => {
                if let Some(ref mut stem) = self.stem_audio {
                    stem.apply_preloaded_stems(*preloaded);
                }
            }
            ContentCommand::StemSetExpanded(expand) => {
                if let Some(ref mut stem) = self.stem_audio {
                    // Auto-load stems on first expand if paths available but not yet loaded.
                    // Port of Unity WorkspaceController.EnsureStemAudioController lazy init.
                    if expand && !stem.stems_ready()
                        && let Some(stem_paths_vec) = self.engine.project()
                            .and_then(|p| p.percussion_import.as_ref())
                            .and_then(|perc| perc.stem_paths.as_ref())
                        {
                            let mut paths: [Option<String>; manifold_playback::stem_audio::STEM_COUNT] = Default::default();
                            for (i, p) in stem_paths_vec.iter().enumerate() {
                                if i < manifold_playback::stem_audio::STEM_COUNT {
                                    paths[i] = Some(p.clone());
                                }
                            }
                            stem.load_stems(&paths);
                        }
                    stem.set_expanded(expand, self.audio_sync.as_mut());
                }
            }
            ContentCommand::StemToggleMute(index) => {
                if let Some(ref mut stem) = self.stem_audio {
                    stem.toggle_muted(index);
                }
            }
            ContentCommand::StemToggleSolo(index) => {
                if let Some(ref mut stem) = self.stem_audio {
                    stem.toggle_soloed(index);
                }
            }
            ContentCommand::StemReset => {
                if let Some(ref mut stem) = self.stem_audio {
                    stem.reset_stems(self.audio_sync.as_mut());
                }
            }

            // ── Direct project mutation ────────────────────────────
            ContentCommand::MutateProject(f) => {
                if let Some(p) = self.engine.project_mut() {
                    f(p);
                }
                // Re-notify renderers so caches (e.g. VideoRenderer's VideoLibrary)
                // stay in sync with the mutated project.
                if let Some(p) = self.engine.project() {
                    let project_clone = p.clone();
                    for renderer in self.engine.renderers_mut() {
                        renderer.on_project_loaded(&project_clone);
                    }
                }
            }

            // ── Save support ───────────────────────────────────────
            ContentCommand::RequestProjectSnapshot(tx) => {
                if let Some(p) = self.engine.project() {
                    let _ = tx.send(p.clone());
                }
            }

            // ── Clipboard ─────────────────────────────────────────
            ContentCommand::CopyClips { clip_ids, region } => {
                if let Some(p) = self.engine.project() {
                    let spb = 60.0 / p.settings.bpm.max(1.0);
                    self.editing_service.copy_clips(p, &clip_ids, region.as_ref(), spb);
                }
            }
            ContentCommand::PasteClips { target_beat, target_layer, result_tx } => {
                if let Some(p) = self.engine.project_mut() {
                    let spb = 60.0 / p.settings.bpm.max(1.0);
                    let result = self.editing_service.paste_clips(p, target_beat, target_layer, spb);
                    if !result.commands.is_empty() {
                        self.editing_service.execute_batch(result.commands, "Paste clips".into(), p);
                    }
                    let _ = result_tx.send(result.pasted_clip_ids);
                } else {
                    let _ = result_tx.send(Vec::new());
                }
            }

            // ── Percussion ────────────────────────────────────────
            ContentCommand::PercussionImport(path) => {
                let beat = self.engine.current_beat();
                let beats_per_bar = self.engine.project()
                    .map_or(4, |p| p.settings.time_signature_numerator.max(1));
                if let Some(p) = self.engine.project_mut() {
                    self.percussion_orchestrator.on_import_percussion_map(
                        Some(path),
                        p,
                        &mut self.editing_service,
                        beat,
                        beats_per_bar,
                    );
                }
            }
            ContentCommand::ReAnalyzeTriggers(instrument_group) => {
                if let Some(p) = self.engine.project_mut() {
                    self.percussion_orchestrator.on_re_analyze_triggers(
                        &instrument_group,
                        p,
                    );
                }
            }
            ContentCommand::ReImportStems => {
                if let Some(p) = self.engine.project_mut() {
                    self.percussion_orchestrator.on_re_import_stems(p);
                }
            }
            ContentCommand::PercussionCalibrateDownbeat { playhead_beat, beats_per_bar } => {
                if let Some(p) = self.engine.project_mut() {
                    self.percussion_orchestrator
                        .calibrate_imported_percussion_downbeat_at_playhead(
                            p, &mut self.editing_service,
                            playhead_beat, beats_per_bar, true,
                        );
                }
            }
            ContentCommand::PercussionNudgeAlignment(delta_beats) => {
                if let Some(p) = self.engine.project_mut() {
                    self.percussion_orchestrator
                        .nudge_imported_percussion_alignment(
                            delta_beats, p, &mut self.editing_service, true,
                        );
                }
            }
            ContentCommand::PercussionResetAlignment => {
                if let Some(p) = self.engine.project_mut() {
                    self.percussion_orchestrator
                        .reset_imported_percussion_alignment(
                            p, &mut self.editing_service, true,
                        );
                }
            }

            // ── Compositor ─────────────────────────────────────────
            ContentCommand::MarkCompositorDirty => {
                self.engine.mark_compositor_dirty(0.0);
            }

            // ── Display ───────────────────────────────────────────
            ContentCommand::UpdateEdrHeadroom(headroom) => {
                eprintln!(
                    "[EDR] Content thread: headroom updated to {:.2}x (mode={})",
                    headroom,
                    if headroom > 1.0 { "passthrough" } else { "ACES tonemap" },
                );
                self.content_pipeline.edr_headroom = headroom;
                self.engine.mark_compositor_dirty(0.0);
            }

            // ── Generator ─────────────────────────────────────────
            ContentCommand::GeneratorTypeChanged { layer_id, new_type } => {
                // Port of C# PlaybackController.NotifyGeneratorTypeChanged().
                let (renderers, _) = self.engine.split_renderer_project();
                for renderer in renderers.iter_mut() {
                    if let Some(gen_renderer) = renderer.as_any_mut()
                        .downcast_mut::<manifold_renderer::generator_renderer::GeneratorRenderer>()
                    {
                        gen_renderer.update_active_types_for_layer(&layer_id, new_type);
                        break;
                    }
                }
                self.engine.mark_compositor_dirty(0.0);
            }

            // ── LED output ─────────────────────────────────────────────
            ContentCommand::InitLedOutput(settings) => {
                let mut ctrl = manifold_led::LedOutputController::new();
                let native_device = self.content_pipeline.native_device()
                    .expect("native device required for LED init");
                if ctrl.initialize(native_device, &settings) {
                    self.led_controller = Some(ctrl);
                    log::info!("[ContentThread] LED output initialized.");
                } else {
                    log::warn!("[ContentThread] LED output failed to initialize.");
                }
            }
            ContentCommand::ShutdownLedOutput => {
                if let Some(ref mut ctrl) = self.led_controller {
                    ctrl.shutdown();
                }
                self.led_controller = None;
                log::info!("[ContentThread] LED output shut down.");
            }
            ContentCommand::SetLedEnabled(enabled) => {
                if let Some(ref mut ctrl) = self.led_controller {
                    ctrl.set_enabled(enabled);
                }
            }

            // ── Export ────────────────────────────────────────────────
            ContentCommand::StartExport(_) => {
                // Handled in run() loop directly (needs cmd_rx/state_tx access).
            }
            ContentCommand::CancelExport => {
                // No-op outside of export loop — cancel flag checked inside run_export.
            }

            // ── Lifecycle ────────────────────────────────────────────
            ContentCommand::PauseRendering => {
                self.rendering_paused = true;
                log::info!("[ContentThread] rendering paused (dialog open)");
            }
            ContentCommand::ResumeRendering => {
                self.rendering_paused = false;
                log::info!("[ContentThread] rendering resumed");
            }

            // ── Profiling ────────────────────────────────────────────
            #[cfg(feature = "profiling")]
            ContentCommand::StartProfiling {
                project_name, project_path, resolution, target_fps, gpu_name,
            } => {
                log::info!("[ContentThread] profiling session started");
                let mut session = manifold_profiler::ProfileSession::new(
                    project_name, project_path, resolution, target_fps, gpu_name,
                );

                // Build timeline snapshot from current project state
                if let Some(p) = self.engine.project() {
                    let layers = p.timeline.layers.iter().map(|layer| {
                        let clips = layer.clips.iter()
                            .map(|c| manifold_profiler::ClipSnapshot {
                                id: c.id.to_string(),
                                start_beat: c.start_beat,
                                duration_beats: c.duration_beats,
                                generator_type: c.generator_type.to_string(),
                                effect_count: c.effects.len(),
                            })
                            .collect();
                        let effects = layer.effects.as_deref().unwrap_or(&[]).iter()
                            .map(|fx| manifold_profiler::EffectSnapshot {
                                effect_type: fx.effect_type().to_string(),
                                enabled: fx.enabled,
                            })
                            .collect();
                        manifold_profiler::LayerSnapshot {
                            index: layer.index,
                            generator_type: layer.gen_params()
                                .map_or("None".to_string(), |gp| gp.generator_type().to_string()),
                            blend_mode: format!("{:?}", layer.default_blend_mode),
                            is_muted: layer.is_muted,
                            clips,
                            effects,
                        }
                    }).collect();
                    let master_effects = p.settings.master_effects.iter()
                        .map(|fx| manifold_profiler::EffectSnapshot {
                            effect_type: fx.effect_type().to_string(),
                            enabled: fx.enabled,
                        })
                        .collect();
                    session.set_timeline_snapshot(manifold_profiler::TimelineSnapshot {
                        bpm: p.settings.bpm,
                        time_signature: p.settings.time_signature_numerator,
                        resolution: (
                            p.settings.output_width as u32,
                            p.settings.output_height as u32,
                        ),
                        layers,
                        master_effects,
                    });
                }

                self.profiler = Some(session);
            }
            #[cfg(feature = "profiling")]
            ContentCommand::StopProfiling => {
                if let Some(ref mut profiler) = self.profiler {
                    match profiler.stop_and_dump() {
                        Ok(path) => {
                            log::info!(
                                "[ContentThread] profiling session saved: {} ({} frames)",
                                path.display(),
                                profiler.frame_count(),
                            );
                        }
                        Err(e) => {
                            log::error!("[ContentThread] profiling dump failed: {}", e);
                        }
                    }
                }
                self.profiler = None;
            }
        }
        false
    }

    /// Run the offline video export loop.
    ///
    /// Temporarily replaces the normal content loop: ticks the engine with fixed
    /// delta, renders each frame, and encodes via the native Metal encoder at
    /// maximum GPU speed (no frame pacing / sleep).
    ///
    /// Port of Unity VideoExporter.ExportCoroutine() (offline / generator-only path).
    #[cfg(target_os = "macos")]
    fn run_export(
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
            export_config.audio_start_beat = audio_sync.start_beat();
            export_config.audio_encoder_delay = audio_sync.encoder_delay_seconds();
        }

        // Calculate timing
        let mut tempo_map = project.tempo_map.clone();
        let start_seconds =
            TempoMapConverter::beat_to_seconds(&mut tempo_map, start_beat, bpm);
        let end_seconds =
            TempoMapConverter::beat_to_seconds(&mut tempo_map, end_beat, bpm);
        let duration = end_seconds - start_seconds;
        let total_frames = (duration * export_config.fps).round() as u32;
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
            layer.is_group() || layer.clips.iter().all(|c| c.is_generator())
        });
        let mode_label = if generator_only { "offline" } else { "real-time" };

        eprintln!(
            "[Export] START ({mode_label}): {} frames, {:.2}s, \
             beats {:.1}-{:.1}, {}x{} @ {} fps",
            total_frames, duration, start_beat, end_beat,
            export_config.width, export_config.height, export_config.fps,
        );

        // 3. Enter export mode
        self.engine.stop();
        self.engine.set_export_mode(true);
        // Ensure content pipeline matches export resolution
        let (cur_w, cur_h) = self.content_pipeline.dimensions();
        if cur_w != export_config.width || cur_h != export_config.height {
            self.content_pipeline.resize(
                &mut self.engine,
                export_config.width,
                export_config.height,
            );
        }
        // Seek to start
        let start_time = self.engine.beat_to_timeline_time(start_beat);
        self.engine.seek_to(start_time);
        self.engine.play();

        // 4. Create export session (initializes native Metal encoder).
        //    Share the content pipeline's Metal device to avoid cross-device GPU sync.
        let device_ptr = self.content_pipeline.native_device_ptr();
        let session_result = if let Some(ptr) = device_ptr {
            unsafe {
                manifold_media::export_session::ExportSession::new_with_device(
                    export_config.clone(), bpm, &mut tempo_map, ptr,
                )
            }
        } else {
            manifold_media::export_session::ExportSession::new(
                export_config.clone(), bpm, &mut tempo_map,
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
                    dt_seconds: frame_dt,
                    realtime_now: 0.0,
                    pre_render_dt: frame_dt as f32,
                    frame_count: -1,
                    export_fixed_dt: frame_dt,
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
            let start_time = self.engine.beat_to_timeline_time(start_beat);
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
        // Restore content pipeline resolution if it was changed for export
        if cur_w != export_config.width || cur_h != export_config.height {
            self.content_pipeline.resize(&mut self.engine, cur_w, cur_h);
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
        let frame_start = std::time::Instant::now();

        let ctx = TickContext {
            dt_seconds: frame_dt,
            realtime_now: frame_idx as f64 * frame_dt,
            pre_render_dt: frame_dt as f32,
            frame_count: frame_idx as i32,
            export_fixed_dt: frame_dt,
        };
        let tick_result = self.engine.tick(ctx);

        // Wait for any in-flight video decodes to complete before rendering.
        // At GPU speed the export outruns the async decoder — without this,
        // the same stale video frame gets encoded for dozens of frames.
        // Skipped for generator-only projects (no video decoders).
        let decode_start = std::time::Instant::now();
        if !generator_only {
            self.engine.flush_pending_decodes();
        }
        let decode_ms = decode_start.elapsed().as_secs_f64() * 1000.0;

        self.content_pipeline.render_content(
            &self.gpu, &mut self.engine, &tick_result, frame_dt, frame_idx as u64,
            true,
        );

        // Diagnostic logging: first 5 frames, then every 30 frames.
        // Uses eprintln! to bypass log level filtering.
        if frame_idx < 5 || frame_idx.is_multiple_of(30) {
            let beat = self.engine.current_beat();
            let time = self.engine.current_time();
            eprintln!(
                "[Export] frame={} beat={:.2} time={:.3}s clips={} dirty={} \
                 decode_wait={:.1}ms",
                frame_idx, beat, time,
                tick_result.ready_clips.len(),
                tick_result.compositor_dirty,
                decode_ms,
            );
        }

        let render_ms = frame_start.elapsed().as_secs_f64() * 1000.0;

        let tex_ptr = if export_config.hdr {
            let paper_white = 200.0f32;
            let max_nits = 10000.0f32;
            let texture = self.content_pipeline.pq_encode_for_export(
                paper_white, max_nits,
            );
            unsafe { Self::get_metal_texture_ptr(texture) }
        } else {
            let texture = self.content_pipeline.export_output_texture();
            unsafe { Self::get_metal_texture_ptr(texture) }
        };

        self.content_pipeline.wait_for_render_complete();
        let gpu_wait_ms = frame_start.elapsed().as_secs_f64() * 1000.0 - render_ms;

        // Log GPU wait time — if it stays near zero, the GPU is finishing
        // before we even check. If it grows, we're GPU-bound.
        if frame_idx < 5 || frame_idx.is_multiple_of(30) {
            eprintln!(
                "[Export] frame={} render={:.1}ms gpu_wait={:.1}ms tex={:?}",
                frame_idx, render_ms, gpu_wait_ms,
                tex_ptr.map(|p| p as usize),
            );
        }

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

        // Per-frame timing for first few frames (diagnose slow starts).
        if frame_idx < 5 {
            let total_ms = frame_start.elapsed().as_secs_f64() * 1000.0;
            eprintln!(
                "[Export] frame={} total={:.1}ms",
                frame_idx, total_ms,
            );
        }

        None
    }

    /// Extract the raw Metal texture pointer from a native GpuTexture.
    /// Returns `id<MTLTexture>` as `*mut c_void` for the native encoder.
    #[cfg(target_os = "macos")]
    unsafe fn get_metal_texture_ptr(texture: &manifold_gpu::GpuTexture) -> Option<*mut std::ffi::c_void> {
        use objc::runtime::Object;
        use std::ffi::c_void;
        // metal::TextureRef is an objc object — cast to raw pointer.
        let raw_ref: &metal::TextureRef = texture.raw();
        let ptr = raw_ref as *const metal::TextureRef as *const Object as *mut c_void;
        Some(ptr)
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
    fn send_export_finished(
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
