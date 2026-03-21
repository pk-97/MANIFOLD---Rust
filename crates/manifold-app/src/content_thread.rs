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

    // ── Tempo recording (port of C# PlaybackController fields) ──
    /// Tempo recording/provenance — tracks external tempo for tempo automation.
    /// Port of C# PlaybackController.tempoRecorder field.
    pub tempo_recorder: TempoRecorder,
    /// Offset between Link's absolute beat epoch and MANIFOLD's timeline beat 0.
    /// Cached ONLY at Play()/Seek() sync points. NOT refreshed periodically —
    /// Link's cumulative beat counter keeps the offset valid across BPM changes.
    /// Port of C# PlaybackController.linkBeatOffset field (line 74).
    pub link_beat_offset: f64,
}

impl ContentThread {
    /// Run the content loop. Blocks until Shutdown is received.
    pub fn run(
        mut self,
        cmd_rx: Receiver<ContentCommand>,
        state_tx: Sender<ContentState>,
    ) {
        log::info!("[ContentThread] started");

        loop {
            // 1. Drain ALL pending commands
            loop {
                match cmd_rx.try_recv() {
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
                // then spin for the final sub-ms to avoid macOS sleep overshoot (~1-2ms).
                let remaining = self.timer.time_until_next_tick();
                if remaining > std::time::Duration::from_millis(2) {
                    // Sleep for most of the remaining time, leaving 1.5ms margin for spin-wait
                    std::thread::sleep(remaining - std::time::Duration::from_micros(1500));
                } else if remaining > std::time::Duration::from_micros(100) {
                    // Close to deadline — yield to OS scheduler instead of sleeping
                    std::thread::yield_now();
                }
                // Below 100μs: fall through to re-check should_tick() immediately
                continue;
            }
            let dt = self.timer.consume_tick();
            let realtime = self.timer.realtime_since_start();
            self.time_since_start = realtime as f32;

            // 3. Process MIDI input (before engine tick — matches Unity Update() ordering).
            // Drains hardware note events and routes them to ClipLauncher → LiveClipManager.
            self.engine.tick_midi_input(
                &mut self.midi_input,
                &mut self.clip_launcher,
                realtime,
            );

            // 3b. Sync controller updates (before engine tick — Unity execution order -100).
            // Link, MidiClock, and OSC poll their sources and issue gated transport
            // commands via SyncArbiter. Snapshot read-only state before mutable borrows.
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

            // 4. Tick engine
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
            if let Some(ref mut stem_audio) = self.stem_audio {
                if let Some(ref audio_sync) = self.audio_sync {
                    stem_audio.update_sync(audio_sync, &self.engine);
                }
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

            // 7. Render content
            self.content_pipeline.render_content(
                &self.gpu, &mut self.engine, &tick_result, dt, self.frame_count,
            );
            self.frame_count += 1;

            // 8. Push state to UI
            let version = self.editing_service.data_version();
            let version_changed = version != self.last_data_version;
            if version_changed {
                self.last_data_version = version;
            }
            // Send a project snapshot when data_version changes (editing commands)
            // OR when modulation is active (LFO/envelope writes to param_values
            // without bumping data_version — UI needs live modulated values).
            let snapshot = if version_changed || tick_result.modulation_active {
                self.engine.project().map(|p| Box::new(p.clone()))
            } else {
                None
            };

            let perc_msg = self.percussion_orchestrator.status_message().to_string();
            let perc_progress = self.percussion_orchestrator.status_progress01();
            let perc_show = self.percussion_orchestrator.show_progress_bar() && !perc_msg.is_empty();

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
                    .map_or(false, |s| s.is_link_enabled()),
                link_tempo: self.transport_controller.link_sync.as_ref()
                    .map_or(120.0, |s| s.link_tempo),
                link_peers: self.transport_controller.link_sync.as_ref()
                    .map_or(0, |s| s.num_peers),
                link_is_playing: self.transport_controller.link_sync.as_ref()
                    .map_or(false, |s| s.link_is_playing),
                midi_clock_enabled: self.transport_controller.midi_clock_sync.as_ref()
                    .map_or(false, |s| s.is_midi_clock_enabled()),
                midi_clock_bpm: self.transport_controller.midi_clock_sync.as_ref()
                    .map_or(120.0, |s| s.current_clock_bpm()),
                midi_clock_position_display: self.transport_controller.midi_clock_sync.as_ref()
                    .map_or_else(String::new, |s| s.current_position_display().to_string()),
                midi_clock_receiving: self.transport_controller.midi_clock_sync.as_ref()
                    .map_or(false, |s| s.is_receiving_clock()),
                osc_sender_enabled: self.transport_controller.osc_sender_enabled,
                osc_receiving_timecode: self.osc_sync.is_receiving_timecode,
                osc_timecode_display: self.osc_sync.current_timecode_display.clone(),
                stem_expanded: self.stem_audio.as_ref().map_or(false, |s| s.is_expanded()),
                stem_ready: self.stem_audio.as_ref().map_or(false, |s| s.stems_ready()),
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
                percussion_status_message: perc_msg,
                percussion_progress: if perc_progress < 0.0 { 0.0 } else { perc_progress.clamp(0.0, 1.0) },
                percussion_show_progress: perc_show,
                project_snapshot: snapshot,
            };

            // Non-blocking send — if the UI is behind, drop the oldest state.
            let _ = state_tx.try_send(state);
        }
    }

    /// Tick all sync controllers once per frame. Called before engine tick.
    /// Handles the borrow-split problem: snapshot read-only engine state first,
    /// then pass &mut engine for transport commands via SyncArbiter.
    fn tick_sync_controllers(&mut self) {
        let authority = self.engine.project()
            .map_or(ClockAuthority::Internal, |p| p.settings.clock_authority);
        let now = self.time_since_start;

        // Link sync — poll beat/phase/tempo from Ableton Link network.
        if let Some(ref mut link) = self.transport_controller.link_sync {
            link.update(
                &mut self.sync_arbiter,
                &mut self.engine,
                authority,
            );
            // Feed live Link tempo to engine for UI readout.
            if link.is_link_enabled() && link.has_active_peers() {
                self.engine.set_live_external_tempo(
                    true,
                    link.link_tempo as f32,
                    TempoPointSource::Link,
                );
            }
        }

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
            // Feed live MIDI Clock BPM to engine.
            if clk.is_midi_clock_enabled() && clk.is_receiving_clock() {
                self.engine.set_live_external_tempo(
                    true,
                    clk.current_clock_bpm(),
                    TempoPointSource::MidiClock,
                );
            }
        }

        // OSC receiver — drain queued UDP messages and dispatch to subscribers.
        self.osc_receiver.update();

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
                if let Some(ref link) = self.transport_controller.link_sync {
                    if link.is_link_enabled()
                        && link.has_active_peers()
                        && !self.link_beat_offset.is_nan()
                    {
                        self.engine
                            .set_beat((link.current_beat - self.link_beat_offset) as f32);
                    }
                }
            }
            ClockAuthority::MidiClock => {
                if !self.sync_arbiter.manifold_owns_playback {
                    if let Some(ref clk) = self.transport_controller.midi_clock_sync {
                        if clk.is_midi_clock_enabled() && clk.is_receiving_clock() {
                            self.engine.set_beat(clk.current_clock_beat());
                        }
                        // else: beat derived from time (engine handles this in advance_time)
                    }
                }
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

        if let Some(project) = self.engine.project_mut() {
            if authority != ClockAuthority::Osc {
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
                } else if project.tempo_map.point_count() <= 1 {
                    // No automation lane authored yet: treat tempo as a global master value.
                    // Compare quantized values so raw float jitter doesn't trigger writes.
                    // Port of C# ApplyResolvedTempo lines 1127-1134.
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
                {
                    if let Some(ref clk) = self.transport_controller.midi_clock_sync {
                        if clk.is_midi_clock_enabled() {
                            let midi_beat = clk.current_clock_beat();
                            self.engine.set_beat(midi_beat);
                            let time = self.engine.beat_to_timeline_time(midi_beat);
                            self.engine.set_time(time.max(0.0) as f64);
                        }
                    }
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
                    {
                        if let Some(ref clk) = self.transport_controller.midi_clock_sync {
                            if clk.is_midi_clock_enabled() {
                                let midi_beat = clk.current_clock_beat();
                                self.engine.set_beat(midi_beat);
                                let time = self.engine.beat_to_timeline_time(midi_beat);
                                self.engine.set_time(time.max(0.0) as f64);
                            }
                        }
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
            }
            ContentCommand::ExecuteBatch(cmds, desc) => {
                if let Some(p) = self.engine.project_mut() {
                    self.editing_service.execute_batch(cmds, desc, p);
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
                    self.editing_service.undo(p);
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
                            &self.gpu.device, &mut self.engine,
                            post_w as u32, post_h as u32,
                        );
                    }
                    if (post_fps - pre_fps).abs() > 0.01 {
                        self.timer.set_target_fps(post_fps as f64);
                    }
                }
            }
            ContentCommand::Redo => {
                // Same pre/post settings detection as Undo.
                let pre = self.engine.project().map(|p| {
                    (p.settings.output_width, p.settings.output_height, p.settings.frame_rate)
                });
                if let Some(p) = self.engine.project_mut() {
                    self.editing_service.redo(p);
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
                            &self.gpu.device, &mut self.engine,
                            post_w as u32, post_h as u32,
                        );
                    }
                    if (post_fps - pre_fps).abs() > 0.01 {
                        self.timer.set_target_fps(post_fps as f64);
                    }
                }
            }
            ContentCommand::SetProject => {
                self.editing_service.set_project();
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
                    self.content_pipeline.resize(&self.gpu.device, &mut self.engine, w, h);
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
            }
            ContentCommand::NewProject(project) => {
                self.link_beat_offset = f64::NAN;
                self.tempo_recorder.reset();
                self.engine.initialize(*project);
                self.editing_service.set_project();
                // Sync frame timer to new project's frame rate.
                if let Some(p) = self.engine.project() {
                    self.timer.set_target_fps(p.settings.frame_rate as f64);
                }
                // Update MIDI mapping config for new project.
                if let Some(p) = self.engine.project() {
                    self.midi_input.set_midi_config(p.midi_config.clone());
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
                self.content_pipeline.resize(&self.gpu.device, &mut self.engine, w, h);
            }

            // ── Transport/sync ─────────────────────────────────────
            ContentCommand::CycleClockAuthority => {
                self.transport_controller.cycle_authority(&mut self.engine);
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
            ContentCommand::ResetBpm => {
                TransportController::reset_bpm(
                    &mut self.engine, &mut self.editing_service,
                );
            }

            // ── Audio ──────────────────────────────────────────────
            ContentCommand::AudioLoaded { preloaded, waveform: _ } => {
                if let Some(ref mut audio_sync) = self.audio_sync {
                    if let Err(e) = audio_sync.apply_preloaded(preloaded) {
                        log::warn!("[ContentThread] Failed to apply loaded audio: {}", e);
                    }
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
                    stem.apply_preloaded_stems(preloaded);
                }
            }
            ContentCommand::StemSetExpanded(expand) => {
                if let Some(ref mut stem) = self.stem_audio {
                    // Auto-load stems on first expand if paths available but not yet loaded.
                    // Port of Unity WorkspaceController.EnsureStemAudioController lazy init.
                    if expand && !stem.stems_ready() {
                        if let Some(stem_paths_vec) = self.engine.project()
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

            // ── Compositor ─────────────────────────────────────────
            ContentCommand::MarkCompositorDirty => {
                self.engine.mark_compositor_dirty(0.0);
            }

            // ── Generator ─────────────────────────────────────────
            ContentCommand::GeneratorTypeChanged { layer_index, new_type } => {
                // Port of C# PlaybackController.NotifyGeneratorTypeChanged().
                let (renderers, _) = self.engine.split_renderer_project();
                for renderer in renderers.iter_mut() {
                    if let Some(gen) = renderer.as_any_mut()
                        .downcast_mut::<manifold_renderer::generator_renderer::GeneratorRenderer>()
                    {
                        gen.update_active_types_for_layer(layer_index, new_type);
                        break;
                    }
                }
                self.engine.mark_compositor_dirty(0.0);
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
        }
        false
    }
}
