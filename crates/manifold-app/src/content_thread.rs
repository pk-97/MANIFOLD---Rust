//! Content thread — runs PlaybackEngine, EditingService, and ContentPipeline
//! on a dedicated thread. Communicates with the UI thread via crossbeam channels.
//!
//! The content thread owns all authoritative state: the engine (which owns the
//! project), the editing service (undo/redo), audio sync, percussion, and the
//! GPU content pipeline (generators + compositor).
use crossbeam_channel::{Receiver, Sender};
use std::sync::Arc;

use manifold_core::change::{ChangeSet, Part};
use manifold_core::types::{ClockAuthority, OscSyncMode, PlaybackState, TempoPointSource};
use manifold_core::{Beats, Bpm, Seconds};
use manifold_core::work_count::{record_work, WorkKind};
use manifold_editing::service::EditingService;
use manifold_playback::clip_launcher::ClipLauncher;
use manifold_playback::engine::{PlaybackEngine, TickContext};
use manifold_playback::external_follow::{ExternalSample, FollowProfile};
use manifold_playback::external_sync::{live_tempo, LinkOffset, LinkTempoGuard, OffsetTake};
use manifold_playback::sync_calibration::{difference_seconds, CalibratedSource, SyncCalibration};
use manifold_playback::midi_input::MidiInputController;
use manifold_playback::osc_receiver::OscReceiver;
use manifold_playback::osc_sender::OscPositionSender;
use manifold_playback::osc_sync::OscSyncController;
use manifold_playback::percussion_orchestrator::PercussionImportOrchestrator;
use manifold_playback::audio_layer_playback::AudioLayerPlayback;
use manifold_playback::sync::{SyncArbiter, SyncTargetSnapshot};
use manifold_playback::sync_projection::{map_sample, seconds_between, song_time_beat_at, timecode_seconds_at};
use manifold_playback::tempo_recorder::TempoRecorder;
use manifold_playback::transport_controller::TransportController;
use manifold_node_engine::gpu::context::GpuContext;

use crate::content_command::ContentCommand;
use crate::content_pipeline::ContentPipeline;
use crate::content_state::{ContentState, StateTx};
use crate::frame_timer::FrameTimer;

/// The follower row for an authority (D6): Link and MIDI clock are fast and
/// clean, AbletonOSC and timecode slow and coarse; Internal follows the map.
fn follow_profile(authority: ClockAuthority) -> Option<FollowProfile> {
    match authority {
        ClockAuthority::Internal => None,
        ClockAuthority::Link | ClockAuthority::MidiClock => Some(FollowProfile::FAST),
        ClockAuthority::Osc => Some(FollowProfile::SLOW),
    }
}

/// Stage presents the content thread issued, each with the deadline of the
/// frame it carries, until Metal reports when it went on screen (D19). A
/// handful covers the frames in flight.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct IssuedPresents {
    ring: [Option<(u64, std::time::Instant)>; 8],
    next: usize,
}

impl IssuedPresents {
    pub(crate) fn record(&mut self, drawable_id: u64, deadline: std::time::Instant) {
        self.ring[self.next] = Some((drawable_id, deadline));
        self.next = (self.next + 1) % self.ring.len();
    }

    /// The deadline of the frame `drawable_id` carried, once.
    pub(crate) fn take(&mut self, drawable_id: u64) -> Option<std::time::Instant> {
        let slot = self.ring.iter_mut().find(|e| e.is_some_and(|(id, _)| id == drawable_id))?;
        slot.take().map(|(_, deadline)| deadline)
    }
}

/// An in-flight "export current frame" request. Captured across two content
/// ticks so the live render never stalls: tick N submits the GPU readback (and
/// fills `dims`), tick N+1 reads the pixels back and hands them to an off-thread
/// encoder. See [`ContentThread::submit_still_export_if_pending`] /
/// [`ContentThread::poll_still_export`].
///
/// The capture can start before the destination is known: Export Frame grabs
/// the frame on screen when clicked, then the save dialog supplies `dest`.
/// Playback keeps running behind the dialog, so capturing afterwards would
/// save whatever is playing when the dialog closes.
pub struct StillExportJob {
    pub dest: Option<(String, manifold_media::still_exporter::StillFormat)>,
    /// Captured dimensions, set once the readback has been submitted. `None`
    /// until then — the poll step skips jobs that haven't been submitted yet.
    pub dims: Option<(u32, u32)>,
    /// Read-back pixels held until `dest` arrives.
    pub pixels: Option<Vec<u8>>,
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
    /// The deadline of the frame this tick shows; Link is read there (D15).
    pub(crate) link_sample_at: std::time::Instant,
    pub last_data_version: u64,
    /// MIDI device input — routes hardware note events to ClipLauncher.
    pub midi_input: MidiInputController,
    /// Bridges MIDI note events to LiveClipManager.
    pub clip_launcher: ClipLauncher,
    /// Content frame timer — target FPS synced from project settings.
    pub timer: FrameTimer,
    /// Physics metrics from the most recently completed live content render.
    /// Owned by the content thread so snapshot construction can read it after
    /// `tick_frame` finishes rendering.
    pub physics_metrics: manifold_node_engine::exec::sim_metrics::SimMetrics,

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
    /// MANIFOLD's beat under Link is Link's beat minus this (D15).
    pub link_offset: LinkOffset,
    /// Whether Link's tempo stands in for MIDI clock's or AbletonOSC's (D21).
    pub tempo_guard: LinkTempoGuard,
    /// The read instant's output latency and the source offsets (D19).
    pub calibration: SyncCalibration,
    /// The `[Transport]` operating log.
    pub sync_log: manifold_playback::sync_log::SyncLog,
    /// The deadline of the frame this tick shows.
    pub(crate) frame_deadline: std::time::Instant,
    /// Stage presents waiting for their presented time.
    pub(crate) issued_presents: IssuedPresents,
    /// A tempo source for tests that stand in for Link or MIDI clock: it
    /// delivers this tempo every tick, where a real source would (D21).
    #[cfg(test)]
    pub(crate) test_tempo_source: Option<(Bpm, TempoPointSource)>,
    /// Per-tick sync rig log state (FRAME_EXACT_TRANSPORT_DESIGN P3, C0).
    pub rig_probe: crate::sync_rig_log::TickProbe,

    // ── LED output ──
    /// LED/ArtNet output controller. None when not initialized.
    pub led_controller: Option<manifold_led::LedOutputController>,
    /// Cached LED composite preview state (LED_STRIPS_DESIGN D24 state table),
    /// published on every tick's `ContentState`. `None` = no controller or
    /// never enabled; `Black` = blackout/disabled (cached frame cleared);
    /// `Frame` = latest completed readback. Updated in the LED send block.
    pub led_preview: crate::content_state::LedPreview,
    /// Version counter for completed readbacks (bumps into `Frame`).
    pub led_preview_version: u64,

    /// Pending single-frame export, if any. Set by `ContentCommand::CaptureStill`
    /// or `ExportFrame` and cleared once the frame has been read back and dispatched to the
    /// off-thread encoder. At most one capture is in flight at a time.
    pub still_export: Option<StillExportJob>,

    // ── MIDI device cache ──
    /// Cached MIDI device names, refreshed every ~2 seconds.
    pub cached_midi_device_names: Vec<String>,
    pub midi_source_discovery: manifold_playback::midi_source_discovery::MidiSourceDiscovery,

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
    pub modifier_preview_context: Option<std::sync::Arc<manifold_node_engine::runtime::ModifierPreviewContext>>,
    /// Whether the node-output preview applies auto-gain/normalization. Off by
    /// default; toggled from the editor's preview pane ("Smart preview"). Pushed
    /// to the pipeline each frame. Node preview only — never affects the live
    /// render.
    pub node_preview_normalize: bool,
    /// Cached editor-canvas snapshot for the watched effect or generator.
    ///
    /// Built lazily by [`crate::graph_snapshot::active_graph`] and invalidated
    /// per the key documented on [`crate::graph_snapshot::CachedGraphSnapshot`].
    /// Holds at most one entry: only one canvas is visible at a time.
    pub cached_graph_snapshot: Option<crate::graph_snapshot::CachedGraphSnapshot>,

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
    /// Last-sent Ableton session; sent every tick, replaced only when the session changes, so the UI can compare by pointer.
    pub last_sent_ableton_session: Option<Arc<manifold_playback::ableton_bridge::AbletonSession>>,

    /// Stamps the reaction pass last reacted to, and what it retains between
    /// passes (docs/UI_INVALIDATION_DESIGN.md D4).
    pub reactions: crate::content_reactions::ReactionState,

    /// D11 undo/redo toast (`UI_CRAFT_AND_MOTION_PLAN.md` P2) — set by
    /// `handle_command`'s `Undo`/`Redo` arms (peeked from
    /// `EditingService::peek_undo_description`/`peek_redo_description` BEFORE
    /// the command moves stacks), consumed by `.take()` at the next
    /// `ContentState` build in the SAME loop iteration (see `content_thread.rs`'s
    /// per-tick state construction). Rides the regular per-tick snapshot rather
    /// than a separate out-of-band send — see `UndoRedoEvent`'s doc comment.
    pub pending_undo_redo_event: Option<crate::content_state::UndoRedoEvent>,
    /// Most recent rejected graph edit, retained until a newer rejection so
    /// every regular snapshot carries the diagnostic to the UI.
    pub graph_edit_diagnostic: Option<Arc<crate::content_state::GraphEditDiagnostic>>,
    /// Most recent content-authoritative selection update for newly created
    /// scene modifiers, retained until a newer update.
    pub edit_selection_update: Option<Arc<crate::edit_selection::EditSelectionUpdate>>,
    pub modifier_selection_update: Option<Arc<crate::content_state::ModifierSelectionUpdate>>,
    pub object_modifier_selection_update:
        Option<Arc<crate::content_state::ObjectModifierSelectionUpdate>>,

    // ── Profiling ──
    /// Active profiling session (only present when feature = "profiling").
    #[cfg(feature = "profiling")]
    pub profiler: Option<manifold_profiler::ProfileSession>,
}

impl ContentThread {
    #[cfg(target_os = "macos")]
    fn handle_command_with_preparation(
        &mut self,
        cmd: ContentCommand,
        cmd_rx: &Receiver<ContentCommand>,
        cmd_tx: &Sender<ContentCommand>,
        state_tx: &StateTx,
    ) -> bool {
        match cmd {
            ContentCommand::LoadProject(project) =>
                self.load_project_and_warmup(project, cmd_rx, cmd_tx, state_tx).warmup.interrupted,
            cmd => self.handle_command(cmd),
        }
    }

    /// Run the content loop. Blocks until Shutdown is received.
    pub fn run(
        mut self,
        cmd_tx: crossbeam_channel::Sender<ContentCommand>,
        cmd_rx: Receiver<ContentCommand>,
        state_tx: StateTx,
    ) {
        log::info!("[ContentThread] started");

        // LED output is NOT auto-initialized. The user enables it via the
        // master-inspector toggle, which sends InitLedOutput / ShutdownLedOutput.

        let mut work_trace = manifold_core::work_count::WorkTrace::new();
        loop {
            work_trace.frame("content");
            // 1. Drain every pending command, coalescing consecutive seeks.
            if self.drain_commands(&cmd_tx, &cmd_rx, &state_tx) {
                return;
            }
            self.publish_snapshot_if_dirty(&state_tx);

            if self.run_paced_frame(&cmd_tx, &cmd_rx, &state_tx) {
                return;
            }
        }
    }

    /// Drains every queued command, coalescing consecutive seeks. During
    /// scrubbing the UI sends a SeekTo/SeekToBeat per mouse move; only the
    /// last seek of a burst matters, so it waits and is overwritten. GPU
    /// events (SurfaceReady, FramePresented) never flush it. Returns true on
    /// shutdown.
    pub(crate) fn drain_commands(
        &mut self,
        cmd_tx: &Sender<ContentCommand>,
        cmd_rx: &Receiver<ContentCommand>,
        state_tx: &StateTx,
    ) -> bool {
        let mut pending_seek: Option<ContentCommand> = None;
        loop {
            match cmd_rx.try_recv() {
                Ok(ContentCommand::StartExport(config)) => {
                    self.engine.clear_automation_previews();
                    // Stop any active live recording before entering export.
                    #[cfg(target_os = "macos")]
                    if let Some(session) = self.content_pipeline.recording_session.take() {
                        log::warn!("[ContentThread] Stopping active recording before export");
                        let _ = session.stop();
                    }
                    // Flush any pending seek before entering export.
                    if let Some(seek) = pending_seek.take() {
                        let _ = self.handle_command(seek);
                    }
                    self.run_export(*config, cmd_rx, state_tx);
                }
                Ok(ContentCommand::LoadProject(project)) => {
                    // Flush any pending seek before the load.
                    if let Some(seek) = pending_seek.take()
                        && self.handle_command(seek)
                    {
                        log::info!("[ContentThread] shutdown received");
                        return true;
                    }
                    let report = self.load_project_and_warmup(project, cmd_rx, cmd_tx, state_tx);
                    if report.warmup.interrupted {
                        return true;
                    }
                }
                Ok(cmd @ ContentCommand::SeekTo(_)) | Ok(cmd @ ContentCommand::SeekToBeat(_)) => {
                    pending_seek = Some(cmd);
                }
                #[cfg(target_os = "macos")]
                Ok(ContentCommand::SurfaceReady) => {}
                Ok(ContentCommand::FramePresented { drawable_id, at }) => self.frame_presented(drawable_id, at),
                Ok(cmd) => {
                    // Flush any pending seek before a non-seek command
                    // to preserve ordering (e.g. Seek then Play).
                    if let Some(seek) = pending_seek.take()
                        && self.handle_command(seek)
                    {
                        log::info!("[ContentThread] shutdown received");
                        return true;
                    }
                    if self.handle_command(cmd) {
                        log::info!("[ContentThread] shutdown received");
                        return true;
                    }
                }
                Err(crossbeam_channel::TryRecvError::Empty) => break,
                Err(crossbeam_channel::TryRecvError::Disconnected) => {
                    log::info!("[ContentThread] command channel disconnected, shutting down");
                    return true;
                }
            }
        }
        // Apply the final coalesced seek (if any).
        if let Some(seek) = pending_seek.take()
            && self.handle_command(seek)
        {
            log::info!("[ContentThread] shutdown received");
            return true;
        }
        false
    }

    /// One production-paced frame shared by the app and headless profiler.
    /// Preserves surface backpressure and the per-frame autorelease pool.
    /// Returns true when command processing requests shutdown.
    pub(crate) fn run_paced_frame(
        &mut self,
        cmd_tx: &Sender<ContentCommand>,
        cmd_rx: &Receiver<ContentCommand>,
        state_tx: &StateTx,
    ) -> bool {
        // Wait for GPU surface, draining commands while waiting.
        // In the common case (99%+) this returns immediately — the GPU
        // finished the surface from 2 frames ago long before now.
        // Under heavy GPU load, keeps processing transport/MIDI/parameter
        // commands instead of busy-spinning. Zero CPU during the wait.
        #[cfg(target_os = "macos")]
        {
            let fence_start = std::time::Instant::now();
            if self.wait_for_surface_draining_commands(cmd_tx, cmd_rx, state_tx) {
                log::info!("[ContentThread] shutdown received during surface wait");
                return true;
            }
            self.content_pipeline
                .set_last_fence_wait_ms(fence_start.elapsed().as_secs_f64() * 1000.0);
        }

        // 2. Wait for next content frame
        self.timer.ensure_thread_policy();

        // Precision frame pacing: block until the next frame deadline.
        // mach_wait_until for the bulk, spin for the final 2ms.
        self.timer.wait_for_deadline();
        // Drain autoreleased ObjC Metal objects at the end of each frame,
        // preventing memory accumulation and random GC-like pauses.
        #[cfg(target_os = "macos")]
        objc2::rc::autoreleasepool(|_| {
            self.tick_frame(state_tx);
        });
        #[cfg(not(target_os = "macos"))]
        self.tick_frame(state_tx);
        false
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
        state_tx: &StateTx,
    ) -> bool {
        // Fast path: surface already ready (99%+ of frames).
        if self.content_pipeline.is_surface_ready(self.frame_count) {
            return false;
        }

        // Slow path: GPU is behind. Register notification — when the GPU
        // signals, SurfaceReady is sent through cmd_tx, waking recv().
        // The fast-exit (`false` = the GPU event already fired) must
        // re-verify the FULL readiness condition: is_surface_ready also
        // covers the bridge read-fence (BUG-xaw4), and the UI's reads may
        // still be in flight even when the content queue's own write is
        // long done. Those retire with a SurfaceReady wake of their own.
        if !self.content_pipeline.register_surface_notify(cmd_tx)
            && self.content_pipeline.is_surface_ready(self.frame_count)
        {
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
            if self.content_pipeline.is_surface_ready(self.frame_count) {
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
                Ok(ContentCommand::FramePresented { drawable_id, at }) => self.frame_presented(drawable_id, at),
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
                    if self.handle_command_with_preparation(cmd, cmd_rx, cmd_tx, state_tx) {
                        return true; // shutdown
                    }
                    // Drain any additional queued commands.
                    while let Ok(cmd) = cmd_rx.try_recv() {
                        match cmd {
                            ContentCommand::SurfaceReady => {}
                            ContentCommand::FramePresented { drawable_id, at } => self.frame_presented(drawable_id, at),
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
                                if self.handle_command_with_preparation(cmd, cmd_rx, cmd_tx, state_tx) {
                                    return true;
                                }
                            }
                        }
                    }
                    self.publish_snapshot_if_dirty(state_tx);
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
        self.publish_snapshot_if_dirty(state_tx);
        false
    }

    /// Execute one content frame: tick engine, render, send state to UI.
    /// Separated from the main loop to allow wrapping in autoreleasepool on macOS.
    /// `pub(crate)` (not private): PERF_BUDGET_GATE_DESIGN.md P1's headless
    /// `perf-soak` xtask drives frames directly (real-time paced via
    /// `self.timer.wait_for_deadline()` between calls, same as `run()`'s own
    /// loop) instead of going through the command channel — no behavior
    /// change to this function itself.
    pub(crate) fn tick_frame(&mut self, state_tx: &StateTx) {
        self.tick_frame_at(std::time::Instant::now(), state_tx);
    }

    /// `tick_frame` with the tick's instant injected, so a test can drive the
    /// real content thread through a chosen press phase, jitter and stalls
    /// (docs/FRAME_EXACT_TRANSPORT_DESIGN.md section 3.4). Only the timer's
    /// tick reads `now`; wall telemetry stays on the real clock (D13).
    pub(crate) fn tick_frame_at(&mut self, now: std::time::Instant, state_tx: &StateTx) {
        let _frame_start = std::time::Instant::now();
        // Reactions to commands handled outside a drain loop (tests, the
        // headless harness); a no-op when the drain already reacted.
        self.run_reactions();
        let frames = self.timer.consume_frames_at(now).frames;
        // Sources are read for the moment this frame reaches the screen (D19).
        self.frame_deadline = self.timer.tick_deadline().unwrap_or(now);
        self.link_sample_at = self.frame_deadline + std::time::Duration::from_secs_f64(self.calibration.read_delay());
        let realtime = self.timer.realtime_since_start();
        self.time_since_start = Seconds(realtime);

        // Cold-touch detector transport flag: the content thread is the only
        // thread that knows whether the audience is currently seeing frames.
        manifold_core::cold_touch::set_transport_playing(self.engine.is_playing());

        // Read back a still-frame export submitted on the previous tick (if any).
        // Runs before this frame's render so the GPU completion we wait on is the
        // capture's, not a newer frame's. The encode itself runs off-thread.
        #[cfg(target_os = "macos")]
        self.poll_still_export(state_tx);

        // Discovery can block inside CoreMIDI. Only receive completed name
        // snapshots here; the worker owns enumeration and its refresh cadence.
        if let Some(names) = self.midi_source_discovery.poll() {
            self.cached_midi_device_names = names;
        }

        #[cfg(feature = "profiling")]
        let _prelude_ms = _frame_start.elapsed().as_secs_f64() * 1000.0;

        // 3. Process MIDI input (before engine tick — matches Unity Update() ordering).
        // Drains hardware note events and routes them to ClipLauncher → LiveClipManager.
        #[cfg(feature = "profiling")]
        let _t0 = std::time::Instant::now();

        self.engine
            .tick_midi_input(&mut self.midi_input, &mut self.clip_launcher, realtime);
        // A NoteOff's recorded take (and a NoteOn's missing layer) commits
        // here, before the tick syncs clips, as one undo step each (D13).
        let live_commits = self.engine.take_live_commits();
        self.execute_engine_commits(live_commits);
        // Notes held for a layer that now exists play on this same tick.
        if self.midi_input.has_replay() {
            self.engine
                .tick_midi_input(&mut self.midi_input, &mut self.clip_launcher, realtime);
            let live_commits = self.engine.take_live_commits();
            self.execute_engine_commits(live_commits);
        }

        #[cfg(feature = "profiling")]
        let _midi_input_ms = _t0.elapsed().as_secs_f64() * 1000.0;

        // 3b. Sync controller updates (before engine tick — Unity execution order -100).
        // Link, MidiClock, and OSC poll their sources and issue gated transport
        // commands via SyncArbiter. Snapshot read-only state before mutable borrows.
        #[cfg(feature = "profiling")]
        let _t0 = std::time::Instant::now();

        // The tick audit (D24, INV-22): from here through `engine.tick`, the
        // project changes only in Values, the audited tick writes and what
        // the gateway stamps.
        let tick_stamps = self.editing_service.stamps();
        let tick_window = self.engine.project().map(manifold_editing::edit::tick_audit::open);

        self.tick_sync_controllers(frames);

        // 3c. External beat derivation + tempo recording/resolution.
        // Port of C# PlaybackController.Update lines 1064-1099.
        // Must run AFTER sync controllers (which set live external tempo)
        // and BEFORE engine.tick() (which uses the derived beat).
        let authority = self
            .engine
            .project()
            .map_or(ClockAuthority::Internal, |p| p.settings.clock_authority);
        self.hand_external_sample(authority);
        self.learn_source_offsets();
        self.update_recording_session_state(authority);
        self.apply_resolved_tempo(authority);

        #[cfg(feature = "profiling")]
        let _sync_controllers_ms = _t0.elapsed().as_secs_f64() * 1000.0;

        // 3d. Audio modulation: reconcile capture against the project (on change)
        // and feed the engine the latest per-send feature snapshot. Must run
        // BEFORE engine.tick() (the modulation pipeline reads the snapshot).
        self.audio_mod_runtime.update(
            &mut self.engine,
            self.editing_service.stamps(),
            self.audio_layer_playback.as_mut(),
        );

        // 4. Tick engine
        #[cfg(feature = "profiling")]
        let _t0 = std::time::Instant::now();

        let ctx = TickContext {
            frames,
            realtime_now: Seconds(realtime),
            frame_count: self.frame_count,
        };
        let mut tick_result = self.engine.tick(ctx);
        if let (Some(window), Some(p)) = (tick_window, self.engine.project()) {
            let gateway = self.editing_service.stamps().moved_since(&tick_stamps);
            manifold_editing::edit::tick_audit::close(window, p, gateway);
        }
        self.rig_probe_tick(now, frames, authority);
        self.log_sync(authority);
        // The engine's clock follows the project rate (D10); the tick grid
        // follows the clock, so the two never run at different rates.
        let clock_fps = f64::from(self.engine.frame_rate());
        if self.timer.target_fps() != clock_fps {
            self.timer.set_target_fps(clock_fps);
        }

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
            // (CORE_ENGINE_MAP section 13.12). Clear it each frame in AbletonOSC
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
        // following the transport. See docs/AUDIO_LAYER_DESIGN.md section 4.
        if let Some(ref mut audio_layer_playback) = self.audio_layer_playback
            && let Some(project) = self.engine.project()
        {
            audio_layer_playback.update(project, &self.engine);
        }

        // 6. Percussion tick
        self.tick_percussion();

        // 6a2. Commit any automation recording gestures that finished this
        // tick (section 5) — one undo entry per gesture, built by
        // `crate::automation::evaluate_all_automation`'s gesture-closure
        // pass inside `self.engine.tick()` above. Mirrors the percussion
        // tick just above: `&mut Project` + `&mut EditingService` handed to
        // the command synchronously, on the same content-thread frame.
        let gesture_commits = std::mem::take(&mut tick_result.pending_gesture_commits);
        self.execute_engine_commits(gesture_commits);
        let structural_commits = std::mem::take(&mut tick_result.pending_structural_commits);
        self.execute_engine_commits(structural_commits);

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
            Some(target @ manifold_core::GraphTarget::SceneModifier { .. }) => {
                match target.host_target() {
                    Some(manifold_core::GraphTarget::Generator(lid)) =>
                        (None, Some((lid.clone(), self.preview_graph_node.clone()))),
                    _ => (None, None),
                }
            }
            None => (None, None),
        };
        self.content_pipeline
            .set_node_preview_request(effect_preview);
        self.content_pipeline
            .set_node_preview_generator(generator_preview);
        self.content_pipeline.set_modifier_preview_context(
            matches!(self.watched_graph_target, Some(manifold_core::GraphTarget::SceneModifier { .. })),
            self.modifier_preview_context.clone(),
        );
        self.content_pipeline
            .set_node_preview_normalize(self.node_preview_normalize);

        // Physics metrics cover the live content render only: anything
        // recorded since the last frame (warmup) is dropped here, and
        // thumbnails never reach the drained slots.
        self.content_pipeline.take_sim_metrics(&mut self.engine);
        let render_work_start = std::time::Instant::now();
        self.content_pipeline.render_content(
            &self.gpu,
            &mut self.engine,
            &tick_result,
            self.frame_count,
            false,
            self.editing_service.stamps(),
            Some(self.audio_mod_runtime.visuals()),
        );
        if let Some(id) = self.content_pipeline.take_issued_present() {
            self.issued_presents.record(id, self.frame_deadline);
        }
        self.physics_metrics = self.content_pipeline.take_sim_metrics(&mut self.engine);
        let render_work_ms = render_work_start.elapsed().as_secs_f64() * 1000.0;
        self.content_pipeline.set_last_render_work_ms(render_work_ms);

        // ── Live RT capture (env MANIFOLD_RT_CAPTURE, requires perf-soak) ──
        // Arms RT_CAPTURE_ARM flags before render, drains queue + logs stats after.
        #[cfg(all(feature = "perf-soak", target_os = "macos"))]
        if std::env::var_os("MANIFOLD_RT_CAPTURE").is_some() && self.frame_count > 30 {
            let sample_tick = self.frame_count.is_multiple_of(30);
            let png_dump_tick = self.frame_count.is_multiple_of(300);
            if sample_tick || png_dump_tick {
                crate::rt_capture::arm_capture();
            }
            // Drain and log after render.
            if let Some(dev) = self.content_pipeline.native_device() {
                let stats = crate::rt_capture::drain_capture_stats(dev);
                if !stats.is_empty() {
                    let beat = self.engine.current_beat_f64();
                    let playing = self.engine.is_playing();
                    let log_dir = std::path::PathBuf::from("/tmp/rt_live");
                    let _ = std::fs::create_dir_all(&log_dir);
                    // Write stats log.
                    use std::io::Write;
                    let mut line = format!(
                        "tick={} play={} beat={:.3}", self.frame_count, playing, beat,
                    );
                    for (label, (hit, luma, sd)) in &stats {
                        line.push_str(&format!(" {}=({:.6},{:.6},{:.6})", label, hit, luma, sd));
                    }
                    line.push('\n');
                    if let Ok(mut f) = std::fs::OpenOptions::new()
                        .create(true).append(true).open(log_dir.join("stats.log"))
                    {
                        let _ = f.write_all(line.as_bytes());
                    }
                    log::info!("[rt-capture] tick={} stats logged", self.frame_count);
                }
            }
        }

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
        // The completed readback also feeds `self.led_preview` (D24's state
        // table): a new Frame per completed readback, Black on blackout or
        // disabled (which drops the cached frame), None with no controller.
        if let Some(ref mut led) = self.led_controller {
            let native_device = self.content_pipeline.native_device().unwrap();
            let (brightness, led_gain) = self
                .engine
                .project()
                .map_or((1.0, 1.0), |p| (p.settings.led_brightness, p.settings.led_gain));
            if !led.is_enabled() {
                // Disabled: the strips were blacked out when the toggle went
                // off; the preview shows a black band and the cached frame is
                // dropped so a stale chase can never animate it (D24).
                self.led_preview = crate::content_state::LedPreview::Black;
            } else if let Some(source) = self.content_pipeline.led_source_texture() {
                // Poll previous frame's readback (send DMX if ready).
                // Only when we still have an LED source — when transitioning
                // to blackout we deliberately skip the poll so a stale
                // completion can't briefly flash the prior frame on the LEDs.
                // A completed readback becomes the new preview Frame; the Arc
                // is the readback's own buffer handed up through the poll
                // (no clone, no extra allocation).
                if let Some(pixels) = led.poll_readback() {
                    self.led_preview_version += 1;
                    self.led_preview = crate::content_state::LedPreview::Frame {
                        pixels,
                        version: self.led_preview_version,
                    };
                }
                // No completed readback this tick (GPU still in flight) —
                // keep the last Frame; the channel coalesces to latest.
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
                self.led_preview = crate::content_state::LedPreview::Black;
            }
        } else {
            self.led_preview = crate::content_state::LedPreview::None;
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
            let s = manifold_node_engine::runtime::chain_dispatch::take_chain_dispatch_stats();
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

        // Capture frame metadata now, but finish its timing after publishing
        // state so both ends of the content tick remain visible to the profiler.
        #[cfg(feature = "profiling")]
        let profile_capture_start = std::time::Instant::now();
        #[cfg(feature = "profiling")]
        let frame_record = if self.profiler.as_ref().is_some_and(|p| p.is_recording()) {
            let frame_wall_ms = _frame_start.elapsed().as_secs_f64() * 1000.0;
            let current_beat = self.engine.current_beat();
            let time_sig = self
                .engine
                .project()
                .map_or(4, |p| p.settings.time_signature_numerator.max(1));
            let bar = (current_beat.as_f32() / time_sig as f32).floor() as u32;
            let budget_ms = 1000.0 / self.timer.target_fps();
            let active_layers = tick_result
                .ready_clips
                .iter()
                .enumerate()
                .filter(|(i, clip)| {
                    !tick_result.ready_clips[..*i]
                        .iter()
                        .any(|previous| previous.layer_index == clip.layer_index)
                })
                .count();

            // Normal captures do not collect pass-level native Metal samples.
            // Unavailable must remain distinct from measured zero GPU work.
            let gpu_pass_count = None;
            let gpu_total_ms = None;
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
                        r.as_any().downcast_ref::<manifold_compositor::generator_renderer::GeneratorRenderer>()
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

            // Configuration snapshot of enabled effects; not execution attribution.
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

            Some(manifold_profiler::FrameRecord {
                index: self.frame_count - 1,
                beat: current_beat.as_f32(),
                bar,
                wall_time_ms: frame_wall_ms,
                budget_exceeded: frame_wall_ms > budget_ms,
                content_thread: manifold_profiler::ContentTimings {
                    total_ms: frame_wall_ms,
                    prelude_ms: _prelude_ms,
                    midi_input_ms: _midi_input_ms,
                    sync_controllers_ms: _sync_controllers_ms,
                    engine_tick_ms: _engine_tick_ms,
                    render_content_ms: _render_content_ms,
                    gpu_poll_ms: _gpu_poll_ms,
                    cleanup_ms: _cleanup_ms,
                    state_publish_ms: 0.0,
                },
                gpu_passes,
                active_clips: active_clip_info,
                active_effects,
                active_layer_count: active_layers,
                gpu_pass_count,
                gpu_total_ms,
                layer_states,
                missed_frames: self.timer.missed_ticks(),
                pacing: self.timer.last_wall_interval().map(|interval| {
                    let interval_ms = interval * 1000.0;
                    let target_interval_ms = 1000.0 / self.timer.target_fps();
                    manifold_profiler::FramePacing {
                        interval_ms,
                        target_interval_ms,
                        deadline_lateness_ms: (interval_ms - target_interval_ms).max(0.0),
                    }
                }),
                profiler_overhead_ms: 0.0,
                memory: manifold_profiler::MemorySnapshot {
                    estimated_texture_bytes: estimated_tex_bytes,
                    render_target_count: rt_count,
                },
            })
        } else {
            None
        };
        #[cfg(feature = "profiling")]
        let profiler_overhead_ms = profile_capture_start.elapsed().as_secs_f64() * 1000.0;

        // 8. Push state to UI
        #[cfg(feature = "profiling")]
        let state_publish_start = std::time::Instant::now();
        self.engine.reclaim_tick_result(tick_result);
        self.send_state(state_tx);
        #[cfg(feature = "profiling")]
        if let Some(mut record) = frame_record {
            record.content_thread.state_publish_ms =
                state_publish_start.elapsed().as_secs_f64() * 1000.0;
            record.profiler_overhead_ms = profiler_overhead_ms;
            record.wall_time_ms =
                (_frame_start.elapsed().as_secs_f64() * 1000.0 - profiler_overhead_ms).max(0.0);
            record.content_thread.total_ms = record.wall_time_ms;
            record.budget_exceeded = record.wall_time_ms > 1000.0 / self.timer.target_fps();
            if let Some(profiler) = self.profiler.as_mut() {
                profiler.record_frame(record);
            }
        }
    }

    /// Build and send a `ContentState` snapshot to the UI. Gated by
    /// `data_version` for the heavy `Arc<Project>` clone; lightweight
    /// modulation/transport fields are sent every call.
    fn send_state(&mut self, state_tx: &StateTx) {
        let version = self.editing_service.data_version();
        // D23: today's rule (every edit, undo, redo, record, load) plus a
        // content-originated write outside the live scalars.
        let version_changed = version != self.last_data_version || self.reactions.publish_pending;
        if version_changed {
            self.last_data_version = version;
            self.reactions.publish_pending = false;
        }
        // Send a project snapshot when data_version changes (editing commands).
        // Value-only writers (LFO/envelope/Ableton/OSC/automation) never bump
        // data_version — those ride the ModulationSnapshot, which is now sent
        // EVERY tick (see below), so no writer class can leave the UI stale.

        // Arc<Project> snapshot: only deep-clone when data_version changes.
        // Per-frame param values ride the lightweight ModulationSnapshot
        // instead (just param_values Vec<f32> clones — no full Project clone).
        let snapshot = if version_changed {
            // Structural change — create a new Arc with a fresh clone.
            let arc = self
                .engine
                .project()
                .map(|p| {
                    record_work(WorkKind::ProjectClone);
                    std::sync::Arc::new(p.clone())
                });
            self.cached_project_snapshot = arc.clone();
            arc
        } else {
            None
        };

        // Build the lightweight modulation snapshot EVERY tick, not only when
        // drivers/envelopes/Ableton are active: OSC-router writes, automation
        // lane sampling, and MutateProject (Values) drags all move param_values
        // without flagging `modulation_active`, and gating the send on that
        // flag left the UI showing stale values until the next structural
        // snapshot. The capture is a linear walk over the param manifests into
        // a reusable scratch buffer (zero-alloc steady state), then one clone
        // of the flat buffer. Blocks are id-keyed at capture, so a structural
        // edit landing between capture and the UI's apply can't misroute
        // values (see ModulationSnapshot).
        // D15: the capture compares as it writes; the buffer is cloned and
        // `values_seq` moves only when a live value changed.
        let modulation_snapshot = if let Some(project) = self.engine.project()
            && self.mod_scratch.capture_into(project)
        {
            self.reactions.values_seq += 1;
            record_work(WorkKind::ModSnapshotCopy);
            Some(self.mod_scratch.clone())
        } else {
            None
        };
        // Sent every tick as the retained Arc: a coalesced UI drain keeps only
        // the latest state, so a one-shot Some would be lost.
        if self.ableton_bridge.session_changed() {
            self.reactions.runtime.ableton_session += 1;
            self.last_sent_ableton_session = Some(Arc::new(self.ableton_bridge.session().clone()));
        }
        let ableton_session = self.last_sent_ableton_session.clone();
        let runtime = self.track_runtime_seqs();

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

        // The editor canvas's graph snapshot, rebuilt only when its inputs
        // move (`graph_snapshot.rs`).
        let (active_graph_target, active_graph_snapshot_arc) = crate::graph_snapshot::active_graph(
            &mut self.cached_graph_snapshot,
            self.engine.project(),
            self.watched_graph_target.as_ref(),
            self.editing_service.stamps().get(Part::EmbeddedPresets),
        );

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
            physics_cpu_ms: self.physics_metrics.cpu_ms,
            physics_body_count: self.physics_metrics.body_count,
            physics_backlog_seconds: self.physics_metrics.backlog_seconds,
            sim_step_cap_hit: self.physics_metrics.sim_step_cap_hit,
            sim_nonfinite: self.physics_metrics.sim_nonfinite,
            active_clips: self.engine.active_clip_count(),
            data_version: version,
            editing_is_dirty: self.editing_service.is_dirty(),
            stamps: self.editing_service.stamps(),
            values_seq: self.reactions.values_seq,
            audio_directory_version: self.audio_mod_runtime.directory_version(),
            runtime,
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
            sdr_preview: Some(self.content_pipeline.sdr_preview()),
            body: true,
            rt_quality: self
                .engine
                .project()
                .map_or(manifold_core::settings::RtQualitySettings::default(), |p| {
                    p.settings.rt_quality
                }),
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
            led_preview: Some(self.led_preview.clone()),
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
            export_status: {
                // One shared empty status: building it per tick allocated.
                static EMPTY: std::sync::LazyLock<Arc<str>> = std::sync::LazyLock::new(|| Arc::from(""));
                EMPTY.clone()
            },
            export_finished: Vec::new(),
            export_run_finished: false,
            warmup: None,
            undo_redo_event: self.pending_undo_redo_event.take(),
            graph_edit_diagnostic: self.graph_edit_diagnostic.clone(),
            trigger_delivery_failure: self.engine.trigger_delivery_failure(),
            edit_selection_update: self.edit_selection_update.clone(),
            modifier_selection_update: self.modifier_selection_update.clone(),
            object_modifier_selection_update: self.object_modifier_selection_update.clone(),
            ableton_session,
            ableton_connected: self.ableton_bridge.is_connected(),
            ableton_transport_enabled: self.ableton_bridge.is_transport_enabled(),
            ableton_sync_status: self.ableton_bridge.transport_sync_status(),
            osc_sync_mode: self
                .engine
                .project()
                .map_or(OscSyncMode::M4L, |p| p.settings.osc_sync_mode),
            project_snapshot: snapshot,
            modulation_snapshot,
            active_graph_target,
            active_graph_snapshot: active_graph_snapshot_arc,
            node_preview_info: self.content_pipeline.node_preview_info(),
            scene_viewport_frames: self.content_pipeline.scene_viewport_frames().clone(),
            live_node_params: self.content_pipeline.live_node_params(),
            node_atlas_layout: self.content_pipeline.node_atlas_layout().to_vec(),
            clip_atlas_layout: self.content_pipeline.clip_atlas_layout().clone(),
            automation_latched_params: self
                .engine
                .automation_latches()
                .keys()
                .cloned()
                .collect(),
            automation_armed: self.engine.automation_armed(),
            sync_offsets: self.calibration.offsets(),
            chain_failures: self.reactions.chain_failures.clone(),
        };

        state_tx.publish(state);
    }

    /// Send a `ContentState` snapshot only if the project has mutated since
    /// the last publish. Used outside the render tick so the UI confirms
    /// edits while the GPU is behind. Coalesces: one publish per drain pass.
    fn publish_snapshot_if_dirty(&mut self, state_tx: &StateTx) {
        // The drain pass is over: react once to everything it moved (D4).
        self.run_reactions();
        if self.editing_service.data_version() != self.last_data_version
            || self.reactions.publish_pending
        {
            self.send_state(state_tx);
        }
    }

    /// Tick all sync controllers once per frame. Called before engine tick.
    /// Handles the borrow-split problem: snapshot read-only engine state first,
    /// then pass &mut engine for transport commands via SyncArbiter.
    /// `frames`: what this tick advances, so a Link offset is taken against
    /// the frame it shows (D15).
    fn tick_sync_controllers(&mut self, frames: u32) {
        let now = self.time_since_start;
        // Live tempo lasts one tick: only a source that delivers a tempo this
        // tick sets it (D21), so it can never outlive its source.
        self.engine
            .set_live_external_tempo(false, Bpm::DEFAULT, TempoPointSource::Unknown);
        #[cfg(test)]
        if let Some((bpm, source)) = self.test_tempo_source {
            self.engine.set_live_external_tempo(true, bpm, source);
        }

        // Auto-determine clock authority BEFORE sync controllers run.
        // Uses previous frame's receiving/peer state (updated by sync controllers
        // last frame). This ensures the SyncArbiter gates are consistent with
        // the authority — prevents one-frame mismatch where samples or
        // transport commands are incorrectly rejected.
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
            let previous = self.engine.project().map_or(ClockAuthority::Internal, |p| p.settings.clock_authority);
            manifold_playback::transport_controller::write_clock_authority(&mut self.engine, auto);
            // An authority change swaps the engine's converter before any
            // source acts on it this tick (D6, D8). The second test catches a
            // change an export deferred and a project loaded with a saved
            // external authority.
            let profile = follow_profile(auto);
            if auto != previous || self.engine.follows_external() != profile.is_some() {
                self.engine.set_follow_authority(profile);
            }
            // Link becoming the authority while MANIFOLD plays (D15).
            if auto != previous && auto == ClockAuthority::Link && self.engine.is_playing() {
                self.link_offset.link_became_authority();
            }
            auto
        };

        // Link sync — poll beat/phase/tempo from Ableton Link network.
        if let Some(ref mut link) = self.transport_controller.link_sync {
            link.update(&mut self.sync_arbiter, &mut self.engine, authority, self.link_sample_at);
            if link.is_link_enabled() {
                // A Link-initiated start lines MANIFOLD's bar up with Link's;
                // every due offset is taken here, at the read instant,
                // against the beat of the frame this tick shows (D15).
                if link.started_playing() && authority == ClockAuthority::Link {
                    self.link_offset.request(OffsetTake::Quantum);
                }
                if self.link_offset.is_due() {
                    let frame_beat = self.engine.upcoming_frame_beat(frames).0;
                    self.link_offset.take(link.current_beat.0, frame_beat, link.quantum());
                }
            }
        }
        let link_bpm = self
            .transport_controller
            .link_sync
            .as_ref()
            .filter(|l| l.is_link_enabled() && l.has_active_peers())
            .map(|l| l.link_tempo);

        // MIDI Clock sync — poll clock/SPP from midir.
        // Snapshot SyncTarget state before passing &mut engine as SyncArbiterTarget.
        // The clock plane is suppressed while a user seek's round trip is in
        // flight (cooldown, M4L path) OR while an AbletonOSC transport
        // command awaits its ack (ABLETON_TRANSPORT_SYNC_DESIGN D5) — in
        // both cases what Ableton emits is known-stale.
        let suppress_clock_plane = self.sync_arbiter.is_seek_cooldown_active(now)
            || self.ableton_bridge.transport_sync_pending();
        if let Some(ref mut clk) = self.transport_controller.midi_clock_sync {
            // With the guard latched the tick filter runs on Link's period
            // and filters phase only (D20, D21).
            let latched = authority == ClockAuthority::MidiClock && self.tempo_guard.latched();
            clk.set_link_tempo(link_bpm.filter(|_| latched));
            let snap = SyncTargetSnapshot::from_engine(&self.engine);
            clk.update(
                now,
                &mut self.sync_arbiter,
                &mut self.engine,
                &snap,
                authority,
                suppress_clock_plane,
            );
        }

        // OSC receiver — drain queued UDP messages and dispatch to subscribers.
        self.osc_receiver.update();

        // OSC parameter router — apply any pending param writes from OSC messages.
        if self.osc_param_router.has_pending()
            && let Some(edit) = self.engine.project_edit()
        {
            let router = &self.osc_param_router;
            self.editing_service.mutate(ChangeSet::of(&[Part::Values]), edit, |p| router.apply(p));
        }

        // Ableton bridge — drain AbletonOSC replies and apply macro values.
        self.ableton_bridge.update(self.time_since_start.0);

        // When discovery just completed, validate mappings and force a full
        // project snapshot so the UI receives updated [ABL]/[ABL-]/[ABL?] statuses.
        // Validation is a Mappings write: the reaction pass rebuilds the
        // listeners once, and the write publishes the new statuses (D23).
        // A fresh session also changes the Live-side param ranges the write
        // targets cache, so the listeners rebuild even when no status moved.
        if self.ableton_bridge.take_validation_dirty() {
            self.reactions.session_discovered = true;
            if self.engine.project().is_some_and(|p| self.ableton_bridge.validation_would_change(p))
                && let Some(edit) = self.engine.project_edit()
            {
                let bridge = &self.ableton_bridge;
                self.editing_service.mutate(ChangeSet::of(&[Part::Mappings]), edit, |p| bridge.validate_mappings(p));
                self.reactions.publish_pending = true;
            }
            self.run_reactions();
        }

        if self.ableton_bridge.has_values_to_apply()
            && let Some(edit) = self.engine.project_edit()
        {
            let bridge = &mut self.ableton_bridge;
            let now = self.time_since_start.0;
            self.editing_service.mutate(ChangeSet::of(&[Part::Values]), edit, |p| {
                bridge.apply(p, now);
            });
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
                }
            }
        }

        self.update_tempo_plane(authority, osc_sync_mode, link_bpm);
    }

    /// The guard compares Link with the authority's own tempo, and the live
    /// tempo reports the authority's base tempo (D21). After every source
    /// has updated this tick.
    fn update_tempo_plane(&mut self, authority: ClockAuthority, mode: OscSyncMode, link_bpm: Option<f64>) {
        let now = self.time_since_start.0;
        let clk = self
            .transport_controller
            .midi_clock_sync
            .as_ref()
            .filter(|c| c.is_midi_clock_enabled() && c.is_receiving_clock());
        let listener = (mode == OscSyncMode::AbletonOsc && self.ableton_bridge.is_transport_receiving(now))
            .then(|| f64::from(self.ableton_bridge.ableton_tempo()));
        let link_beat = self.transport_controller.link_sync.as_ref().map(|l| l.current_beat.0);
        // The guard watches whenever Link and a comparable source are both
        // live, so it is already latched when that source takes over.
        match (link_bpm, link_beat, clk, listener) {
            (Some(bpm), Some(beat_at_r), Some(clk), _) => {
                if let Some((tick, arrival)) = clk.last_landed_tick() {
                    // Link's beat when the tick arrived: Link runs at its
                    // session tempo between the arrival and the read instant.
                    let back = clk.seconds(self.link_sample_at) - arrival;
                    self.tempo_guard.observe_clock(now, arrival, tick, beat_at_r - back * bpm / 60.0);
                }
            }
            (Some(bpm), _, None, Some(listener)) => self.tempo_guard.observe_listener(now, bpm, listener),
            _ => self.tempo_guard.release(),
        }

        let own = match authority {
            ClockAuthority::MidiClock => clk.map(|c| (f64::from(c.current_clock_bpm()), TempoPointSource::MidiClock)),
            ClockAuthority::Osc => listener.map(|bpm| (bpm, TempoPointSource::AbletonOsc)),
            _ => None,
        };
        if let Some((bpm, source)) = live_tempo(authority, link_bpm, own, self.tempo_guard.latched()) {
            self.engine.set_live_external_tempo(true, Bpm(bpm as f32), source);
        }
    }

    /// Link's session tempo while the guard lets it stand in for the
    /// authority's own (D21).
    fn latched_link_tempo(&self) -> Option<f64> {
        self.transport_controller
            .link_sync
            .as_ref()
            .filter(|l| self.tempo_guard.latched() && l.is_link_enabled() && l.has_active_peers())
            .map(|l| l.link_tempo)
    }

    /// Sync rig log records for this tick; reads only, never steers playback.
    fn rig_probe_tick(&mut self, wake: std::time::Instant, frames: u32, authority: ClockAuthority) {
        if !manifold_playback::rig_log::enabled() {
            return;
        }
        let link = self
            .transport_controller
            .link_sync
            .as_ref()
            .filter(|l| l.is_link_enabled())
            .map(|l| (l.current_beat.0, l.link_tempo, l.num_peers));
        let clk = self
            .transport_controller
            .midi_clock_sync
            .as_ref()
            .filter(|c| c.is_midi_clock_enabled() && c.is_receiving_clock())
            .map(|c| (c.current_clock_beat(), c.current_clock_bpm()));
        let osc = self.ableton_bridge.observed_song_time().map(|(beats, at)| {
            (beats, self.time_since_start.0 - at, self.ableton_bridge.ableton_tempo())
        });
        let facts = crate::sync_rig_log::TickFacts {
            frame: self.frame_count,
            wake,
            deadline: self.timer.tick_deadline(),
            frames,
            index: self.engine.transport_frame(),
            beat: self.engine.current_beat_f64(),
            authority,
            playing: self.engine.is_playing(),
            link,
            link_offset: self.link_offset.get().unwrap_or(f64::NAN),
            clk,
            osc,
        };
        self.rig_probe.after_tick(&facts);
    }

    // ═══════════════════════════════════════════════════════════════
    // Phase 1 — External beat derivation
    // Port of C# PlaybackController.Update lines 1064-1096.
    // ═══════════════════════════════════════════════════════════════

    /// This tick's sample from the authority's own source, after every
    /// transport action the sources took this tick (D7): a placement drops
    /// the sample, so it is handed over last. Today's gates stay in front.
    fn hand_external_sample(&mut self, authority: ClockAuthority) {
        if self.engine.current_state() != PlaybackState::Playing || !self.engine.follows_external() {
            return;
        }
        let sample = match authority {
            ClockAuthority::Link => {
                // Block while MANIFOLD owns transport (prevents brief
                // authority-falls-to-Link glitches during MIDI Clock gaps).
                // Link's beat is exact at the read instant, less the offset;
                // none until an offset is taken (D15).
                if !self.sync_arbiter.manifold_owns_playback
                    && let Some(ref link) = self.transport_controller.link_sync
                    && link.is_link_enabled()
                    && link.has_active_peers()
                {
                    self.link_offset.beat(link.current_beat.0).map(|beat| ExternalSample {
                        beat,
                        bpm: link.link_tempo,
                        fresh: true,
                        exact: true,
                    })
                } else {
                    None
                }
            }
            ClockAuthority::MidiClock => {
                // Suppressed during the seek cooldown (user scrub, M4L path)
                // AND while an AbletonOSC transport command awaits its ack
                // (ABLETON_TRANSPORT_SYNC_DESIGN D5): what Ableton's clock
                // reports then is the old position.
                if !self.sync_arbiter.is_seek_cooldown_active(self.time_since_start)
                    && !self.ableton_bridge.transport_sync_pending()
                {
                    self.clk_sample(self.source_read_at(CalibratedSource::MidiClock))
                } else {
                    None
                }
            }
            ClockAuthority::Osc => match self.engine.project().map_or(OscSyncMode::M4L, |p| p.settings.osc_sync_mode) {
                OscSyncMode::M4L => self.timecode_sample(self.source_read_at(CalibratedSource::Timecode)),
                OscSyncMode::AbletonOsc => self.ableton_osc_sample(self.source_read_at(CalibratedSource::AbletonOsc)),
            },
            ClockAuthority::Internal => None,
        };
        if let Some(sample) = sample {
            self.engine.set_external_sample(sample);
        }
    }

    /// A non-Link source is read at the read instant plus the offset it
    /// showed against Link (D19).
    fn source_read_at(&self, source: CalibratedSource) -> std::time::Instant {
        let offset = self.calibration.offset(source);
        let magnitude = std::time::Duration::from_secs_f64(offset.abs());
        if offset >= 0.0 { self.link_sample_at + magnitude } else { self.link_sample_at - magnitude }
    }

    /// The tick filter's beat at `r` (D20), while the clock is live.
    fn clk_sample(&self, r: std::time::Instant) -> Option<ExternalSample> {
        let clk = self
            .transport_controller
            .midi_clock_sync
            .as_ref()
            .filter(|c| c.is_midi_clock_enabled() && c.is_receiving_clock())?;
        clk.sample_at(r).map(|s| ExternalSample { beat: s.beat, bpm: s.bpm, fresh: true, exact: false })
    }

    /// M4L timecode: the latest message's song seconds projected from its
    /// arrival to `r`, through the map, at the map's tempo over the next frame
    /// (D7). Every tick while timecode arrives (D6: one sample per tick).
    fn timecode_sample(&mut self, r: std::time::Instant) -> Option<ExternalSample> {
        let tc = self.osc_sync.timecode_report()?;
        let s = timecode_seconds_at(tc.seconds, seconds_between(tc.at, r), tc.frame);
        let step = 1.0 / f64::from(self.engine.frame_rate());
        let engine = &mut self.engine;
        let (beat, bpm) = map_sample(s, step, |s| engine.time_to_timeline_beat(Seconds(s)).0);
        Some(ExternalSample { beat, bpm, fresh: true, exact: false })
    }

    /// The relay's song time projected to `r` at Live's tempo in force (D7).
    /// No one-way delay: the relay's round trip measures command to
    /// acknowledgement, not the wire; a constant is the calibration's to
    /// learn (D19). The tempo is Link's while the guard is latched (D21).
    fn ableton_osc_sample(&self, r: std::time::Instant) -> Option<ExternalSample> {
        let (report, tempo) = self.ableton_bridge.follower_song_time(self.time_since_start.0)?;
        let beat = song_time_beat_at(report.beat, seconds_between(report.at, r), tempo.before, tempo.bpm, seconds_between(tempo.at, r));
        let bpm = self.latched_link_tempo().unwrap_or(tempo.bpm);
        Some(ExternalSample { beat, bpm, fresh: true, exact: false })
    }

    /// Metal reported a stage frame on screen: its delay after the deadline
    /// it was rendered for feeds the screen delay (D19). Never a transport
    /// boundary, so it never flushes a coalesced seek.
    pub(crate) fn frame_presented(&mut self, drawable_id: u64, at: std::time::Instant) {
        if let Some(deadline) = self.issued_presents.take(drawable_id) {
            let delay = manifold_playback::sync_projection::seconds_between(deadline, at);
            self.calibration.presented(delay, self.time_since_start.0);
        }
    }

    /// The operating log's view of this tick, after the engine's tick.
    fn log_sync(&mut self, authority: ClockAuthority) {
        let mode = self.engine.project().map_or(OscSyncMode::M4L, |p| p.settings.osc_sync_mode);
        let (name, calibrated) = match (authority, mode) {
            (ClockAuthority::Link, _) => (Some("Link"), None),
            (ClockAuthority::MidiClock, _) => (Some("CLK"), Some(CalibratedSource::MidiClock)),
            (ClockAuthority::Osc, OscSyncMode::AbletonOsc) => (Some("OSC"), Some(CalibratedSource::AbletonOsc)),
            (ClockAuthority::Osc, OscSyncMode::M4L) => (Some("timecode"), Some(CalibratedSource::Timecode)),
            (ClockAuthority::Internal, _) => (None, None),
        };
        let tick = manifold_playback::sync_log::SyncTick {
            now: self.time_since_start.0,
            source: name.filter(|_| self.engine.follows_external()),
            playing: self.engine.is_playing(),
            follower: self.engine.follower(),
            event: self.engine.follow_event(),
            calibration: &self.calibration,
            offset: calibrated.map(|s| self.calibration.offset(s)),
        };
        self.sync_log.observe(&tick);
    }

    /// While Link and another source both deliver, each source's offset
    /// against Link is learned from the two reads at the same read instant
    /// (D19). Steady play only: playing, no catch-up window open.
    fn learn_source_offsets(&mut self) {
        if !self.engine.is_playing() {
            return;
        }
        let Some((link_beat, bpm, quantum)) = self
            .transport_controller
            .link_sync
            .as_ref()
            .filter(|l| l.is_link_enabled() && l.has_active_peers())
            .map(|l| (l.current_beat.0, l.link_tempo, l.quantum()))
        else {
            return;
        };
        let steady = !self.engine.follower().catch_up_open();
        let now = self.time_since_start.0;
        let mode = self.engine.project().map_or(OscSyncMode::M4L, |p| p.settings.osc_sync_mode);
        for source in CalibratedSource::ALL {
            let r = self.source_read_at(source);
            let sample = match (source, mode) {
                (CalibratedSource::MidiClock, _) => self.clk_sample(r),
                (CalibratedSource::AbletonOsc, OscSyncMode::AbletonOsc) => self.ableton_osc_sample(r),
                (CalibratedSource::Timecode, OscSyncMode::M4L) => self.timecode_sample(r),
                _ => None,
            };
            if let Some(sample) = sample {
                let difference = difference_seconds(link_beat, sample.beat, bpm, quantum);
                self.calibration.observe_offset(source, now, difference, steady);
            }
        }
    }

    // ═══════════════════════════════════════════════════════════════
    // Phase 2 — Tempo recording / resolution
    // Port of C# PlaybackController.UpdateRecordingSessionState
    // and PlaybackController.ApplyResolvedTempo.
    // ═══════════════════════════════════════════════════════════════

    /// Arm/disarm the tempo recording session based on transport state.
    /// Starting writes nothing; ending is the session-end Settings write.
    /// Port of C# PlaybackController.UpdateRecordingSessionState lines 1098.
    fn update_recording_session_state(&mut self, authority: ClockAuthority) {
        let should_record = self.engine.is_recording()
            && self.engine.current_state() == PlaybackState::Playing
            && authority != ClockAuthority::Osc;
        let active = self.tempo_recorder.is_session_active();
        if should_record && !active {
            self.tempo_recorder.start_session();
        } else if !should_record && active {
            self.end_tempo_recording_session();
        }
    }

    /// Apply resolved external tempo to tempo map (recording) or global BPM (non-recording).
    /// Every write here is an audited tick write (D24).
    /// Port of C# PlaybackController.ApplyResolvedTempo lines 1099.
    fn apply_resolved_tempo(&mut self, authority: ClockAuthority) {
        use manifold_editing::edit::{TickField, tick_write};
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
        if authority == ClockAuthority::Osc {
            return;
        }

        // A recorded tempo point pairs the frame's time with its exact beat.
        let current_beat = self.engine.frame_beat().as_f32();
        let current_time = self.engine.current_time().as_f32();

        let mut tempo_map_changed = false;
        if should_record {
            // Studio recording: append tempo automation points over time.
            // Port of C# ApplyResolvedTempo lines 1117-1122.
            let recorder = &mut self.tempo_recorder;
            if let Some(edit) = self.engine.project_edit() {
                tick_write(TickField::SessionTempoPoints, edit, |p| {
                    tempo_map_changed =
                        recorder.try_record_tempo_point(&mut p.tempo_map, current_beat, current_time, bpm, source);
                });
            }
            if tempo_map_changed && let Some(edit) = self.engine.project_edit() {
                let recorder = &mut self.tempo_recorder;
                tick_write(TickField::RecordingProvenance, edit, |p| {
                    recorder.append_tempo_change(&mut p.recording_provenance, current_time, current_beat, bpm, source);
                });
            }
        }
        // Outside a recording session nothing writes the map: under Internal
        // the map is the truth (FRAME_EXACT_TRANSPORT_DESIGN D8, D21).

        if tempo_map_changed {
            // Time is kept; the frame's beat comes from the new map.
            self.engine.rederive_beat();
        }
    }

    /// End the tempo recording session if active (Pause, Stop, or recording
    /// switched off while playing). The recorded lane is one Settings write
    /// (D24): the UI receives it, and the tempo reconcile skips it, since the
    /// session's points are what the playhead already followed.
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

        // A tempo edit already waiting in this pass reconciles against its
        // own retained map first.
        self.run_reactions();
        if let Some(edit) = self.engine.project_edit() {
            let recorder = &mut self.tempo_recorder;
            self.editing_service.mutate(ChangeSet::of(&[Part::Settings]), edit, |p| {
                recorder.end_session_if_active(
                    &mut p.recording_provenance,
                    &mut p.tempo_map,
                    default_bpm,
                    &get_source_at_beat,
                );
            });
            self.reactions.tempo_retained = None;
            self.reactions.publish_pending = true;
        } else {
            self.tempo_recorder.reset();
        }
    }

    /// Execute commands the engine handed back (automation gestures, live
    /// takes, missing layers) through the gateway, one undo step each.
    fn execute_engine_commits(&mut self, commands: Vec<Box<dyn manifold_editing::command::Command>>) {
        if commands.is_empty() {
            return;
        }
        if let Some(mut edit) = self.engine.project_edit() {
            for cmd in commands {
                self.editing_service.execute(cmd, edit.reborrow());
            }
        }
    }

    /// Drive percussion detection. Only a finished result writes, and it
    /// writes through the gateway: its commands go to history, and the
    /// clip's cached analysis and any trimmed or renamed lanes are declared.
    fn tick_percussion(&mut self) {
        let now = self.time_since_start.as_f32();
        if !self.percussion_orchestrator.result_ready() {
            self.percussion_orchestrator.tick(now, None);
            return;
        }
        let Some(edit) = self.engine.project_edit() else { return };
        let orchestrator = &mut self.percussion_orchestrator;
        self.editing_service.mutate_with_history(edit, |p, editing| {
            let before = DetectionWrite::observe(p);
            orchestrator.tick(now, Some((p, editing)));
            ((), before.touches(p))
        });
        self.reactions.publish_pending = true;
    }

    /// Re-place or clear a clip's detected triggers through the gateway.
    pub(crate) fn rewrite_clip_triggers(&mut self, clip_id: manifold_core::ClipId, clear: bool) {
        let Some(edit) = self.engine.project_edit() else { return };
        let orchestrator = &mut self.percussion_orchestrator;
        self.editing_service.mutate_with_history(edit, |p, editing| {
            let before = DetectionWrite::observe(p);
            if clear {
                orchestrator.clear_clip_triggers(clip_id, p, editing);
            } else {
                orchestrator.replan_clip(clip_id, p, editing);
            }
            ((), before.touches(p))
        });
        self.reactions.publish_pending = true;
    }
}

/// What a detection pass changes outside its recorded commands: the clip's
/// cached analysis and routing, overlap trims and removals (Clips, always),
/// and lane names (Layers, only when one moved). Name sums are order-free so
/// observing allocates nothing.
struct DetectionWrite {
    names: u64,
}

impl DetectionWrite {
    fn observe(p: &manifold_core::project::Project) -> Self {
        let hasher = ahash::RandomState::with_seeds(5, 6, 7, 8);
        let names = p.timeline.layers.iter().map(|l| hasher.hash_one(&l.name)).fold(0u64, u64::wrapping_add);
        Self { names }
    }

    fn touches(&self, p: &manifold_core::project::Project) -> ChangeSet {
        if Self::observe(p).names != self.names {
            ChangeSet::of(&[Part::Clips, Part::Layers])
        } else {
            ChangeSet::of(&[Part::Clips])
        }
    }
}

#[cfg(test)]
mod tests {
    use manifold_core::project::Project;

    use crate::content_command::ContentCommand;
    use crate::headless_harness::headless_content_thread;

    // The run loop publishes after each command drain, before the paced
    // frame, so the UI confirms edits even while the GPU is behind.

    fn one_layer(layer_type: manifold_core::types::LayerType) -> Project {
        let mut project = Project::default();
        let mut layer = manifold_core::layer::Layer::new("Pad".into(), layer_type, 0);
        layer.midi_note = 60;
        layer.midi_channel = -1;
        project.timeline.layers.push(layer);
        project.timeline.reindex_layers();
        project
    }

    fn last_snapshot(rx: &crossbeam_channel::Receiver<crate::content_state::ContentState>) -> Option<std::sync::Arc<Project>> {
        rx.try_iter().filter_map(|s| s.project_snapshot).last()
    }

    /// BUG-wfs20 (Stale Link tempo after peers vanish writes beat-0 tempo
    /// point), FRAME_EXACT_TRANSPORT_DESIGN P3 test 10: Link plays at 128
    /// over a one-point map at 120; when its peers vanish, authority falls to
    /// Internal and the map must be untouched (D21).
    #[test]
    fn peers_vanishing_keeps_the_beat_and_the_map() {
        use manifold_core::types::{ClockAuthority, TempoPointSource};
        use manifold_core::{Beats, Bpm};
        use manifold_playback::link_sync::{LinkSyncController, SimulatedLink};
        let mut project = Project::default();
        project.settings.bpm = Bpm(120.0);
        project.tempo_map.add_or_replace_point(Beats::ZERO, Bpm(120.0), TempoPointSource::Unknown, 0.001);
        let mut thread = headless_content_thread(project, 64, 64);
        thread.timer.set_frame_clocked(true);
        let (state_tx, _state_rx) = crate::content_state::state_channel();
        let mut link = LinkSyncController::new();
        let session = SimulatedLink { at: std::time::Instant::now(), beat: 0.0, tempo: 128.0, peers: 1, playing: false };
        link.simulate(session);
        thread.transport_controller.link_sync = Some(link);
        thread.handle_command(ContentCommand::Play);
        for _ in 0..300 {
            thread.tick_frame(&state_tx);
        }
        assert_eq!(thread.engine.project().unwrap().settings.clock_authority, ClockAuthority::Link);

        thread.transport_controller.link_sync.as_mut().unwrap().simulate(SimulatedLink { peers: 0, ..session });
        for _ in 0..60 {
            thread.tick_frame(&state_tx);
        }
        let p = thread.engine.project().unwrap();
        assert_eq!(p.settings.clock_authority, ClockAuthority::Internal);
        assert_eq!(p.tempo_map.point_count(), 1, "peers vanishing must not add tempo points");
        assert_eq!(
            p.tempo_map.get_bpm_at_beat_immut(Beats::ZERO, Bpm(0.0)),
            Bpm(120.0),
            "the stale Link tempo must not be written into the beat-0 point"
        );
    }

    /// BUG-0u1al (Link-initiated Play never caches the Link beat offset),
    /// FRAME_EXACT_TRANSPORT_DESIGN P3 test 9: after a Stop, MANIFOLD sits at
    /// beat 10 and Link starts playing at its beat 37.3. The offset rounds to
    /// Link's quantum, so MANIFOLD's bar lines up with Link's, and Link then
    /// steers for a minute within Link's 1 ms bound (D15).
    #[test]
    fn link_started_play_aligns_to_the_bar_and_steers() {
        use manifold_core::types::{ClockAuthority, TempoPointSource};
        use manifold_core::{Beats, Bpm};
        use manifold_playback::link_sync::{LinkSyncController, SimulatedLink};
        use std::time::Duration;
        let mut project = Project::default();
        project.settings.bpm = Bpm(120.0);
        project.tempo_map.add_or_replace_point(Beats::ZERO, Bpm(120.0), TempoPointSource::Unknown, 0.001);
        let mut thread = headless_content_thread(project, 64, 64);
        let (state_tx, _state_rx) = crate::content_state::state_channel();
        let step = Duration::from_secs_f64(1.0 / 60.0);
        let anchor = std::time::Instant::now() + step;
        let at = |k: u32| anchor + step * k + step.mul_f64(0.3);
        // Link's beat is 37.3 when it starts playing, 600 frames in.
        let session = SimulatedLink { at: anchor, beat: 37.3 - 10.0 * 2.0, tempo: 120.0, peers: 1, playing: false };
        let mut link = LinkSyncController::new();
        link.simulate(session);
        thread.transport_controller.link_sync = Some(link);
        thread.handle_command(ContentCommand::Stop);
        thread.handle_command(ContentCommand::SeekToBeat(Beats(10.0)));
        let mut k = 0;
        while k < 600 {
            thread.tick_frame_at(at(k), &state_tx);
            k += 1;
        }
        assert!(!thread.engine.is_playing(), "MANIFOLD waits at beat 10");
        thread.transport_controller.link_sync.as_mut().unwrap().simulate(SimulatedLink { playing: true, ..session });
        let mut worst_ms: f64 = 0.0;
        for _ in 0..3_600 {
            thread.tick_frame_at(at(k), &state_tx);
            k += 1;
            assert_eq!(thread.engine.project().unwrap().settings.clock_authority, ClockAuthority::Link);
            if k < 600 + 300 {
                continue;
            }
            assert!(thread.engine.is_playing(), "Link's play started MANIFOLD");
            let link_beat = session.beat_at(thread.link_sample_at);
            let diff = link_beat - thread.engine.frame_beat().0;
            let folded = (diff + 2.0).rem_euclid(4.0) - 2.0;
            worst_ms = worst_ms.max(folded.abs() * 60.0 / 120.0 * 1000.0);
        }
        assert!(worst_ms <= 1.0, "MANIFOLD's bar is {worst_ms:.2} ms off Link's (mod 4 beats)");
    }

    /// BUG-mmct0.5 (MIDI recording commits bypass EditingService): a recorded
    /// take commits through the gateway, so the UI receives it and Cmd+Z
    /// removes it.
    #[test]
    fn midi_take_reaches_the_ui_and_undoes() {
        use manifold_core::change::Part;
        use manifold_playback::midi_input::{MidiNoteEvent, MidiNoteEventType};
        let mut thread = headless_content_thread(one_layer(manifold_core::types::LayerType::Trigger), 64, 64);
        thread.timer.set_frame_clocked(true);
        let (state_tx, state_rx) = crate::content_state::state_channel();
        thread.handle_command(ContentCommand::SetRecording(true));
        thread.handle_command(ContentCommand::Play);
        let note = |event_type, sequence| MidiNoteEvent {
            event_type, note: 60, velocity: 1.0, channel: 0, source_index: 0, absolute_tick: -1, sequence,
        };
        thread.midi_input.inject_note(note(MidiNoteEventType::NoteOn, 1));
        for _ in 0..30 {
            thread.tick_frame(&state_tx);
        }
        let _ = last_snapshot(&state_rx);
        let before = thread.editing_service.stamps();
        thread.midi_input.inject_note(note(MidiNoteEventType::NoteOff, 2));
        thread.tick_frame(&state_tx);
        assert!(thread.editing_service.stamps().moved_since(&before).contains(Part::Clips), "the take moves the Clips stamp");
        let snap = last_snapshot(&state_rx).expect("the take publishes");
        assert_eq!(snap.timeline.layers[0].clips.len(), 1, "the UI receives the take");

        thread.handle_command(ContentCommand::Undo);
        thread.tick_frame(&state_tx);
        let snap = last_snapshot(&state_rx).expect("the undo publishes");
        assert!(snap.timeline.layers[0].clips.is_empty(), "Cmd+Z removes the take");
    }

    /// BUG-7f4d (percussion import outside EditingService): clearing a clip's
    /// detected triggers writes through the gateway, so the UI receives it.
    #[test]
    fn clear_detected_triggers_reaches_the_ui() {
        use manifold_core::change::Part;
        let mut project = one_layer(manifold_core::types::LayerType::Generator);
        let source = manifold_core::ClipId::from("audio-source");
        let mut trigger = manifold_core::clip::TimelineClip::new_trigger(manifold_core::Beats(0.0), manifold_core::Beats(1.0));
        trigger.detection_source = Some(source.clone());
        trigger.layer_id = project.timeline.layers[0].layer_id.clone();
        project.timeline.layers[0].clips.push(trigger);
        let mut thread = headless_content_thread(project, 64, 64);
        let (state_tx, state_rx) = crate::content_state::state_channel();
        thread.tick_frame(&state_tx);
        let _ = last_snapshot(&state_rx);
        let before = thread.editing_service.stamps();
        thread.handle_command(ContentCommand::ClearClipTriggers(source));
        thread.tick_frame(&state_tx);
        assert!(thread.editing_service.stamps().moved_since(&before).contains(Part::Clips));
        let snap = last_snapshot(&state_rx).expect("the clear publishes");
        assert!(snap.timeline.layers[0].clips.is_empty());
        thread.handle_command(ContentCommand::Undo);
        thread.tick_frame(&state_tx);
        assert_eq!(last_snapshot(&state_rx).expect("the undo publishes").timeline.layers[0].clips.len(), 1);
    }

    #[test]
    fn mutation_publishes_snapshot_without_a_tick() {
        let mut thread = headless_content_thread(Project::default(), 1280, 720);
        let (state_tx, state_rx) = crate::content_state::state_channel();
        assert!(!thread.handle_command(ContentCommand::SetProject));
        thread.publish_snapshot_if_dirty(&state_tx);
        let state = state_rx.try_recv().expect("snapshot published");
        assert!(state.data_version > 0, "expected data_version to bump, got {}", state.data_version);
    }

    #[test]
    fn sdr_preview_publishes_runtime_snapshot_without_a_tick() {
        let mut thread = headless_content_thread(Project::default(), 64, 64);
        let (state_tx, state_rx) = crate::content_state::state_channel();
        assert!(!thread.handle_command(ContentCommand::SetSdrPreview(true)));
        thread.publish_snapshot_if_dirty(&state_tx);
        let state = state_rx.try_recv().expect("runtime preview snapshot published");
        assert_eq!(state.sdr_preview, Some(true));
        assert_eq!(state.data_version, 0, "preview must not create an edit");
        assert!(!state.editing_is_dirty, "preview must not dirty the project");
    }

    /// FRAME_EXACT_TRANSPORT_DESIGN D10: the project setting is the only fps.
    /// The engine clock follows it on the next tick and the content timer
    /// follows the clock: after a load, a frame-rate edit, its undo and redo.
    #[test]
    fn timer_and_clock_follow_project_fps() {
        use manifold_editing::commands::settings::ChangeFrameRateCommand;
        let mut thread = headless_content_thread(Project::default(), 64, 64);
        let (state_tx, _state_rx) = crate::content_state::state_channel();
        let tick_and_check = |thread: &mut super::ContentThread, fps: f32, after: &str| {
            let give_up = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while !thread.content_pipeline.is_surface_ready(thread.frame_count) {
                assert!(std::time::Instant::now() < give_up, "GPU surface not ready after 5 s");
                std::thread::yield_now();
            }
            thread.tick_frame_at(std::time::Instant::now(), &state_tx);
            assert_eq!(thread.engine.frame_rate(), fps, "engine clock after {after}");
            assert_eq!(thread.timer.target_fps(), f64::from(fps), "content timer after {after}");
        };
        let mut project = Project::default();
        project.settings.frame_rate = 24.0;
        assert!(!thread.handle_command(ContentCommand::LoadProject(Box::new(project))));
        tick_and_check(&mut thread, 24.0, "load");
        let edit = ChangeFrameRateCommand::new(24.0, 30.0);
        assert!(!thread.handle_command(ContentCommand::Execute(Box::new(edit))));
        assert!(!thread.handle_command(ContentCommand::SetFrameRate(30.0)));
        tick_and_check(&mut thread, 30.0, "the frame-rate edit");
        assert!(!thread.handle_command(ContentCommand::Undo));
        tick_and_check(&mut thread, 24.0, "undo");
        assert!(!thread.handle_command(ContentCommand::Redo));
        tick_and_check(&mut thread, 30.0, "redo");
    }

    /// Metal's presents arrive between a scrub's seeks: they feed the screen
    /// delay without flushing the coalesced seek, so a burst is one seek.
    #[test]
    fn presents_inside_a_scrub_burst_leave_one_seek() {
        let mut thread = headless_content_thread(Project::default(), 64, 64);
        let (cmd_tx, cmd_rx) = crossbeam_channel::unbounded();
        let (state_tx, _state_rx) = crate::content_state::state_channel();
        let epoch = thread.engine.transport_epoch();
        let now = std::time::Instant::now();
        for (id, seconds) in [(1u64, 1.0), (2, 2.0), (3, 3.0)] {
            cmd_tx.send(ContentCommand::SeekTo(manifold_core::Seconds(seconds))).expect("send");
            thread.issued_presents.record(id, now);
            let at = now + std::time::Duration::from_millis(25);
            cmd_tx.send(ContentCommand::FramePresented { drawable_id: id, at }).expect("send");
        }
        assert!(!thread.drain_commands(&cmd_tx, &cmd_rx, &state_tx), "no shutdown");
        assert_eq!(thread.engine.transport_epoch(), epoch.wrapping_add(1), "one seek for the burst");
        assert!((thread.engine.current_time().0 - 3.0).abs() < 1.0 / 60.0, "the burst's last seek wins");
        assert!(thread.issued_presents.take(1).is_none(), "the presents reached the calibration");
    }
}
