//! Content thread — runs PlaybackEngine, EditingService, and ContentPipeline
//! on a dedicated thread. Communicates with the UI thread via crossbeam channels.
//!
//! The content thread owns all authoritative state: the engine (which owns the
//! project), the editing service (undo/redo), audio sync, percussion, and the
//! GPU content pipeline (generators + compositor).


use crossbeam_channel::{Receiver, Sender};

use manifold_editing::service::EditingService;
use manifold_playback::audio_sync::ImportedAudioSyncController;
use manifold_playback::clip_launcher::ClipLauncher;
use manifold_playback::engine::{PlaybackEngine, TickContext};
use manifold_playback::midi_input::MidiInputController;
use manifold_playback::percussion_orchestrator::PercussionImportOrchestrator;
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
}

impl ContentThread {
    /// Run the content loop. Blocks until Shutdown is received.
    pub fn run(
        mut self,
        cmd_rx: Receiver<ContentCommand>,
        state_tx: Sender<ContentState>,
    ) {
        log::info!("[ContentThread] started");
        let mut timer = FrameTimer::new(60.0);

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
            if !timer.should_tick() {
                // Precise sleep: compute exact time to next frame, sleep most of it,
                // then spin for the final sub-ms to avoid macOS sleep overshoot (~1-2ms).
                let remaining = timer.time_until_next_tick();
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
            let dt = timer.consume_tick();
            let realtime = timer.realtime_since_start();
            self.time_since_start = realtime as f32;

            // 3. Process MIDI input (before engine tick — matches Unity Update() ordering).
            // Drains hardware note events and routes them to ClipLauncher → LiveClipManager.
            self.engine.tick_midi_input(
                &mut self.midi_input,
                &mut self.clip_launcher,
                realtime,
            );

            // 4. Tick engine
            let ctx = TickContext {
                dt_seconds: dt,
                realtime_now: realtime,
                pre_render_dt: dt as f32,
                frame_count: self.frame_count as i32,
                export_fixed_dt: 0.0,
            };
            let tick_result = self.engine.tick(ctx);

            // 5. Audio sync
            if let Some(ref mut audio_sync) = self.audio_sync {
                audio_sync.update_sync(&mut self.engine);
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
            let snapshot = if version != self.last_data_version {
                self.last_data_version = version;
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
                content_fps: timer.current_fps() as f32,
                content_frame_time_ms: (timer.last_dt() * 1000.0) as f32,
                data_version: version,
                editing_is_dirty: self.editing_service.is_dirty(),
                bpm: self.engine.project().map_or(120.0, |p| p.settings.bpm as f64),
                frame_rate: self.engine.project().map_or(60.0, |p| p.settings.frame_rate as f64),
                clock_authority: self.engine.project()
                    .map_or(manifold_core::types::ClockAuthority::Internal, |p| p.settings.clock_authority),
                time_signature_numerator: self.engine.project()
                    .map_or(4, |p| p.settings.time_signature_numerator),
                link_enabled: self.transport_controller.link_sync.as_ref()
                    .map_or(false, |s| s.is_enabled()),
                midi_clock_enabled: self.transport_controller.midi_clock_sync.as_ref()
                    .map_or(false, |s| s.is_enabled()),
                osc_sender_enabled: self.transport_controller.osc_sender_enabled,
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

    /// Handle a single command. Returns true if Shutdown.
    fn handle_command(&mut self, cmd: ContentCommand) -> bool {
        match cmd {
            ContentCommand::Shutdown => return true,

            // ── Transport ──────────────────────────────────────────
            ContentCommand::Play => self.engine.play(),
            ContentCommand::Pause => self.engine.pause(),
            ContentCommand::Stop => self.engine.stop(),
            ContentCommand::TogglePlayback => {
                if self.engine.is_playing() {
                    self.engine.pause();
                } else {
                    self.engine.play();
                }
            }
            ContentCommand::SeekTo(t) => { self.engine.seek_to(t); }
            ContentCommand::SeekToBeat(beat) => {
                let time = self.engine.beat_to_timeline_time(beat);
                self.engine.seek_to(time);
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
                if let Some(p) = self.engine.project_mut() {
                    self.editing_service.undo(p);
                }
                self.engine.mark_compositor_dirty(0.0);
            }
            ContentCommand::Redo => {
                if let Some(p) = self.engine.project_mut() {
                    self.editing_service.redo(p);
                }
                self.engine.mark_compositor_dirty(0.0);
            }
            ContentCommand::SetProject => {
                self.editing_service.set_project();
            }

            // ── Project lifecycle ──────────────────────────────────
            ContentCommand::LoadProject(project) => {
                if let Some(ref mut audio) = self.audio_sync {
                    audio.reset_audio();
                }
                self.engine.initialize(*project);
                // Resize content pipeline to project dims
                if let Some(p) = self.engine.project() {
                    let w = p.settings.output_width.max(1) as u32;
                    let h = p.settings.output_height.max(1) as u32;
                    self.content_pipeline.resize(&self.gpu.device, &mut self.engine, w, h);
                }
                // Update MIDI mapping config from the newly loaded project.
                // Port of C# PlaybackController.OnProjectLoaded → midiInputController.SetMidiConfig().
                if let Some(p) = self.engine.project() {
                    self.midi_input.set_midi_config(p.midi_config.clone());
                }
            }
            ContentCommand::NewProject(project) => {
                self.engine.initialize(*project);
                self.editing_service.set_project();
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

            // ── Compositor ─────────────────────────────────────────
            ContentCommand::MarkCompositorDirty => {
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
