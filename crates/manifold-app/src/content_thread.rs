//! Content thread — runs PlaybackEngine, EditingService, and ContentPipeline
//! on a dedicated thread. Communicates with the UI thread via crossbeam channels.
//!
//! The content thread owns all authoritative state: the engine (which owns the
//! project), the editing service (undo/redo), audio sync, percussion, and the
//! GPU content pipeline (generators + compositor).
use std::sync::Arc;
use crossbeam_channel::{Receiver, Sender};

use manifold_core::math::BeatQuantizer;
use manifold_core::types::{ClockAuthority, OscSyncMode, PlaybackState, TempoPointSource};
use manifold_core::{Beats, Bpm, Seconds};
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
use crate::content_state::ContentState;
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
    pub time_since_start: Seconds,
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
    /// Ableton Live OSC bridge — discovers session, pushes macro values.
    pub ableton_bridge: manifold_playback::ableton_bridge::AbletonBridge,
    /// Set to true when Ableton wrote param values last frame, so the state-push
    /// in the FOLLOWING frame forces a modulation snapshot (bridge apply runs after
    /// the state push in the same frame, so we need a 1-frame sticky signal).
    pub ableton_active_last_frame: bool,

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
    pub last_midi_device_scan_time: Seconds,

    // ── Cached project snapshot (Arc avoids deep clone every modulation frame) ──
    pub cached_project_snapshot: Option<std::sync::Arc<manifold_core::project::Project>>,

    // ── Reusable modulation scratch (flat buffer — zero alloc after first frame) ──
    pub mod_scratch: crate::content_state::ModulationSnapshot,

    // ── Cached ContentState strings (Arc<str> — clone = refcount bump, zero alloc) ──
    pub cached_midi_clock_position: Arc<str>,
    pub cached_midi_clock_device: Arc<str>,
    pub cached_osc_timecode: Arc<str>,
    pub cached_perc_message: Arc<str>,
    /// Last-sent MIDI device names — only reallocated when the list changes.
    pub last_sent_midi_device_names: Arc<[String]>,

    // ── Profiling ──
    /// Active profiling session (only present when feature = "profiling").
    #[cfg(feature = "profiling")]
    pub profiler: Option<manifold_profiler::ProfileSession>,
}

impl ContentThread {
    /// Run the content loop. Blocks until Shutdown is received.
    pub fn run(
        mut self,
        cmd_tx: crossbeam_channel::Sender<ContentCommand>,
        cmd_rx: Receiver<ContentCommand>,
        state_tx: Sender<ContentState>,
    ) {
        log::info!("[ContentThread] started");

        // Set content thread to real-time scheduling via THREAD_TIME_CONSTRAINT_POLICY.
        // This is the native macOS real-time API (used by CoreAudio, game engines).
        // Tells the kernel: "I'm a periodic real-time workload with a specific
        // deadline." The scheduler reserves time slots and mach_wait_until wakes
        // with sub-microsecond precision.
        //
        // SCHED_RR (POSIX) was used previously but macOS doesn't honor it for
        // real-time — it falls back to normal scheduling with 1-2ms jitter.
        #[cfg(target_os = "macos")]
        {
            #[repr(C)]
            struct ThreadTimeConstraintPolicy {
                period: u32,
                computation: u32,
                constraint: u32,
                preemptible: i32,
            }

            unsafe extern "C" {
                fn thread_policy_set(
                    thread: u32,
                    flavor: u32,
                    policy_info: *const ThreadTimeConstraintPolicy,
                    count: u32,
                ) -> i32;
                fn pthread_mach_thread_np(thread: libc::pthread_t) -> u32;
            }

            // THREAD_TIME_CONSTRAINT_POLICY = 2
            const THREAD_TIME_CONSTRAINT_POLICY: u32 = 2;
            // Count = struct size in natural_t (u32) units
            const POLICY_COUNT: u32 =
                (std::mem::size_of::<ThreadTimeConstraintPolicy>() / std::mem::size_of::<u32>())
                    as u32;

            // Convert frame timing to Mach absolute time units.
            // On Apple Silicon: timebase 1:1, so 1 tick = 1 nanosecond.
            let frame_ns = (1_000_000_000.0 / self.timer.target_fps()) as u32;
            // Computation budget: allow up to 75% of the frame for render work.
            // The remaining 25% is headroom for the scheduler.
            let computation_ns = (frame_ns as f64 * 0.75) as u32;

            let policy = ThreadTimeConstraintPolicy {
                period: frame_ns,           // 16.67ms at 60fps
                computation: computation_ns, // 12.5ms max render time
                constraint: frame_ns,        // must complete within one period
                preemptible: 1,              // can be preempted during computation
            };

            let mach_thread = unsafe {
                pthread_mach_thread_np(libc::pthread_self())
            };
            let ret = unsafe {
                thread_policy_set(
                    mach_thread,
                    THREAD_TIME_CONSTRAINT_POLICY,
                    &policy,
                    POLICY_COUNT,
                )
            };

            if ret == 0 {
                log::info!(
                    "[ContentThread] Real-time thread policy set \
                     (THREAD_TIME_CONSTRAINT: period={:.2}ms, \
                     computation={:.2}ms)",
                    frame_ns as f64 / 1_000_000.0,
                    computation_ns as f64 / 1_000_000.0,
                );
            } else {
                log::warn!(
                    "[ContentThread] THREAD_TIME_CONSTRAINT failed (err={}), \
                     falling back to QOS_CLASS_USER_INTERACTIVE",
                    ret,
                );
                unsafe extern "C" {
                    fn pthread_set_qos_class_self_np(
                        qos_class: u32,
                        relative_priority: i32,
                    ) -> i32;
                }
                let qos_ret =
                    unsafe { pthread_set_qos_class_self_np(0x21, 0) };
                if qos_ret != 0 {
                    log::warn!(
                        "[ContentThread] QoS fallback also failed (err={})",
                        qos_ret,
                    );
                } else {
                    log::info!(
                        "[ContentThread] QoS set to USER_INTERACTIVE (fallback)"
                    );
                }
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

        // LED output is NOT auto-initialized. The user enables it via the
        // master-inspector toggle, which sends InitLedOutput / ShutdownLedOutput.

        loop {
            // 1. Drain ALL pending commands, coalescing consecutive seeks.
            // During scrubbing the UI sends a SeekTo/SeekToBeat per mouse-move
            // event — at high polling rates this floods the channel. Only the
            // final seek in a burst matters, so we defer it and overwrite.
            let mut pending_seek: Option<ContentCommand> = None;
            loop {
                match cmd_rx.try_recv() {
                    Ok(ContentCommand::StartExport(config)) => {
                        // Stop any active live recording before entering export.
                        #[cfg(target_os = "macos")]
                        if let Some(session) =
                            self.content_pipeline.recording_session.take()
                        {
                            log::warn!(
                                "[ContentThread] Stopping active recording \
                                 before export"
                            );
                            let _ = session.stop();
                        }
                        // Flush any pending seek before entering export.
                        if let Some(seek) = pending_seek.take() {
                            let _ = self.handle_command(seek);
                        }
                        self.run_export(*config, &cmd_rx, &state_tx);
                    }
                    Ok(cmd @ ContentCommand::SeekTo(_))
                    | Ok(cmd @ ContentCommand::SeekToBeat(_)) => {
                        // Coalesce: overwrite previous pending seek.
                        pending_seek = Some(cmd);
                    }
                    // SurfaceReady is a no-op GPU event — don't flush pending
                    // seeks, as that would break coalescing during scrubbing.
                    #[cfg(target_os = "macos")]
                    Ok(ContentCommand::SurfaceReady) => {}
                    Ok(cmd) => {
                        // Flush any pending seek before a non-seek command
                        // to preserve ordering (e.g. Seek then Play).
                        if let Some(seek) = pending_seek.take()
                            && self.handle_command(seek)
                        {
                            log::info!("[ContentThread] shutdown received");
                            return;
                        }
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
            // Apply the final coalesced seek (if any).
            if let Some(seek) = pending_seek.take()
                && self.handle_command(seek)
            {
                log::info!("[ContentThread] shutdown received");
                return;
            }

            // 1b. Wait for GPU surface, draining commands while waiting.
            // In the common case (99%+) this returns immediately — the GPU
            // finished the surface from 2 frames ago long before now.
            // Under heavy GPU load, keeps processing transport/MIDI/parameter
            // commands instead of busy-spinning. Zero CPU during the wait.
            #[cfg(target_os = "macos")]
            {
                let fence_start = std::time::Instant::now();
                if self.wait_for_surface_draining_commands(&cmd_tx, &cmd_rx) {
                    log::info!(
                        "[ContentThread] shutdown received during surface wait"
                    );
                    return;
                }
                self.content_pipeline.set_last_fence_wait_ms(
                    fence_start.elapsed().as_secs_f64() * 1000.0,
                );
            }

            // 2. Wait for next content frame (skip tick+render when paused)
            if self.rendering_paused {
                std::thread::sleep(std::time::Duration::from_millis(16));
                continue;
            }

            // Precision frame pacing: block until the next frame deadline.
            // mach_wait_until for the bulk, spin for the final 2ms.
            self.timer.wait_for_deadline();
            // Drain autoreleased ObjC Metal objects at the end of each frame,
            // preventing memory accumulation and random GC-like pauses.
            #[cfg(target_os = "macos")]
            objc2::rc::autoreleasepool(|_| {
                self.tick_frame(&state_tx);
            });
            #[cfg(not(target_os = "macos"))]
            self.tick_frame(&state_tx);
        }
    }

    /// Wait for the GPU to finish with the surface we're about to render to,
    /// while continuing to drain and process commands.
    ///
    /// In the common case (GPU finished 2 frames ago), this returns immediately.
    /// Under heavy GPU load, this keeps transport, MIDI, and parameter processing
    /// alive instead of busy-spinning — zero CPU while waiting.
    ///
    /// Returns `true` if a shutdown command was received during the wait.
    #[cfg(target_os = "macos")]
    fn wait_for_surface_draining_commands(
        &mut self,
        cmd_tx: &crossbeam_channel::Sender<ContentCommand>,
        cmd_rx: &crossbeam_channel::Receiver<ContentCommand>,
    ) -> bool {
        // Fast path: surface already ready (99%+ of frames).
        if self.content_pipeline.is_surface_ready() {
            return false;
        }

        // Slow path: GPU is behind. Register notification — when the GPU
        // signals, SurfaceReady is sent through cmd_tx, waking recv().
        if !self.content_pipeline.register_surface_notify(cmd_tx) {
            return false; // became ready between check and register
        }

        log::debug!("[ContentThread] GPU behind — waiting with command drain");
        let deadline =
            std::time::Instant::now() + std::time::Duration::from_secs(5);

        // Coalesce seeks during the wait, matching the main drain loop's
        // behavior. Without this, scrubbing during a GPU stall would
        // execute every intermediate seek position individually.
        let mut pending_seek: Option<ContentCommand> = None;

        loop {
            // Check if GPU finished.
            if self.content_pipeline.is_surface_ready() {
                break;
            }

            // Check timeout.
            let remaining =
                deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                self.content_pipeline.handle_surface_timeout();
                break;
            }

            // Block until either a command arrives (UI or SurfaceReady from
            // GPU notification) or the 5-second deadline expires.
            // Zero CPU — thread sleeps in the kernel until woken.
            match cmd_rx.recv_timeout(remaining) {
                Ok(ContentCommand::SurfaceReady) => {
                    // GPU wake signal — loop back to check is_surface_ready().
                }
                Ok(cmd @ ContentCommand::SeekTo(_))
                | Ok(cmd @ ContentCommand::SeekToBeat(_)) => {
                    pending_seek = Some(cmd);
                }
                Ok(cmd) => {
                    // Flush pending seek before non-seek command.
                    if let Some(seek) = pending_seek.take()
                        && self.handle_command(seek)
                    {
                        return true;
                    }
                    if self.handle_command(cmd) {
                        return true; // shutdown
                    }
                    // Drain any additional queued commands.
                    while let Ok(cmd) = cmd_rx.try_recv() {
                        match cmd {
                            ContentCommand::SurfaceReady => {}
                            cmd @ ContentCommand::SeekTo(_)
                            | cmd @ ContentCommand::SeekToBeat(_) => {
                                pending_seek = Some(cmd);
                            }
                            cmd => {
                                if let Some(seek) = pending_seek.take()
                                    && self.handle_command(seek)
                                {
                                    return true;
                                }
                                if self.handle_command(cmd) {
                                    return true;
                                }
                            }
                        }
                    }
                }
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                    // 5-second deadline expired — GPU hung.
                    self.content_pipeline.handle_surface_timeout();
                    break;
                }
                Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                    return true; // shutdown
                }
            }
        }
        // Apply final coalesced seek.
        if let Some(seek) = pending_seek
            && self.handle_command(seek)
        {
            return true;
        }
        false
    }

    /// Execute one content frame: tick engine, render, send state to UI.
    /// Separated from the main loop to allow wrapping in autoreleasepool on macOS.
    fn tick_frame(&mut self, state_tx: &Sender<ContentState>) {
            let dt = self.timer.consume_tick();
            let realtime = self.timer.realtime_since_start();
            self.time_since_start = Seconds(realtime);

            // Refresh MIDI device list every ~2 seconds
            if (self.time_since_start - self.last_midi_device_scan_time).0 >= 2.0 {
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
                dt_seconds: Seconds(dt),
                realtime_now: Seconds(realtime),
                pre_render_dt: Seconds(dt),
                frame_count: self.frame_count,
                export_fixed_dt: Seconds::ZERO,
            };
            let tick_result = self.engine.tick(ctx);

            // 4b. Transport output (LateUpdate equivalent — after engine tick).
            // In M4L mode: OscPositionSender sends /manifold/* to M4L device.
            // In AbletonOSC mode: AbletonBridge sends /live/song/* to AbletonOSC.
            // Outbound transport: AbletonOSC mode uses bridge, M4L mode uses
            // OscPositionSender. Both use the same fire-and-forget pattern.
            let osc_sync_mode = self.engine.project()
                .map_or(OscSyncMode::M4L, |p| p.settings.osc_sync_mode);
            if osc_sync_mode == OscSyncMode::AbletonOsc
                && self.ableton_bridge.is_transport_enabled()
            {
                let bpm = self.engine.project()
                    .map_or(120.0_f32, |p| p.settings.bpm.0);
                let seconds_per_beat = if bpm > 0.0 { 60.0 / bpm } else { 0.5 };
                self.ableton_bridge.late_update_transport(
                    self.engine.is_playing(),
                    self.engine.current_beat().as_f32(),
                    seconds_per_beat,
                    realtime,
                    &mut self.sync_arbiter,
                );
            } else if self.transport_controller.osc_sender_enabled {
                let bpm = self.engine.project()
                    .map_or(120.0_f32, |p| p.settings.bpm.0);
                let seconds_per_beat = if bpm > 0.0 { 60.0 / bpm } else { 0.5 };
                self.osc_sender.late_update(
                    self.engine.is_playing(),
                    self.engine.current_beat().as_f32(),
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
                    self.time_since_start.as_f32(),
                    p,
                    &mut self.editing_service,
                    beat.as_f32(),
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
                false, self.editing_service.data_version(),
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
                let native_device = self.content_pipeline.native_device().unwrap();
                let brightness = self
                    .engine
                    .project()
                    .map_or(1.0, |p| p.settings.led_brightness);
                if let Some(source) = self.content_pipeline.led_source_texture() {
                    // Poll previous frame's readback (send DMX if ready).
                    // Only when we still have an LED source — when transitioning
                    // to blackout we deliberately skip the poll so a stale
                    // completion can't briefly flash the prior frame on the LEDs.
                    led.poll_readback();
                    // Submit new frame: edge-extend compute + readback copy.
                    led.process_frame(
                        native_device,
                        source,
                        tick_result.ready_clips.len(),
                        brightness,
                    );
                } else {
                    // No layer is flagged `blit_to_led` (or none have active
                    // clips) — blackout. The controller cancels any in-flight
                    // readback inside this call. Texture pointer is unused.
                    led.process_frame(
                        native_device,
                        self.content_pipeline.export_output_texture(),
                        0,
                        brightness,
                    );
                }
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

                // GPU pass-level profiling not yet available on native Metal.
                let gpu_pass_count = 0u32;
                let gpu_total_ms = 0.0f64;
                let gpu_passes = Vec::new();

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
                    tick_result.ready_clips.iter().map(|entry| {
                        let progress = gen_renderer
                            .map_or(0.0, |gr| gr.get_clip_anim_progress(entry.clip_id.as_str()));
                        (entry.clip_id.to_string(), progress)
                    }).collect()
                };

                // Now borrow project immutably for layers, effects, params
                let layers = self.engine.project()
                    .map(|p| p.timeline.layers.as_slice())
                    .unwrap_or(&[]);

                let active_clip_info: Vec<manifold_profiler::ActiveClipInfo> =
                    tick_result.ready_clips.iter().enumerate().map(|(i, entry)| {
                        let layer = layers.get(entry.layer_index as usize);
                        let gen_param_values = layer.and_then(|l| l.gen_params());
                        let gen_type = layer
                            .map(|l| l.generator_type().clone())
                            .unwrap_or_default();
                        let gen_params = gen_param_values
                            .map(|gp| build_gen_params(&gen_type, &gp.param_values))
                            .unwrap_or_default();
                        let anim_progress = anim_map.get(i).map_or(0.0, |a| a.1);
                        manifold_profiler::ActiveClipInfo {
                            clip_id: entry.clip_id.to_string(),
                            generator_type: gen_type.to_string(),
                            layer_index: entry.layer_index,
                            anim_progress,
                            gen_params,
                        }
                    }).collect();

                // Collect active effect info with named live params + group_id
                let mut active_effects: Vec<manifold_profiler::ActiveEffectInfo> = Vec::new();
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
                    profiler_overhead_ms: 0.0,
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
            // Include Ableton as a modulation source. Bridge apply() runs after this
            // state push (same frame), so we use last frame's flag — on the following
            // frame, evaluate_modulation will have already reset param_values from the
            // updated base_param_values, so the snapshot will contain Ableton values.
            let modulation_active =
                tick_result.modulation_active || self.ableton_active_last_frame;

            // Reclaim tick_result buffers (ready_clips, stopped_clips) for reuse
            // on the next tick — avoids per-frame Vec allocation.
            self.engine.reclaim_tick_result(tick_result);

            // Arc<Project> snapshot: only deep-clone when data_version changes.
            // Modulation frames send a lightweight ModulationSnapshot instead
            // (just param_values Vec<f32> clones — no full Project clone).
            let snapshot = if version_changed {
                // Structural change — create a new Arc with a fresh clone.
                let arc = self.engine.project()
                    .map(|p| std::sync::Arc::new(p.clone()));
                self.cached_project_snapshot = arc.clone();
                arc
            } else {
                None
            };

            // Build lightweight modulation snapshot when drivers/envelopes are
            // active — contains only the param_values that changed this frame.
            // Uses a reusable scratch buffer: capture_into() clears and refills
            // without allocating (vecs keep capacity), then clone() copies the
            // flat buffer (3 allocations vs ~128 with the old nested Vec<Vec<f32>>).
            let modulation_snapshot = if modulation_active {
                if let Some(project) = self.engine.project() {
                    self.mod_scratch.capture_into(project);
                    Some(self.mod_scratch.clone())
                } else {
                    None
                }
            } else {
                None
            };

            // Update cached Arc<str> only when underlying values change.
            // On unchanged frames, .clone() = refcount bump (zero allocation).
            let new_pos = self.transport_controller.midi_clock_sync.as_ref()
                .map_or("", |s| s.current_position_display());
            if new_pos != &*self.cached_midi_clock_position {
                self.cached_midi_clock_position = Arc::from(new_pos);
            }
            let new_dev = self.transport_controller.midi_clock_sync.as_ref()
                .map_or("None", |s| s.selected_source_name());
            if new_dev != &*self.cached_midi_clock_device {
                self.cached_midi_clock_device = Arc::from(new_dev);
            }
            if self.osc_sync.current_timecode_display != *self.cached_osc_timecode {
                self.cached_osc_timecode =
                    Arc::from(self.osc_sync.current_timecode_display.as_str());
            }
            let new_perc = self.percussion_orchestrator.status_message();
            if new_perc != &*self.cached_perc_message {
                self.cached_perc_message = Arc::from(new_perc);
            }
            if self.cached_midi_device_names[..] != self.last_sent_midi_device_names[..] {
                self.last_sent_midi_device_names =
                    Arc::from(self.cached_midi_device_names.as_slice());
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
                gpu_fence_wait_ms: self.content_pipeline.last_fence_wait_ms() as f32,
                active_clips: self.engine.active_clip_count(),
                data_version: version,
                editing_is_dirty: self.editing_service.is_dirty(),
                bpm: self.engine.project().map_or(120.0, |p| p.settings.bpm.0 as f64),
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
                    .map_or(Bpm(120.0), |s| Bpm(s.current_clock_bpm())),
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
                #[cfg(target_os = "macos")]
                is_live_recording: self.content_pipeline.recording_session.is_some(),
                #[cfg(not(target_os = "macos"))]
                is_live_recording: false,
                #[cfg(target_os = "macos")]
                recording_dropped_frames: self.content_pipeline.recording_session
                    .as_ref()
                    .map_or(0, |s| s.frames_dropped()),
                #[cfg(not(target_os = "macos"))]
                recording_dropped_frames: 0,
                is_exporting: false,
                export_progress: 0.0,
                export_status: Arc::from(""),
                export_finished: None,
                ableton_session: if self.ableton_bridge.session_changed() {
                    Some(Arc::new(self.ableton_bridge.session().clone()))
                } else {
                    None
                },
                ableton_connected: self.ableton_bridge.is_connected(),
                ableton_transport_enabled: self.ableton_bridge.is_transport_enabled(),
                osc_sync_mode: self.engine.project()
                    .map_or(OscSyncMode::M4L, |p| p.settings.osc_sync_mode),
                project_snapshot: snapshot,
                modulation_snapshot,
                active_graph_snapshot: self
                    .content_pipeline
                    .graph_snapshot()
                    .map(Arc::new),
            };

            // Send state to UI. Unbounded channel — never drops snapshots.
            if let Err(e) = state_tx.send(state) {
                log::error!("[ContentThread] State channel disconnected: {e}");
            }
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
        let osc_sync_mode = self.engine.project()
            .map_or(OscSyncMode::M4L, |p| p.settings.osc_sync_mode);
        let authority = {
            // AbletonOSC transport is a command channel, not a clock source —
            // it must NOT claim authority. Only M4L timecode claims OSC authority
            // (because it was an actual timing source). MIDI Clock handles timing.
            let osc_receiving = match osc_sync_mode {
                OscSyncMode::M4L => self.osc_sync.is_receiving_timecode,
                OscSyncMode::AbletonOsc => false,
            };
            let auto = if self.transport_controller.midi_clock_sync.as_ref()
                .is_some_and(|s| s.is_midi_clock_enabled() && s.is_receiving_clock())
            {
                ClockAuthority::MidiClock
            } else if osc_receiving {
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
                    Bpm(link.link_tempo as f32),
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
                    Bpm(clk.current_clock_bpm()),
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

        // Ableton bridge — drain AbletonOSC replies and apply macro values.
        self.ableton_bridge.update(self.time_since_start.0);

        // When discovery just completed, validate mappings and force a full
        // project snapshot so the UI receives updated [ABL]/[ABL-]/[ABL?] statuses.
        if self.ableton_bridge.take_validation_dirty() {
            if let Some(p) = self.engine.project_mut() {
                self.ableton_bridge.validate_mappings(p);
                self.ableton_bridge.rebuild_listeners(p);
            }
            // Bump data_version so UI sees updated [ABL] statuses.
            self.editing_service.notify_external_change();
        }

        let ableton_active = if let Some(p) = self.engine.project_mut() {
            self.ableton_bridge.apply(p, self.time_since_start.0)
        } else {
            false
        };
        self.ableton_active_last_frame = ableton_active;

        // OSC timecode sync — M4L mode only.
        if osc_sync_mode == OscSyncMode::M4L {
            let snap = SyncTargetSnapshot::from_engine(&self.engine);
            self.osc_sync.update(
                now,
                &snap,
                &mut self.sync_arbiter,
                &mut self.engine,
                authority,
            );
        }

        // AbletonOSC inbound transport relay — DISABLED pending investigation.
        // The is_playing listener echoes were causing play/pause loops and
        // audio stutters. For now, M4L OscPositionSender handles outbound
        // and MIDI CLK handles inbound transport state.
        // TODO: Re-enable once echo suppression is reliable.
        // if osc_sync_mode == OscSyncMode::AbletonOsc
        //     && self.ableton_bridge.is_transport_receiving(now.0)
        // { ... }
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
                // Link only provides BPM — block position override when Manifold
                // owns transport (prevents brief authority-falls-to-Link glitches
                // during MIDI Clock gaps).
                if !self.sync_arbiter.manifold_owns_playback
                    && let Some(ref link) = self.transport_controller.link_sync
                    && link.is_link_enabled()
                        && link.has_active_peers()
                        && !self.link_beat_offset.is_nan()
                    {
                        self.engine
                            .set_beat(Beats(link.current_beat.0 - self.link_beat_offset));
                        self.engine.sync_time_from_beat();
                    }
            }
            ClockAuthority::MidiClock => {
                // MIDI Clock always drives position when active — suppressed only
                // during seek cooldown (user scrubbing, Ableton hasn't caught up).
                if !self.sync_arbiter.is_seek_cooldown_active(self.time_since_start)
                    && let Some(ref clk) = self.transport_controller.midi_clock_sync
                        && clk.is_midi_clock_enabled() && clk.is_receiving_clock() {
                            self.engine.set_beat(Beats::from_f32(clk.current_clock_beat()));
                            self.engine.sync_time_from_beat();
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
    pub(crate) fn cache_link_beat_offset(&mut self) {
        if let Some(ref link) = self.transport_controller.link_sync {
            if link.is_link_enabled() {
                let manifold_beat =
                    self.engine.time_to_timeline_beat(self.engine.current_time()).0;
                self.link_beat_offset = link.current_beat.0 - manifold_beat;
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
            .map_or(120.0_f32, |p| p.settings.bpm.0);

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
                .set_live_external_tempo(false, Bpm::DEFAULT, TempoPointSource::Unknown);
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
                        current_beat.as_f32(),
                        current_time.as_f32(),
                        bpm,
                        source,
                    );
                    if tempo_map_changed {
                        self.tempo_recorder.append_tempo_change(
                            &mut project.recording_provenance,
                            current_time.as_f32(),
                            current_beat.as_f32(),
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
                        project.tempo_map.get_bpm_at_beat(Beats::ZERO, project.settings.bpm);
                    let q_resolved_bpm = BeatQuantizer::quantize_bpm(bpm);
                    if (map_bpm.0 - q_resolved_bpm).abs() >= TempoRecorder::BPM_THRESHOLD {
                        project.tempo_map.add_or_replace_point(
                            Beats::ZERO, Bpm(bpm), source, 0.001,
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
    pub(crate) fn end_tempo_recording_session(&mut self) {
        if !self.tempo_recorder.is_session_active() {
            return;
        }

        let default_bpm = self
            .engine
            .project()
            .map_or(120.0, |p| p.settings.bpm.0);
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

}
