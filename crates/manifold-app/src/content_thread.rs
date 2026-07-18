//! Content thread — runs PlaybackEngine, EditingService, and ContentPipeline
//! on a dedicated thread. Communicates with the UI thread via crossbeam channels.
//!
//! The content thread owns all authoritative state: the engine (which owns the
//! project), the editing service (undo/redo), audio sync, percussion, and the
//! GPU content pipeline (generators + compositor).
use crossbeam_channel::{Receiver, Sender};
use std::sync::Arc;

use manifold_core::math::BeatQuantizer;
use manifold_core::types::{ClockAuthority, OscSyncMode, PlaybackState, TempoPointSource};
use manifold_core::{Beats, Bpm, Seconds};
use manifold_editing::service::EditingService;
use manifold_playback::clip_launcher::ClipLauncher;
use manifold_playback::engine::{PlaybackEngine, TickContext};
use manifold_playback::midi_input::MidiInputController;
use manifold_playback::osc_receiver::OscReceiver;
use manifold_playback::osc_sender::OscPositionSender;
use manifold_playback::osc_sync::OscSyncController;
use manifold_playback::percussion_orchestrator::PercussionImportOrchestrator;
use manifold_playback::audio_layer_playback::AudioLayerPlayback;
use manifold_playback::sync::{SyncArbiter, SyncTargetSnapshot};
use manifold_playback::tempo_recorder::TempoRecorder;
use manifold_playback::transport_controller::TransportController;
use manifold_renderer::gpu::GpuContext;

use crate::content_command::ContentCommand;
use crate::content_pipeline::ContentPipeline;
use crate::content_state::ContentState;
use crate::frame_timer::FrameTimer;

/// Cache entry for the watched editor-canvas snapshot (effect OR generator).
/// Holds the snapshot inside an `Arc` so the per-frame
/// `ContentState::active_graph_snapshot` field can clone the refcount
/// (no deep copy). At most one entry — only one canvas is visible at a time.
///
/// Invalidated when any of the snapshot's inputs change:
/// - `target` — the watched instance changed (canvas swap).
/// - `preset_type` — the instance's preset type changed in place
///   (effect/generator type swap via the picker while watched).
/// - `version` — the instance's `graph_version` bumped (a graph-editor edit
///   landed on it).
/// - `fingerprint` — the embedded-preset catalog overlay was reinstalled
///   (a fork or recalibrate), which can change the compositor-sourced
///   routings / default graph even when `version` didn't move.
pub struct CachedGraphSnapshot {
    pub target: manifold_core::GraphTarget,
    pub preset_type: manifold_core::PresetTypeId,
    pub version: u32,
    pub fingerprint: u64,
    pub snapshot: Arc<manifold_renderer::node_graph::GraphSnapshot>,
}

/// An in-flight "export current frame" request. Captured across two content
/// ticks so the live render never stalls: tick N submits the GPU readback (and
/// fills `dims`), tick N+1 reads the pixels back and hands them to an off-thread
/// encoder. See [`ContentThread::submit_still_export_if_pending`] /
/// [`ContentThread::poll_still_export`].
pub struct StillExportJob {
    pub path: String,
    pub format: manifold_media::still_exporter::StillFormat,
    /// Captured dimensions, set once the readback has been submitted. `None`
    /// until then — the poll step skips jobs that haven't been submitted yet.
    pub dims: Option<(u32, u32)>,
}

/// Owns all content-side state and runs the content loop.
pub struct ContentThread {
    pub engine: PlaybackEngine,
    pub editing_service: EditingService,
    pub content_pipeline: ContentPipeline,
    /// Per-clip audio-layer playback (one kira voice per active audio clip).
    /// `None` if the kira backend failed to open. See `docs/AUDIO_LAYER_DESIGN.md`.
    pub audio_layer_playback: Option<AudioLayerPlayback>,
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

    /// Pending single-frame export, if any. Set by `ContentCommand::ExportFrame`
    /// and cleared once the frame has been read back and dispatched to the
    /// off-thread encoder. At most one capture is in flight at a time.
    pub still_export: Option<StillExportJob>,

    // ── MIDI device cache ──
    /// Cached MIDI device names, refreshed every ~2 seconds.
    pub cached_midi_device_names: Vec<String>,
    pub last_midi_device_scan_time: Seconds,

    // ── Cached project snapshot (Arc avoids deep clone every modulation frame) ──
    pub cached_project_snapshot: Option<std::sync::Arc<manifold_core::project::Project>>,

    /// Effect instance the editor canvas is currently watching, keyed
    /// by stable `EffectId`. The content thread looks up the instance
    /// in the project each frame and snapshots either its per-card
    /// graph override (if `instance.graph.is_some()`) or the catalog
    /// default for its type. `None` means the editor isn't focused on
    /// any effect — canvas stays empty until the user clicks a cog.
    /// Phase 2 of per-card divergence — see `docs/NODE_GRAPH_SYSTEM.md`.
    pub watched_graph_target: Option<manifold_core::GraphTarget>,
    /// Node the editor is previewing within the watched effect/generator, if
    /// any. Combined with `watched_graph_target` each frame to drive the
    /// per-node output capture. `None` = no preview.
    pub preview_graph_node: Option<manifold_core::NodeId>,
    /// Whether the node-output preview applies auto-gain/normalization. Off by
    /// default; toggled from the editor's preview pane ("Smart preview"). Pushed
    /// to the pipeline each frame. Node preview only — never affects the live
    /// render.
    pub node_preview_normalize: bool,
    /// Cached editor-canvas snapshot for the watched effect or generator.
    ///
    /// Built lazily by [`Self::graph_snapshot`] and invalidated per the
    /// key documented on [`CachedGraphSnapshot`]. Avoids the per-content-tick
    /// rebuild (re-parse of the bundled preset JSON / compositor lookup) the
    /// non-cached paths used to do every state push the editor was open.
    /// Holds at most one entry: only one canvas is visible at a time.
    pub cached_graph_snapshot: Option<CachedGraphSnapshot>,

    // ── Reusable modulation scratch (flat buffer — zero alloc after first frame) ──
    pub mod_scratch: crate::content_state::ModulationSnapshot,

    /// Audio-modulation capture runtime — owns the live audio capture device +
    /// feature worker and feeds the engine its per-send feature snapshot each
    /// tick. Idle (no device open) until the project has an active audio
    /// modulation. See [`crate::audio_mod_runtime`].
    pub audio_mod_runtime: crate::audio_mod_runtime::AudioModRuntime,

    // ── Cached ContentState strings (Arc<str> — clone = refcount bump, zero alloc) ──
    pub cached_midi_clock_position: Arc<str>,
    pub cached_midi_clock_device: Arc<str>,
    pub cached_perc_message: Arc<str>,
    /// Last-sent MIDI device names — only reallocated when the list changes.
    pub last_sent_midi_device_names: Arc<[String]>,

    /// Fingerprint of the project's embedded ("forked") presets, so an
    /// editing command that forks a preset or recalibrates an embedded one
    /// re-installs the renderer catalog overlay (and re-derives the core
    /// registry) — without paying that catalog rebuild on every unrelated
    /// edit. `0` while the project carries no forks (the common case), so the
    /// guard is a single integer compare on the editing path. See
    /// [`Self::refresh_preset_overlay_if_changed`].
    pub embedded_presets_fingerprint: u64,

    /// D11 undo/redo toast (`UI_CRAFT_AND_MOTION_PLAN.md` P2) — set by
    /// `handle_command`'s `Undo`/`Redo` arms (peeked from
    /// `EditingService::peek_undo_description`/`peek_redo_description` BEFORE
    /// the command moves stacks), consumed by `.take()` at the next
    /// `ContentState` build in the SAME loop iteration (see `content_thread.rs`'s
    /// per-tick state construction). Rides the regular per-tick snapshot rather
    /// than a separate out-of-band send — see `UndoRedoEvent`'s doc comment.
    pub pending_undo_redo_event: Option<crate::content_state::UndoRedoEvent>,

    // ── Profiling ──
    /// Active profiling session (only present when feature = "profiling").
    #[cfg(feature = "profiling")]
    pub profiler: Option<manifold_profiler::ProfileSession>,
}

/// D4 (`docs/PARAM_TWO_WAY_BINDING_DESIGN.md`): overlay the EFFECTIVE
/// (forward-reshaped) value onto every card-bound node-face row, instead of
/// the raw `def.nodes[..].params` value `GraphSnapshot::from_def` reads —
/// which may hold a stale node-face write `apply_bindings` stomps on the
/// next rebuild (BUG-158's snap-back). Walks `def.preset_metadata.bindings`;
/// for each `Node` target, reads the outer card's live value off
/// `instance.params` and pushes it through `apply_card_reshape` before
/// overwriting the matching row's `current_value`. A no-op for every unbound
/// param (no metadata, or no binding targets it) and for a binding whose
/// outer id isn't in the manifest (an authoring-time gap, not a user state).
fn apply_effective_bound_values(
    snap: &mut manifold_renderer::node_graph::GraphSnapshot,
    def: &manifold_core::effect_graph_def::EffectGraphDef,
    instance: &manifold_core::effects::PresetInstance,
) {
    use manifold_core::effect_graph_def::BindingTarget;
    let Some(meta) = def.preset_metadata.as_ref() else {
        return;
    };
    for binding in &meta.bindings {
        let BindingTarget::Node { node_id, param } = &binding.target else {
            continue;
        };
        if node_id.is_empty() {
            continue;
        }
        let Some(spec) = meta.params.iter().find(|p| p.id == binding.id) else {
            continue;
        };
        let Some(card_value) = instance.params.get(&binding.id).map(|p| p.value) else {
            continue;
        };
        let effective = manifold_core::effects::apply_card_reshape(
            card_value,
            spec.min,
            spec.max,
            spec.invert,
            spec.curve,
            binding.scale,
            binding.offset,
        );
        if let Some(row) = find_node_param_row_mut(&mut snap.nodes, node_id, param) {
            row.current_value = effective;
        }
    }
}

/// Find the `ParamSnapshot` for `(node_id, param)` anywhere in `nodes`,
/// recursing into group bodies — a bound node can live inside a group
/// (BUG-103's glTF per-object case).
fn find_node_param_row_mut<'a>(
    nodes: &'a mut [manifold_renderer::node_graph::NodeSnapshot],
    node_id: &manifold_core::NodeId,
    param: &str,
) -> Option<&'a mut manifold_renderer::node_graph::ParamSnapshot> {
    for node in nodes.iter_mut() {
        if &node.node_id == node_id
            && let Some(row) = node.parameters.iter_mut().find(|p| p.name == param)
        {
            return Some(row);
        }
        if let Some(group) = node.group.as_mut()
            && let Some(row) = find_node_param_row_mut(&mut group.nodes, node_id, param)
        {
            return Some(row);
        }
    }
    None
}

/// Set the CALLING thread to real-time scheduling via
/// `THREAD_TIME_CONSTRAINT_POLICY` at `target_fps`. This is the native macOS
/// real-time API (used by CoreAudio, game engines): it tells the kernel "I'm
/// a periodic real-time workload with a specific deadline," so the scheduler
/// reserves time slots and `mach_wait_until` (`FrameTimer::wait_for_deadline`)
/// wakes with sub-microsecond precision instead of the 1-2ms jitter a
/// normally-scheduled thread gets. `pub(crate)` (extracted from `run()`,
/// PERF_BUDGET_GATE_DESIGN.md P1): the headless `perf-soak` xtask drives
/// frames directly rather than through `run()`'s loop, and without this same
/// call its `mach_wait_until` calls pace at roughly half the target rate on a
/// normally-scheduled CLI thread (measured: ~28fps actual against a 60fps
/// target on the Liveschool fixture) — a fidelity gap the soak's whole point
/// is to avoid. No behavior change to `run()` itself, same policy values.
///
/// SCHED_RR (POSIX) was used previously but macOS doesn't honor it for
/// real-time — it falls back to normal scheduling with 1-2ms jitter.
#[cfg(target_os = "macos")]
pub(crate) fn apply_realtime_thread_policy(target_fps: f64) {
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
        (std::mem::size_of::<ThreadTimeConstraintPolicy>() / std::mem::size_of::<u32>()) as u32;

    // Convert frame timing to Mach absolute time units.
    // On Apple Silicon: timebase 1:1, so 1 tick = 1 nanosecond.
    let frame_ns = (1_000_000_000.0 / target_fps) as u32;
    // Computation budget: allow up to 75% of the frame for render work.
    // The remaining 25% is headroom for the scheduler.
    let computation_ns = (frame_ns as f64 * 0.75) as u32;

    let policy = ThreadTimeConstraintPolicy {
        period: frame_ns,            // 16.67ms at 60fps
        computation: computation_ns, // 12.5ms max render time
        constraint: frame_ns,        // must complete within one period
        preemptible: 1,              // can be preempted during computation
    };

    let mach_thread = unsafe { pthread_mach_thread_np(libc::pthread_self()) };
    let ret =
        unsafe { thread_policy_set(mach_thread, THREAD_TIME_CONSTRAINT_POLICY, &policy, POLICY_COUNT) };

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
            fn pthread_set_qos_class_self_np(qos_class: u32, relative_priority: i32) -> i32;
        }
        let qos_ret = unsafe { pthread_set_qos_class_self_np(0x21, 0) };
        if qos_ret != 0 {
            log::warn!("[ContentThread] QoS fallback also failed (err={})", qos_ret,);
        } else {
            log::info!("[ContentThread] QoS set to USER_INTERACTIVE (fallback)");
        }
    }
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

        apply_realtime_thread_policy(self.timer.target_fps());

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
                        if let Some(session) = self.content_pipeline.recording_session.take() {
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
                    log::info!("[ContentThread] shutdown received during surface wait");
                    return;
                }
                self.content_pipeline
                    .set_last_fence_wait_ms(fence_start.elapsed().as_secs_f64() * 1000.0);
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
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);

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
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
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
                Ok(cmd @ ContentCommand::SeekTo(_)) | Ok(cmd @ ContentCommand::SeekToBeat(_)) => {
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
    /// `pub(crate)` (not private): PERF_BUDGET_GATE_DESIGN.md P1's headless
    /// `perf-soak` xtask drives frames directly (real-time paced via
    /// `self.timer.wait_for_deadline()` between calls, same as `run()`'s own
    /// loop) instead of going through the command channel — no behavior
    /// change to this function itself.
    pub(crate) fn tick_frame(&mut self, state_tx: &Sender<ContentState>) {
        let dt = self.timer.consume_tick();
        let realtime = self.timer.realtime_since_start();
        self.time_since_start = Seconds(realtime);

        // Read back a still-frame export submitted on the previous tick (if any).
        // Runs before this frame's render so the GPU completion we wait on is the
        // capture's, not a newer frame's. The encode itself runs off-thread.
        #[cfg(target_os = "macos")]
        self.poll_still_export(state_tx);

        // Refresh MIDI device list every ~2 seconds
        if (self.time_since_start - self.last_midi_device_scan_time).0 >= 2.0 {
            self.cached_midi_device_names =
                manifold_playback::midi_clock_sync::MidiClockSyncController::available_source_names(
                );
            self.last_midi_device_scan_time = self.time_since_start;
        }

        // Profiling: frame start timestamp
        #[cfg(feature = "profiling")]
        let _frame_start = std::time::Instant::now();

        // 3. Process MIDI input (before engine tick — matches Unity Update() ordering).
        // Drains hardware note events and routes them to ClipLauncher → LiveClipManager.
        #[cfg(feature = "profiling")]
        let _t0 = std::time::Instant::now();

        self.engine
            .tick_midi_input(&mut self.midi_input, &mut self.clip_launcher, realtime);

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
        let authority = self
            .engine
            .project()
            .map_or(ClockAuthority::Internal, |p| p.settings.clock_authority);
        self.derive_external_beat(authority);
        self.update_recording_session_state(authority);
        self.apply_resolved_tempo(authority);

        #[cfg(feature = "profiling")]
        let _sync_controllers_ms = _t0.elapsed().as_secs_f64() * 1000.0;

        // 3d. Audio modulation: reconcile capture against the project (on change)
        // and feed the engine the latest per-send feature snapshot. Must run
        // BEFORE engine.tick() (the modulation pipeline reads the snapshot).
        self.audio_mod_runtime.update(
            &mut self.engine,
            self.editing_service.data_version(),
            self.audio_layer_playback.as_mut(),
        );

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
        let mut tick_result = self.engine.tick(ctx);

        // 4b. Transport output (LateUpdate equivalent — after engine tick).
        // In M4L mode: OscPositionSender sends /manifold/* to M4L device.
        // In AbletonOSC mode: AbletonBridge sends /live/song/* to AbletonOSC.
        // Outbound transport: AbletonOSC mode uses bridge, M4L mode uses
        // OscPositionSender. Both use the same fire-and-forget pattern.
        let osc_sync_mode = self
            .engine
            .project()
            .map_or(OscSyncMode::M4L, |p| p.settings.osc_sync_mode);
        if osc_sync_mode == OscSyncMode::AbletonOsc && self.ableton_bridge.is_transport_enabled() {
            let clk_receiving = self
                .transport_controller
                .midi_clock_sync
                .as_ref()
                .is_some_and(|s| s.is_midi_clock_enabled() && s.is_receiving_clock());
            self.ableton_bridge.late_update_transport(
                self.engine.is_playing(),
                self.engine.current_beat().as_f32(),
                realtime,
                clk_receiving,
            );
            // The arbiter's suppress flag only serves the M4L sender; a
            // CLK-relayed play sets it unconditionally, and with the M4L
            // sender disabled nothing would ever consume it — enabling SYNC
            // later would then swallow the first real transport edge
            // (CORE_ENGINE_MAP §13.12). Clear it each frame in AbletonOSC
            // mode; the closed-loop machine needs no suppression flag.
            if !self.transport_controller.osc_sender_enabled {
                self.sync_arbiter.suppress_next_transport = false;
            }
        } else if self.transport_controller.osc_sender_enabled {
            let bpm = self
                .engine
                .project()
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

        // 5c. Audio-layer playback — one kira voice per active audio clip,
        // following the transport. See docs/AUDIO_LAYER_DESIGN.md §4.
        if let Some(ref mut audio_layer_playback) = self.audio_layer_playback
            && let Some(project) = self.engine.project()
        {
            audio_layer_playback.update(project, &self.engine);
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

        // 6a2. Commit any automation recording gestures that finished this
        // tick (§5) — one undo entry per gesture, built by
        // `crate::automation::evaluate_all_automation`'s gesture-closure
        // pass inside `self.engine.tick()` above. Mirrors the percussion
        // tick just above: `&mut Project` + `&mut EditingService` handed to
        // the command synchronously, on the same content-thread frame.
        if !tick_result.pending_gesture_commits.is_empty()
            && let Some(p) = self.engine.project_mut()
        {
            for cmd in tick_result.pending_gesture_commits.drain(..) {
                self.editing_service.execute(cmd, p);
            }
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

        // Forward the authoring-time node-output preview request to the
        // pipeline. Effect and generator are mutually exclusive (only one
        // editor canvas is watched at a time); the inactive one is cleared.
        let (effect_preview, generator_preview) = match &self.watched_graph_target {
            Some(manifold_core::GraphTarget::Effect(eid)) => {
                (Some((eid.clone(), self.preview_graph_node.clone())), None)
            }
            Some(manifold_core::GraphTarget::Generator(lid)) => {
                (None, Some((lid.clone(), self.preview_graph_node.clone())))
            }
            None => (None, None),
        };
        self.content_pipeline
            .set_node_preview_request(effect_preview);
        self.content_pipeline
            .set_node_preview_generator(generator_preview);
        self.content_pipeline
            .set_node_preview_normalize(self.node_preview_normalize);

        let render_work_start = std::time::Instant::now();
        self.content_pipeline.render_content(
            &self.gpu,
            &mut self.engine,
            &tick_result,
            dt,
            self.frame_count,
            false,
            self.editing_service.data_version(),
        );
        let _render_work_ms = render_work_start.elapsed().as_secs_f64() * 1000.0;

        // Submit a pending still-frame readback now that the frame the user sees
        // is fully rendered. Read back next tick (see poll_still_export above).
        #[cfg(target_os = "macos")]
        self.submit_still_export_if_pending();

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
            self.content_pipeline
                .cleanup_stopped_clips(&tick_result.stopped_clips);
        }

        // 7c. LED output — native Metal: dispatch edge-extend compute on
        // compositor output, readback tiny pixel grid, send DMX/ArtNet.
        // Uses a dedicated encoder (separate from the content frame).
        if let Some(ref mut led) = self.led_controller {
            let native_device = self.content_pipeline.native_device().unwrap();
            let (brightness, led_gain) = self
                .engine
                .project()
                .map_or((1.0, 1.0), |p| (p.settings.led_brightness, p.settings.led_gain));
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
                    led_gain,
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
                    led_gain,
                );
            }
        }

        #[cfg(feature = "profiling")]
        let _cleanup_ms = _t0.elapsed().as_secs_f64() * 1000.0;

        self.frame_count += 1;

        // Chain-dispatch instrumentation — once a second, print a
        // summary of how the chain-graph hot path is being used.
        // Cheap (one atomic-swap per counter). Gated behind the
        // env var so production builds stay silent.
        if std::env::var("MANIFOLD_LOG_CHAIN_STATS").is_ok() && self.frame_count.is_multiple_of(60)
        {
            let s = manifold_renderer::chain_dispatch::take_chain_dispatch_stats();
            if s.dispatches > 0 {
                let avg_effects = s.effects as f64 / s.dispatches.max(1) as f64;
                let avg_dispatch_us = (s.dispatch_ns as f64 / 1000.0) / s.dispatches.max(1) as f64;
                let avg_graph_run_us =
                    (s.graph_run_ns as f64 / 1000.0) / s.graph_runs.max(1) as f64;
                let avg_rebuild_us = if s.rebuilds > 0 {
                    (s.rebuild_ns as f64 / 1000.0) / s.rebuilds as f64
                } else {
                    0.0
                };
                eprintln!(
                    "[chain-stats] over last 60 frames: dispatches={} \
                         effects={} (avg {:.1}/chain) graph_runs={} \
                         rebuilds={} | avg dispatch={:.1}μs graph_run={:.1}μs \
                         rebuild={:.1}μs | totals dispatch={:.2}ms graph_run={:.2}ms \
                         rebuild={:.2}ms",
                    s.dispatches,
                    s.effects,
                    avg_effects,
                    s.graph_runs,
                    s.rebuilds,
                    avg_dispatch_us,
                    avg_graph_run_us,
                    avg_rebuild_us,
                    s.dispatch_ns as f64 / 1_000_000.0,
                    s.graph_run_ns as f64 / 1_000_000.0,
                    s.rebuild_ns as f64 / 1_000_000.0,
                );
            }
        }

        // Profiling: record frame data
        #[cfg(feature = "profiling")]
        if let Some(ref mut profiler) = self.profiler
            && profiler.is_recording()
        {
            let frame_wall_ms = _frame_start.elapsed().as_secs_f64() * 1000.0;
            let current_beat = self.engine.current_beat();
            let time_sig = self
                .engine
                .project()
                .map_or(4, |p| p.settings.time_signature_numerator.max(1));
            let bar = (current_beat.as_f32() / time_sig as f32).floor() as u32;
            let budget_ms = 1000.0 / self.timer.target_fps();
            let active_layers = self.engine.project().map_or(0, |p| p.timeline.layers.len());

            // GPU pass-level profiling not yet available on native Metal.
            let gpu_pass_count = 0u32;
            let gpu_total_ms = 0.0f64;
            let gpu_passes = Vec::new();

            // Helper: build named params from values + registry
            fn build_effect_params(
                fx: &manifold_core::effects::PresetInstance,
            ) -> Vec<manifold_profiler::NamedParam> {
                // The manifest entry carries its own display name (`spec.name`,
                // seeded from the registry def or a user label) and effective
                // value — no positional registry lookup needed.
                fx.params
                    .iter()
                    .map(|p| manifold_profiler::NamedParam {
                        name: p.spec.name.clone(),
                        value: p.value,
                    })
                    .collect()
            }

            fn build_gen_params(
                params: &manifold_core::params::ParamManifest,
            ) -> Vec<manifold_profiler::NamedParam> {
                params
                    .iter()
                    .map(|p| manifold_profiler::NamedParam {
                        name: p.spec.name.clone(),
                        value: p.value,
                    })
                    .collect()
            }

            // Get anim_progress from generator_renderer (mutable borrow, done first)
            let anim_map: Vec<(String, f32)> = {
                let (renderers, _) = self.engine.split_renderer_project();
                let gen_renderer = renderers.iter().find_map(|r| {
                        r.as_any().downcast_ref::<manifold_renderer::generator_renderer::GeneratorRenderer>()
                    });
                tick_result
                    .ready_clips
                    .iter()
                    .map(|entry| {
                        let progress = gen_renderer
                            .map_or(0.0, |gr| gr.get_clip_anim_progress(entry.clip_id.as_str()));
                        (entry.clip_id.to_string(), progress)
                    })
                    .collect()
            };

            // Now borrow project immutably for layers, effects, params
            let layers = self
                .engine
                .project()
                .map(|p| p.timeline.layers.as_slice())
                .unwrap_or(&[]);

            let active_clip_info: Vec<manifold_profiler::ActiveClipInfo> = tick_result
                .ready_clips
                .iter()
                .enumerate()
                .map(|(i, entry)| {
                    let layer = layers.get(entry.layer_index as usize);
                    let gen_param_values = layer.and_then(|l| l.gen_params());
                    let gen_type = layer
                        .map(|l| l.generator_type().clone())
                        .unwrap_or_default();
                    let gen_params = gen_param_values
                        .map(|gp| build_gen_params(&gp.params))
                        .unwrap_or_default();
                    let anim_progress = anim_map.get(i).map_or(0.0, |a| a.1);
                    manifold_profiler::ActiveClipInfo {
                        clip_id: entry.clip_id.to_string(),
                        generator_type: gen_type.to_string(),
                        layer_index: entry.layer_index,
                        anim_progress,
                        gen_params,
                    }
                })
                .collect();

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
            let layer_states: Vec<manifold_profiler::LayerState> = layers
                .iter()
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
            let estimated_tex_bytes =
                comp_w as u64 * comp_h as u64 * bytes_per_pixel * rt_count as u64;

            profiler.record_frame(manifold_profiler::FrameRecord {
                index: self.frame_count - 1,
                beat: current_beat.as_f32(),
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
        // Send a project snapshot when data_version changes (editing commands).
        // Value-only writers (LFO/envelope/Ableton/OSC/automation) never bump
        // data_version — those ride the ModulationSnapshot, which is now sent
        // EVERY tick (see below), so no writer class can leave the UI stale.

        // Reclaim tick_result buffers (ready_clips, stopped_clips) for reuse
        // on the next tick — avoids per-frame Vec allocation.
        self.engine.reclaim_tick_result(tick_result);

        // Arc<Project> snapshot: only deep-clone when data_version changes.
        // Per-frame param values ride the lightweight ModulationSnapshot
        // instead (just param_values Vec<f32> clones — no full Project clone).
        let snapshot = if version_changed {
            // Structural change — create a new Arc with a fresh clone.
            let arc = self
                .engine
                .project()
                .map(|p| std::sync::Arc::new(p.clone()));
            self.cached_project_snapshot = arc.clone();
            arc
        } else {
            None
        };

        // Build the lightweight modulation snapshot EVERY tick, not only when
        // drivers/envelopes/Ableton are active: OSC-router writes, automation
        // lane sampling, and MutateProjectLive drags all move param_values
        // without flagging `modulation_active`, and gating the send on that
        // flag left the UI showing stale values until the next structural
        // snapshot. The capture is a linear walk over the param manifests into
        // a reusable scratch buffer (zero-alloc steady state), then one clone
        // of the flat buffer. Blocks are id-keyed at capture, so a structural
        // edit landing between capture and the UI's apply can't misroute
        // values (see ModulationSnapshot).
        let modulation_snapshot = if let Some(project) = self.engine.project() {
            self.mod_scratch.capture_into(project);
            Some(self.mod_scratch.clone())
        } else {
            None
        };

        // Update cached Arc<str> only when underlying values change.
        // On unchanged frames, .clone() = refcount bump (zero allocation).
        let new_pos = self
            .transport_controller
            .midi_clock_sync
            .as_ref()
            .map_or("", |s| s.current_position_display());
        if new_pos != &*self.cached_midi_clock_position {
            self.cached_midi_clock_position = Arc::from(new_pos);
        }
        let new_dev = self
            .transport_controller
            .midi_clock_sync
            .as_ref()
            .map_or("None", |s| s.selected_source_name());
        if new_dev != &*self.cached_midi_clock_device {
            self.cached_midi_clock_device = Arc::from(new_dev);
        }
        let new_perc = self.percussion_orchestrator.status_message();
        if new_perc != &*self.cached_perc_message {
            self.cached_perc_message = Arc::from(new_perc);
        }
        if self.cached_midi_device_names[..] != self.last_sent_midi_device_names[..] {
            self.last_sent_midi_device_names = Arc::from(self.cached_midi_device_names.as_slice());
        }

        let perc_progress = self.percussion_orchestrator.status_progress01();
        let perc_show = self.percussion_orchestrator.show_progress_bar()
            && !self.cached_perc_message.is_empty();

        // Resolve the editor-canvas graph snapshot once. Effect path
        // returns a plain `GraphSnapshot` (cheap to construct, no
        // cache needed); generator path returns a cached `Arc` clone
        // (avoids re-parsing the bundled JSON every state push when
        // the canvas is open). One target, one snapshot path (fork #10).
        let active_graph_snapshot_arc = self
            .watched_graph_target
            .clone()
            .and_then(|t| self.graph_snapshot(&t));

        // Per-send audio levels for the Audio Setup meters — the full-band
        // amplitude (overall loudness, 0..1).
        let full = manifold_core::AudioBand::Full.index();
        let mut audio_send_levels = [0.0f32; manifold_audio::analysis::MAX_SENDS];
        let audio_snapshot = self.engine.audio_snapshot();
        let audio_send_count = audio_snapshot.sends.len().min(audio_send_levels.len());
        for (dst, f) in audio_send_levels
            .iter_mut()
            .zip(audio_snapshot.sends.iter().take(audio_send_count))
        {
            *dst = f.bands[full].amplitude;
        }

        // Drain any new VQT spectrogram columns for the Audio Setup scope. Empty
        // unless the scope is open on a send; the UI feeds them to the waterfall.
        let mut spectrogram_columns = Vec::new();
        self.audio_mod_runtime
            .drain_spectrogram_columns(|col| spectrogram_columns.extend_from_slice(col));
        // Per-column overlay records (centroid traces + onset tick lanes),
        // drained in lockstep with the columns above — one ScopeColumn each.
        let mut spectrogram_col_scalars = Vec::new();
        self.audio_mod_runtime
            .drain_spectrogram_scalars(|col| spectrogram_col_scalars.push(col));
        let spectrogram_num_bins = self.audio_mod_runtime.spectrogram_num_bins();
        let (spectrogram_fmin, spectrogram_fmax) = self
            .audio_mod_runtime
            .spectrogram_freq_range()
            .unwrap_or((0.0, 0.0));
        // The editable Low/Mid/High crossovers, for the band-divider lines + the
        // per-band meters. Fall back to the historical defaults pre-project.
        let (spectrogram_low_hz, spectrogram_mid_hz) = self.engine.project().map_or(
            (
                manifold_core::audio_setup::DEFAULT_LOW_HZ,
                manifold_core::audio_setup::DEFAULT_MID_HZ,
            ),
            |p| (p.audio_setup.low_hz, p.audio_setup.mid_hz),
        );
        // The tapped send's features, for the scope's per-band level meters.
        let spectrogram_features = self
            .audio_mod_runtime
            .tapped_send_index()
            .and_then(|i| audio_snapshot.sends.get(i).copied());

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
            bpm: self
                .engine
                .project()
                .map_or(120.0, |p| p.settings.bpm.0 as f64),
            frame_rate: self
                .engine
                .project()
                .map_or(60.0, |p| p.settings.frame_rate as f64),
            clock_authority: self
                .engine
                .project()
                .map_or(manifold_core::types::ClockAuthority::Internal, |p| {
                    p.settings.clock_authority
                }),
            time_signature_numerator: self
                .engine
                .project()
                .map_or(4, |p| p.settings.time_signature_numerator),
            link_enabled: self
                .transport_controller
                .link_sync
                .as_ref()
                .is_some_and(|s| s.is_link_enabled()),
            link_peers: self
                .transport_controller
                .link_sync
                .as_ref()
                .map_or(0, |s| s.num_peers),
            midi_clock_enabled: self
                .transport_controller
                .midi_clock_sync
                .as_ref()
                .is_some_and(|s| s.is_midi_clock_enabled()),
            midi_clock_position_display: self.cached_midi_clock_position.clone(),
            midi_clock_receiving: self
                .transport_controller
                .midi_clock_sync
                .as_ref()
                .is_some_and(|s| s.is_receiving_clock()),
            midi_clock_device_name: self.cached_midi_clock_device.clone(),
            midi_device_names: self.last_sent_midi_device_names.clone(),
            audio_send_levels,
            audio_send_count,
            // D6 fire meter (`AUDIO_SETUP_DOCK_AND_TRIGGER_UNIFICATION_
            // DESIGN.md` P3c, BUG-082's fix): the tick just run's shaped-
            // signal capture for every fire-mode config, `Copy` off the
            // engine — no allocation. See `PlaybackEngine::fire_meters`.
            fire_meters: self.engine.fire_meters(),
            spectrogram_columns,
            spectrogram_col_scalars,
            spectrogram_num_bins,
            spectrogram_fmin,
            spectrogram_fmax,
            spectrogram_low_hz,
            spectrogram_mid_hz,
            spectrogram_features,
            osc_sender_enabled: self.transport_controller.osc_sender_enabled,
            percussion_importing: self.percussion_orchestrator.is_import_in_progress(),
            percussion_status_message: self.cached_perc_message.clone(),
            percussion_progress: if perc_progress < 0.0 {
                0.0
            } else {
                perc_progress.clamp(0.0, 1.0)
            },
            percussion_show_progress: perc_show,
            profiling_active: {
                #[cfg(feature = "profiling")]
                {
                    self.profiler.as_ref().is_some_and(|p| p.is_recording())
                }
                #[cfg(not(feature = "profiling"))]
                {
                    false
                }
            },
            profiling_frame_count: {
                #[cfg(feature = "profiling")]
                {
                    self.profiler.as_ref().map_or(0, |p| p.frame_count())
                }
                #[cfg(not(feature = "profiling"))]
                {
                    0
                }
            },
            led_enabled: self.led_controller.as_ref().is_some_and(|c| c.is_enabled()),
            #[cfg(target_os = "macos")]
            is_live_recording: self.content_pipeline.recording_session.is_some(),
            #[cfg(not(target_os = "macos"))]
            is_live_recording: false,
            #[cfg(target_os = "macos")]
            recording_dropped_frames: self
                .content_pipeline
                .recording_session
                .as_ref()
                .map_or(0, |s| s.frames_dropped()),
            #[cfg(not(target_os = "macos"))]
            recording_dropped_frames: 0,
            #[cfg(target_os = "macos")]
            recording_dropped_audio_frames: self
                .content_pipeline
                .recording_session
                .as_ref()
                .map_or(0, |s| s.audio_frames_dropped()),
            #[cfg(not(target_os = "macos"))]
            recording_dropped_audio_frames: 0,
            // Export runs its own blocking loop (`run_export` /
            // `send_export_progress` in content_export.rs) that sends
            // dedicated degraded ContentState snapshots — the regular
            // per-tick build here never runs while an export is in
            // progress, so these are always the "not exporting" values.
            is_exporting: false,
            export_progress: 0.0,
            export_status: Arc::from(""),
            export_finished: None,
            undo_redo_event: self.pending_undo_redo_event.take(),
            ableton_session: if self.ableton_bridge.session_changed() {
                Some(Arc::new(self.ableton_bridge.session().clone()))
            } else {
                None
            },
            ableton_connected: self.ableton_bridge.is_connected(),
            ableton_transport_enabled: self.ableton_bridge.is_transport_enabled(),
            ableton_sync_status: self.ableton_bridge.transport_sync_status(),
            osc_sync_mode: self
                .engine
                .project()
                .map_or(OscSyncMode::M4L, |p| p.settings.osc_sync_mode),
            project_snapshot: snapshot,
            modulation_snapshot,
            active_graph_snapshot: active_graph_snapshot_arc,
            node_preview_info: self.content_pipeline.node_preview_info(),
            live_node_params: self.content_pipeline.live_node_params(),
            node_atlas_layout: self.content_pipeline.node_atlas_layout().to_vec(),
            clip_atlas_layout: self.content_pipeline.clip_atlas_layout().to_vec(),
            automation_latched_params: self
                .engine
                .automation_latches()
                .keys()
                .cloned()
                .collect(),
            automation_armed: self.engine.automation_armed(),
        };

        // Send state to UI. Unbounded channel — never drops snapshots.
        if let Err(e) = state_tx.send(state) {
            log::error!("[ContentThread] State channel disconnected: {e}");
        }
    }

    /// Build (or return cached) the editor canvas's graph snapshot for the
    /// currently-watched preset instance — effect or generator (fork #10).
    ///
    /// Two paths per kind, mirrored:
    /// - The instance has `graph: Some(def)` → build directly from the
    ///   per-instance override (renders what the user saved). Effects fill
    ///   `outer_routings` from the compositor (the outer→inner map is static
    ///   per type); generators project routings from the override's own
    ///   (bundle-grafted) `preset_metadata`, which also captures user-added
    ///   bindings.
    /// - The instance has `graph: None` → the type-keyed view: effects use the
    ///   compositor's `graph_snapshot_for`, generators the unified
    ///   `LoadedPresetView` via `snapshot_for_view` (#4 gave generators views).
    ///
    /// Cached behind `cached_graph_snapshot`, keyed by (target, preset_type,
    /// graph_version, catalog fingerprint) so the rebuild runs once per edit,
    /// not once per content tick. Returns `None` if the target no longer
    /// resolves, the generator has no type, or the type has no JSON preset.
    fn graph_snapshot(
        &mut self,
        target: &manifold_core::GraphTarget,
    ) -> Option<Arc<manifold_renderer::node_graph::GraphSnapshot>> {
        use manifold_core::GraphTarget;
        let fingerprint = self.embedded_presets_fingerprint;
        let project = self.engine.project()?;

        // Resolve the instance's preset type + graph version for the cache key.
        let (preset_type, version) = match target {
            GraphTarget::Effect(eid) => {
                let inst = project.find_effect_by_id(eid)?;
                (inst.effect_type().clone(), inst.graph_version)
            }
            GraphTarget::Generator(lid) => {
                let (_, layer) = project.timeline.find_layer_by_id(lid)?;
                let gp = layer.gen_params()?;
                if gp.generator_type().is_none() {
                    return None;
                }
                (gp.generator_type().clone(), gp.graph_version)
            }
        };

        // Cache hit: identical target / type / version / catalog → clone Arc.
        if let Some(cache) = self.cached_graph_snapshot.as_ref()
            && &cache.target == target
            && cache.preset_type == preset_type
            && cache.version == version
            && cache.fingerprint == fingerprint
        {
            return Some(Arc::clone(&cache.snapshot));
        }

        // Cache miss: rebuild the snapshot for this kind.
        let snap = match target {
            GraphTarget::Effect(eid) => {
                let instance = project.find_effect_by_id(eid)?;
                if let Some(def) = instance.graph.as_ref() {
                    // Per-card override: `from_def` has no live effect, so its
                    // outer_routings come out empty. The compositor's per-type
                    // routings are authoritative — fill them in here.
                    let mut snap = manifold_renderer::node_graph::GraphSnapshot::from_def(def)?;
                    snap.outer_routings = self
                        .content_pipeline
                        .outer_routings_for(instance.effect_type());
                    apply_effective_bound_values(&mut snap, def, instance);
                    snap
                } else {
                    self.content_pipeline
                        .graph_snapshot_for(instance.effect_type())?
                }
            }
            GraphTarget::Generator(lid) => {
                let (_, layer) = project.timeline.find_layer_by_id(lid)?;
                let gen_type = layer.generator_type();
                if let Some(override_def) = layer.generator_graph() {
                    // Per-instance override: exposure state lives on the
                    // layer's graph, so build from the override (grafting
                    // bundled metadata back if edits dropped it) and project
                    // routings from the override def's own metadata — captures
                    // user-added bindings the compositor's type view wouldn't.
                    let mut d = override_def.clone();
                    manifold_renderer::generators::registry::graft_preset_metadata_from_bundle(
                        &mut d, gen_type,
                    );
                    let mut snap = manifold_renderer::node_graph::GraphSnapshot::from_def(&d)?;
                    if let Some(meta) = d.preset_metadata.as_ref() {
                        use manifold_core::effect_graph_def::BindingTarget;
                        use manifold_renderer::node_graph::{OuterParamRouting, OuterParamSource};
                        // Recurse into group bodies (BUG-103): a binding whose
                        // target lives inside a group — the glTF importer's
                        // per-object knobs on `mat_k` nodes inside each object's
                        // box — is dropped by a top-level-only handle map, so a
                        // diverged imported scene loses its group-face rows the
                        // same way the pristine path did. Shared helper so both
                        // arms resolve handles identically.
                        let mut handle_by_id: std::collections::HashMap<&str, &str> =
                            std::collections::HashMap::new();
                        manifold_renderer::node_graph::collect_node_handles(
                            &d.nodes,
                            &mut handle_by_id,
                        );
                        snap.outer_routings = meta
                            .bindings
                            .iter()
                            .filter_map(|b| match &b.target {
                                BindingTarget::Node { node_id, param } => {
                                    let handle = handle_by_id.get(node_id.as_str())?;
                                    Some(OuterParamRouting {
                                        outer_label: b.label.clone(),
                                        outer_param_id: b.id.clone(),
                                        node_handle: handle.to_string(),
                                        inner_param: param.clone(),
                                        source: OuterParamSource::Static,
                                    })
                                }
                                BindingTarget::Composite { .. } => None,
                            })
                            .collect();
                    }
                    if let Some(gp) = layer.gen_params() {
                        apply_effective_bound_values(&mut snap, &d, gp);
                    }
                    snap
                } else {
                    // Pristine layer: the unified LoadedPresetView. Generators
                    // got views in #4, so this mirrors the effect pristine path
                    // — `snapshot_for_view` does `from_def(canonical_def)` +
                    // `outer_routings_from_view`.
                    let view = manifold_renderer::node_graph::loaded_preset_view_by_id(gen_type)?;
                    manifold_renderer::node_graph::snapshot_for_view(view)?
                }
            }
        };

        let arc = Arc::new(snap);
        self.cached_graph_snapshot = Some(CachedGraphSnapshot {
            target: target.clone(),
            preset_type,
            version,
            fingerprint,
            snapshot: Arc::clone(&arc),
        });
        Some(arc)
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
        let osc_sync_mode = self
            .engine
            .project()
            .map_or(OscSyncMode::M4L, |p| p.settings.osc_sync_mode);
        let authority = {
            // MIDI Clock is the timing plane and outranks everything. In
            // AbletonOSC mode the bridge's transport channel claims OSC
            // authority as the FALLBACK tier (ABLETON_TRANSPORT_SYNC_DESIGN
            // D10): the ladder below puts CLK first, so this only takes
            // effect when the clock plane is absent or dies mid-set —
            // degraded-but-moving beats frozen. In M4L mode, timecode
            // claims OSC authority as before.
            let osc_receiving = match osc_sync_mode {
                OscSyncMode::M4L => self.osc_sync.is_receiving_timecode,
                OscSyncMode::AbletonOsc => {
                    self.ableton_bridge.is_transport_receiving(now.0)
                }
            };
            let auto = if self
                .transport_controller
                .midi_clock_sync
                .as_ref()
                .is_some_and(|s| s.is_midi_clock_enabled() && s.is_receiving_clock())
            {
                ClockAuthority::MidiClock
            } else if osc_receiving {
                ClockAuthority::Osc
            } else if self
                .transport_controller
                .link_sync
                .as_ref()
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
            link.update(&mut self.sync_arbiter, &mut self.engine, authority);
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
        // The clock plane is suppressed while a user seek's round trip is in
        // flight (cooldown, M4L path) OR while an AbletonOSC transport
        // command awaits its ack (ABLETON_TRANSPORT_SYNC_DESIGN D5) — in
        // both cases what Ableton emits is known-stale.
        let suppress_clock_plane = self.sync_arbiter.is_seek_cooldown_active(now)
            || self.ableton_bridge.transport_sync_pending();
        if let Some(ref mut clk) = self.transport_controller.midi_clock_sync {
            let snap = SyncTargetSnapshot::from_engine(&self.engine);
            clk.update(
                now,
                &mut self.sync_arbiter,
                &mut self.engine,
                &snap,
                authority,
                suppress_clock_plane,
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

        if let Some(p) = self.engine.project_mut() {
            self.ableton_bridge.apply(p, self.time_since_start.0);
        }

        // OSC timecode sync — M4L mode only. `enable_osc`/`disable_osc` had
        // zero callers anywhere in the app before this fix (F1,
        // CORE_ENGINE_MAP) — is_osc_enabled could never become true, so
        // `update()` below always returned on its first line. Track
        // enablement against the live mode here, mirroring how the
        // AbletonOsc branch above is itself gated on osc_sync_mode. Both
        // calls are idempotent (each checks its own is_osc_enabled first).
        if osc_sync_mode == OscSyncMode::M4L {
            if !self.osc_sync.is_osc_enabled {
                self.osc_sync.enable_osc(&mut self.osc_receiver);
            }
            // Drain the latest timecode message captured by the subscription
            // callback (osc_receiver.update() already dispatched it above,
            // before this block) into on_timecode_received, BEFORE update().
            self.osc_sync.drain_pending_osc_timecode(now);

            let snap = SyncTargetSnapshot::from_engine(&self.engine);
            self.osc_sync.update(
                now,
                &snap,
                &mut self.sync_arbiter,
                &mut self.engine,
                authority,
            );
        } else if self.osc_sync.is_osc_enabled {
            self.osc_sync.disable_osc(Some(&mut self.osc_receiver));
        }

        // AbletonOSC inbound transport relay — closed-loop via the transport
        // state machine (ABLETON_TRANSPORT_SYNC_DESIGN D10). The machine
        // only emits actions for genuine external changes (its own commands
        // are consumed as value-matched acks, which is what killed the
        // 2026-era play/pause oscillation that had this path amputated).
        // CLK keeps priority: while it's receiving, CLK Start/Stop already
        // relays Ableton's buttons, so machine actions apply only when the
        // clock plane is absent — checked HERE, at drain time, not via the
        // frame-start `authority` (which is derived from last frame's
        // is_transport_receiving and is stale precisely on the first
        // message after an idle period — the from-idle play in Ableton
        // would be discarded and D10's primary scenario would fail).
        if osc_sync_mode == OscSyncMode::AbletonOsc {
            let clk_receiving = self
                .transport_controller
                .midi_clock_sync
                .as_ref()
                .is_some_and(|s| s.is_midi_clock_enabled() && s.is_receiving_clock());
            while let Some(action) = self.ableton_bridge.pop_transport_action() {
                if clk_receiving {
                    continue; // CLK owns transport + position; drop stale intents
                }
                use manifold_playback::transport_sync::EngineAction;
                // Source and authority are both Osc: the arbiter gate is
                // satisfied by construction; the real gate is the CLK-liveness
                // check above (the D10 fallback-tier rule).
                match action {
                    EngineAction::Play => {
                        self.sync_arbiter.play(
                            ClockAuthority::Osc,
                            ClockAuthority::Osc,
                            &mut self.engine,
                        );
                    }
                    EngineAction::Pause => {
                        self.sync_arbiter.pause(
                            ClockAuthority::Osc,
                            ClockAuthority::Osc,
                            &mut self.engine,
                            false,
                        );
                    }
                    EngineAction::SeekBeats(beat) => {
                        let time = self
                            .engine
                            .beat_to_timeline_time(Beats::from_f32(beat));
                        self.sync_arbiter.seek(
                            ClockAuthority::Osc,
                            ClockAuthority::Osc,
                            &mut self.engine,
                            time,
                        );
                    }
                    EngineAction::NudgeBeats(beat) => {
                        let time = self
                            .engine
                            .beat_to_timeline_time(Beats::from_f32(beat));
                        self.sync_arbiter.nudge_time(
                            ClockAuthority::Osc,
                            ClockAuthority::Osc,
                            &mut self.engine,
                            time,
                        );
                    }
                }
            }
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
                // MIDI Clock always drives position when active — suppressed
                // during the seek cooldown (user scrub, M4L path) AND while
                // an AbletonOSC transport command awaits its ack
                // (ABLETON_TRANSPORT_SYNC_DESIGN D5). The missing ack gate
                // here was the play-from-cursor drag-back: the cooldown
                // expired on a wall clock while Ableton's clock still
                // reported the old position, and it pulled the playhead back.
                if !self
                    .sync_arbiter
                    .is_seek_cooldown_active(self.time_since_start)
                    && !self.ableton_bridge.transport_sync_pending()
                    && let Some(ref clk) = self.transport_controller.midi_clock_sync
                    && clk.is_midi_clock_enabled()
                    && clk.is_receiving_clock()
                {
                    self.engine
                        .set_beat(Beats::from_f32(clk.current_clock_beat()));
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
                let manifold_beat = self
                    .engine
                    .time_to_timeline_beat(self.engine.current_time())
                    .0;
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

        let should_record =
            self.engine.is_recording() && self.engine.current_state() == PlaybackState::Playing;

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
            && authority != ClockAuthority::Osc
        {
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
            } else if project.tempo_map.point_count() <= 1 && authority == ClockAuthority::Internal
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
                let map_bpm = project
                    .tempo_map
                    .get_bpm_at_beat(Beats::ZERO, project.settings.bpm);
                let q_resolved_bpm = BeatQuantizer::quantize_bpm(bpm);
                if (map_bpm.0 - q_resolved_bpm).abs() >= TempoRecorder::BPM_THRESHOLD {
                    project
                        .tempo_map
                        .add_or_replace_point(Beats::ZERO, Bpm(bpm), source, 0.001);
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

        let default_bpm = self.engine.project().map_or(120.0, |p| p.settings.bpm.0);
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

