//! Command dispatch for ContentThread — extracted from content_thread.rs.
//! Contains the `handle_command` method which routes `ContentCommand` variants
//! to the appropriate subsystem on the content thread.

use manifold_core::types::ClockAuthority;
use manifold_core::{Beats, Seconds};
use manifold_playback::transport_controller::TransportController;

use crate::content_command::ContentCommand;
use crate::content_thread::ContentThread;

/// Look up the existing Ableton mapping for a target (for undo snapshot).
fn get_existing_mapping(
    project: &manifold_core::project::Project,
    target: &manifold_core::ableton_mapping::AbletonMappingTarget,
    param_index: usize,
) -> Option<manifold_core::ableton_mapping::AbletonParamMapping> {
    use manifold_core::ableton_mapping::AbletonMappingTarget;
    match target {
        AbletonMappingTarget::MasterEffect { effect_type, .. } => {
            project.settings.master_effects.iter()
                .find(|f| f.effect_type() == effect_type)
                .and_then(|fx| fx.ableton_mappings.as_ref())
                .and_then(|ms| ms.iter().find(|m| m.param_index == param_index))
                .cloned()
        }
        AbletonMappingTarget::LayerEffect { layer_id, effect_type, .. } => {
            project.timeline.find_layer_by_id(layer_id.as_str())
                .and_then(|(_, l)| l.effects.as_ref())
                .and_then(|es| es.iter().find(|f| f.effect_type() == effect_type))
                .and_then(|fx| fx.ableton_mappings.as_ref())
                .and_then(|ms| ms.iter().find(|m| m.param_index == param_index))
                .cloned()
        }
        AbletonMappingTarget::GenParam { layer_id, .. } => {
            project.timeline.find_layer_by_id(layer_id.as_str())
                .and_then(|(_, l)| l.gen_params())
                .and_then(|gp| gp.ableton_mappings.as_ref())
                .and_then(|ms| ms.iter().find(|m| m.param_index == param_index))
                .cloned()
        }
        AbletonMappingTarget::MacroSlot { slot_index } => {
            project.settings.macro_bank.slots.get(*slot_index)
                .and_then(|s| s.ableton_mapping.clone())
        }
    }
}

impl ContentThread {
    /// Handle a single command. Returns true if Shutdown.
    pub(crate) fn handle_command(&mut self, cmd: ContentCommand) -> bool {
        match cmd {
            ContentCommand::Shutdown => return true,

            ContentCommand::WatchEffectGraph(type_id) => {
                self.watched_graph_effect = type_id;
            }

            // ── GPU events ─────────────────────────────────────────
            // SurfaceReady wakes recv() — no action needed, the main
            // loop re-checks is_surface_ready() after handling.
            #[cfg(target_os = "macos")]
            ContentCommand::SurfaceReady => {}

            // ── Transport ──────────────────────────────────────────
            ContentCommand::Play => {
                // User-initiated transport: clear any stale suppress flag so
                // OscPositionSender doesn't silently swallow this change.
                // Also claim ownership so MIDI Clock doesn't fight us.
                if self.osc_sender.is_sender_enabled()
                    || self.ableton_bridge.is_transport_enabled()
                {
                    self.sync_arbiter.suppress_next_transport = false;
                    self.sync_arbiter.set_manifold_owns_at(self.time_since_start);
                    // Suppress MIDI Clock position sync during the play→seek
                    // round-trip. Without this, stale MIDI Clock positions
                    // arriving before Ableton processes our deferred seek
                    // pull the playhead back to the wrong beat.
                    self.sync_arbiter.set_user_seek_time(self.time_since_start);
                }
                // Align transport to active external beat source BEFORE
                // the first sync pass. Port of C# PlaybackController.Play() lines 631-643.
                let authority = self.engine.project()
                    .map_or(ClockAuthority::Internal, |p| p.settings.clock_authority);
                if authority == ClockAuthority::MidiClock
                    && !self.sync_arbiter.manifold_owns_playback
                    && let Some(ref clk) = self.transport_controller.midi_clock_sync
                        && clk.is_midi_clock_enabled() {
                            let midi_beat = Beats::from_f32(clk.current_clock_beat());
                            self.engine.set_beat(midi_beat);
                            let time = self.engine.beat_to_timeline_time(midi_beat);
                            self.engine.set_time(Seconds(time.0.max(0.0)));
                        }
                self.engine.play();
                self.cache_link_beat_offset();
            }
            ContentCommand::Pause => {
                if self.osc_sender.is_sender_enabled()
                    || self.ableton_bridge.is_transport_enabled()
                {
                    self.sync_arbiter.suppress_next_transport = false;
                    self.sync_arbiter.set_manifold_owns_at(self.time_since_start);
                }
                // End tempo recording session on pause.
                // Port of C# PlaybackController.Pause → tempoRecorder.EndSessionIfActive.
                self.end_tempo_recording_session();
                self.engine.pause();
            }
            ContentCommand::Stop => {
                if self.osc_sender.is_sender_enabled()
                    || self.ableton_bridge.is_transport_enabled()
                {
                    self.sync_arbiter.suppress_next_transport = false;
                    self.sync_arbiter.set_manifold_owns_at(self.time_since_start);
                }
                // End tempo recording session on stop.
                self.end_tempo_recording_session();
                self.engine.stop();
                self.link_beat_offset = f64::NAN;
            }
            ContentCommand::SeekTo(t) => {
                self.sync_arbiter.set_user_seek_time(self.time_since_start);
                self.engine.seek_to(t);
                self.cache_link_beat_offset();
            }
            ContentCommand::SeekToBeat(beat) => {
                self.sync_arbiter.set_user_seek_time(self.time_since_start);
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
                    self.ableton_bridge.rebuild_listeners(p);
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
                    (p.settings.output_width, p.settings.output_height, p.settings.frame_rate, p.settings.render_scale)
                });
                if let Some(p) = self.engine.project_mut() {
                    let _ = self.editing_service.undo(p);
                }
                self.engine.mark_compositor_dirty(Seconds::ZERO);
                self.engine.mark_sync_dirty();
                // Apply resolution/FPS changes if the undo altered project settings.
                let post = self.engine.project().map(|p| {
                    (p.settings.output_width, p.settings.output_height, p.settings.frame_rate, p.settings.render_scale)
                });
                if let (Some((pre_w, pre_h, pre_fps, pre_rs)), Some((post_w, post_h, post_fps, post_rs))) = (pre, post) {
                    if post_w != pre_w || post_h != pre_h || (post_rs - pre_rs).abs() > 0.01 {
                        self.content_pipeline.resize(
                            &mut self.engine,
                            post_w as u32, post_h as u32, post_rs,
                        );
                    }
                    if (post_fps - pre_fps).abs() > 0.01 {
                        self.timer.set_target_fps(post_fps as f64);
                    }
                }
                if let Some(p) = self.engine.project() {
                    self.osc_param_router.rebuild(p, &mut self.osc_receiver);
                    self.ableton_bridge.rebuild_listeners(p);
                }
            }
            ContentCommand::Redo => {
                // Same pre/post settings detection as Undo.
                let pre = self.engine.project().map(|p| {
                    (p.settings.output_width, p.settings.output_height, p.settings.frame_rate, p.settings.render_scale)
                });
                if let Some(p) = self.engine.project_mut() {
                    let _ = self.editing_service.redo(p);
                }
                self.engine.mark_compositor_dirty(Seconds::ZERO);
                self.engine.mark_sync_dirty();
                // Apply resolution/FPS changes if the redo altered project settings.
                let post = self.engine.project().map(|p| {
                    (p.settings.output_width, p.settings.output_height, p.settings.frame_rate, p.settings.render_scale)
                });
                if let (Some((pre_w, pre_h, pre_fps, pre_rs)), Some((post_w, post_h, post_fps, post_rs))) = (pre, post) {
                    if post_w != pre_w || post_h != pre_h || (post_rs - pre_rs).abs() > 0.01 {
                        self.content_pipeline.resize(
                            &mut self.engine,
                            post_w as u32, post_h as u32, post_rs,
                        );
                    }
                    if (post_fps - pre_fps).abs() > 0.01 {
                        self.timer.set_target_fps(post_fps as f64);
                    }
                }
                if let Some(p) = self.engine.project() {
                    self.osc_param_router.rebuild(p, &mut self.osc_receiver);
                    self.ableton_bridge.rebuild_listeners(p);
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
                // Clear stale temporal effect state from the previous project
                // (feedback textures, bloom state, etc.) to prevent bleed-through.
                self.content_pipeline.clear_all_effect_state();
                // Resize content pipeline to project dims and render scale.
                if let Some(p) = self.engine.project() {
                    let w = p.settings.output_width.max(1) as u32;
                    let h = p.settings.output_height.max(1) as u32;
                    let rs = p.settings.render_scale;
                    self.content_pipeline.resize(&mut self.engine, w, h, rs);
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
                self.osc_receiver.start_listening();
                // Auto-connect Ableton bridge — silently stays disconnected if
                // Ableton isn't running. Heartbeat will detect connection later.
                self.ableton_bridge.connect();
                // Re-validate mappings against the current Ableton session so
                // they don't sit at default Dormant after a project load.
                // Discovery may already be complete from a prior connect, in
                // which case `take_validation_dirty` won't fire on its own.
                if let Some(p) = self.engine.project_mut() {
                    self.ableton_bridge.validate_mappings(p);
                    self.ableton_bridge.rebuild_listeners(p);
                }
                // Honor the persisted LED ON/OFF state. If the project saved
                // with the LED pipeline enabled, stand it up automatically;
                // otherwise tear down any previously active controller.
                let want_led_enabled = self
                    .engine
                    .project()
                    .is_some_and(|p| p.settings.led_enabled);
                if want_led_enabled {
                    if !self.led_controller.as_ref().is_some_and(|c| c.is_initialized()) {
                        let settings = manifold_led::LedSettings {
                            enabled: true,
                            ..Default::default()
                        };
                        let mut ctrl = manifold_led::LedOutputController::new();
                        let native_device = self
                            .content_pipeline
                            .native_device()
                            .expect("native device required for LED init");
                        let strip_count = settings.strip_count;
                        let leds_per_strip = settings.leds_per_strip;
                        if ctrl.initialize(native_device, &settings) {
                            self.led_controller = Some(ctrl);
                            self.content_pipeline
                                .set_led_grid_size(strip_count, leds_per_strip);
                            log::info!(
                                "[ContentThread] LED output auto-initialised from project."
                            );
                        }
                    }
                } else if let Some(ref mut ctrl) = self.led_controller {
                    ctrl.shutdown();
                    self.led_controller = None;
                }
            }
            // ── Settings ───────────────────────────────────────────
            ContentCommand::SetFrameRate(fps) => {
                if let Some(p) = self.engine.project_mut() {
                    p.settings.frame_rate = fps as f32;
                }
                self.timer.set_target_fps(fps);
            }

            // ── GPU ────────────────────────────────────────────────
            ContentCommand::ResizeContent(w, h, render_scale) => {
                self.content_pipeline.resize(&mut self.engine, w, h, render_scale);
            }
            ContentCommand::ResizeWorkspacePreview(w, h) => {
                self.content_pipeline.resize_workspace_preview(w, h);
            }

            // ── Transport/sync ─────────────────────────────────────
            ContentCommand::ToggleLink => {
                self.transport_controller.toggle_link(&mut self.engine);
            }
            ContentCommand::ToggleMidiClock => {
                self.transport_controller.toggle_midi_clock(&mut self.engine);
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
            ContentCommand::AudioLoaded { preloaded } => {
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
                // Rebuild Ableton listeners so trim range changes take effect
                // immediately (WriteTarget caches range_min/range_max).
                if self.ableton_bridge.is_connected()
                    && let Some(p) = self.engine.project()
                {
                    self.ableton_bridge.rebuild_listeners(p);
                }
            }

            // ── Clipboard ─────────────────────────────────────────
            ContentCommand::CopyClips { clip_ids, region } => {
                if let Some(p) = self.engine.project() {
                    let spb = 60.0 / p.settings.bpm.0.max(1.0);
                    self.editing_service.copy_clips(p, &clip_ids, region.as_ref(), spb);
                }
            }
            ContentCommand::PasteClips { target_beat, target_layer, result_tx } => {
                if let Some(p) = self.engine.project_mut() {
                    let spb = 60.0 / p.settings.bpm.0.max(1.0);
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
                        beat.as_f32(),
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
                            playhead_beat.as_f32(), beats_per_bar, true,
                        );
                }
            }
            ContentCommand::PercussionNudgeAlignment(delta_beats) => {
                if let Some(p) = self.engine.project_mut() {
                    self.percussion_orchestrator
                        .nudge_imported_percussion_alignment(
                            delta_beats.as_f32(), p, &mut self.editing_service, true,
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

            // ── Ableton bridge ──────────────────────────���──────────
            ContentCommand::AbletonMapParam { target, address } => {
                use manifold_core::ableton_mapping::{
                    AbletonMappingStatus, AbletonMappingTarget, AbletonParamMapping,
                };
                use manifold_editing::commands::ableton::ChangeAbletonMappingCommand;
                if let Some(p) = self.engine.project_mut() {
                    let pi = match &target {
                        AbletonMappingTarget::MasterEffect { param_index, .. }
                        | AbletonMappingTarget::LayerEffect { param_index, .. }
                        | AbletonMappingTarget::GenParam { param_index, .. } => *param_index,
                        AbletonMappingTarget::MacroSlot { slot_index } => *slot_index,
                    };
                    // Capture old state for undo
                    let old_mapping = get_existing_mapping(p, &target, pi);
                    let (old_label, new_label) = match &target {
                        AbletonMappingTarget::MacroSlot { slot_index } => {
                            let old = p.settings.macro_bank.slots.get(*slot_index)
                                .map(|s| s.label.clone());
                            let new = Some(address.macro_name.clone());
                            (old, new)
                        }
                        _ => (None, None),
                    };
                    let new_mapping = AbletonParamMapping {
                        param_index: pi,
                        address,
                        range_min: 0.0,
                        range_max: 1.0,
                        inverted: false,
                        last_value: 0.0,
                        status: AbletonMappingStatus::Active,
                    };
                    let cmd = ChangeAbletonMappingCommand::map(
                        target, new_mapping, old_mapping, old_label, new_label,
                    );
                    self.editing_service.execute(Box::new(cmd), p);
                    self.ableton_bridge.rebuild_listeners(p);
                    p.settings.ableton_set_context =
                        Some(self.ableton_bridge.build_set_context());
                }
                self.engine.mark_sync_dirty();
            }
            ContentCommand::AbletonUnmapParam { target } => {
                use manifold_core::ableton_mapping::AbletonMappingTarget;
                use manifold_editing::commands::ableton::ChangeAbletonMappingCommand;
                if let Some(p) = self.engine.project_mut() {
                    let pi = match &target {
                        AbletonMappingTarget::MasterEffect { param_index, .. }
                        | AbletonMappingTarget::LayerEffect { param_index, .. }
                        | AbletonMappingTarget::GenParam { param_index, .. } => *param_index,
                        AbletonMappingTarget::MacroSlot { slot_index } => *slot_index,
                    };
                    if let Some(old) = get_existing_mapping(p, &target, pi) {
                        let cmd = ChangeAbletonMappingCommand::unmap(target, old);
                        self.editing_service.execute(Box::new(cmd), p);
                        self.ableton_bridge.rebuild_listeners(p);
                    }
                }
                self.engine.mark_sync_dirty();
            }
            ContentCommand::AbletonRediscover => {
                if self.ableton_bridge.is_connected() {
                    let realtime = self.timer.realtime_since_start();
                    self.ableton_bridge.start_discovery(realtime);
                }
            }
            ContentCommand::ToggleOscSyncMode => {
                // Toggle AbletonOSC transport sync on/off.
                // Sets mode to AbletonOsc and enables/disables transport listeners.
                if let Some(p) = self.engine.project_mut() {
                    p.settings.osc_sync_mode =
                        manifold_core::types::OscSyncMode::AbletonOsc;
                }
                if self.ableton_bridge.is_transport_enabled() {
                    self.ableton_bridge.disable_transport_sync();
                } else {
                    if !self.ableton_bridge.is_connected() {
                        self.ableton_bridge.connect();
                    }
                    self.ableton_bridge.enable_transport_sync();
                }
            }

            // ── Compositor ─────────────────────────────────────────
            ContentCommand::MarkCompositorDirty => {
                self.engine.mark_compositor_dirty(Seconds::ZERO);
            }

            // ── Display ───────────────────────────────────────────
            ContentCommand::UpdateEdrHeadroom(headroom) => {
                log::info!(
                    "[EDR] Content thread: headroom updated to {:.2}x (mode={})",
                    headroom,
                    if headroom > 1.0 { "passthrough" } else { "ACES tonemap" },
                );
                self.content_pipeline.edr_headroom = headroom;
                self.engine.mark_compositor_dirty(Seconds::ZERO);
            }

            // ── Output surface (direct present) ────────────────────
            #[cfg(target_os = "macos")]
            ContentCommand::SetOutputSurface(surface) => {
                self.content_pipeline.set_output_surface(surface);
            }
            #[cfg(target_os = "macos")]
            ContentCommand::ClearOutputSurface => {
                self.content_pipeline.clear_output_surface();
            }
            #[cfg(target_os = "macos")]
            ContentCommand::ResizeOutputSurface(w, h) => {
                self.content_pipeline.resize_output_surface(w, h);
            }
            #[cfg(target_os = "macos")]
            ContentCommand::SetOutputPresentSuspended(suspended) => {
                self.content_pipeline.set_output_present_suspended(suspended);
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
                self.engine.mark_compositor_dirty(Seconds::ZERO);
            }

            // ── LED output ─────────────────────────────────────────────
            ContentCommand::InitLedOutput(settings) => {
                let mut ctrl = manifold_led::LedOutputController::new();
                let native_device = self.content_pipeline.native_device()
                    .expect("native device required for LED init");
                let strip_count = settings.strip_count;
                let leds_per_strip = settings.leds_per_strip;
                if ctrl.initialize(native_device, &settings) {
                    self.led_controller = Some(ctrl);
                    self.content_pipeline.set_led_grid_size(strip_count, leds_per_strip);
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
            // ── Export ────────────────────────────────────────────────
            ContentCommand::StartExport(_) => {
                // Handled in run() loop directly (needs cmd_rx/state_tx access).
            }
            ContentCommand::CancelExport => {
                // No-op outside of export loop — cancel flag checked inside run_export.
            }

            // ── Live Recording ──────────────────────────────────────
            #[cfg(target_os = "macos")]
            ContentCommand::StartLiveRecording(config) => {
                if self.content_pipeline.recording_session.is_some() {
                    log::warn!("[ContentThread] Already recording, ignoring StartLiveRecording");
                    return false;
                }
                let device = self.content_pipeline.native_device();
                let (w, h) = self.content_pipeline.output_dimensions();
                let fps = self.engine.project().map_or(
                    60.0,
                    |p| p.settings.frame_rate,
                );
                if let Some(device) = device {
                    match manifold_recording::LiveRecordingSession::new(
                        *config, device, w, h, fps,
                    ) {
                        Ok(session) => {
                            log::info!("[ContentThread] Live recording started");
                            self.content_pipeline.recording_session = Some(session);
                        }
                        Err(e) => {
                            log::error!("[ContentThread] Failed to start recording: {e}");
                        }
                    }
                } else {
                    log::error!("[ContentThread] No native device — cannot record");
                }
            }
            #[cfg(target_os = "macos")]
            ContentCommand::StopLiveRecording => {
                if let Some(session) = self.content_pipeline.recording_session.take() {
                    let result = session.stop();
                    log::info!(
                        "[ContentThread] Recording stopped: {} frames, {} dropped, {:.1}s -> {}",
                        result.frames_recorded,
                        result.frames_dropped,
                        result.duration_seconds,
                        result.output_path,
                    );
                }
            }
            #[cfg(not(target_os = "macos"))]
            ContentCommand::StartLiveRecording(_) | ContentCommand::StopLiveRecording => {
                log::warn!("[ContentThread] Live recording not supported on this platform");
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
                                start_beat: c.start_beat.as_f32(),
                                duration_beats: c.duration_beats.as_f32(),
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
}
