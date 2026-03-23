//! Project-related dispatch: file operations, export, audio/percussion, resolution,
//! MIDI note/channel, generator type, waveform/stem actions.

use manifold_core::LayerId;
use manifold_core::project::Project;
use manifold_core::GeneratorTypeId;
use manifold_ui::PanelAction;

use crate::app::SelectionState;
use crate::dialog_path_memory::{self, DialogContext};
use crate::ui_root::UIRoot;
use crate::user_prefs::UserPrefs;
use super::DispatchResult;

pub(super) fn dispatch_project(
    action: &PanelAction,
    project: &mut Project,
    content_tx: &crossbeam_channel::Sender<crate::content_command::ContentCommand>,
    _content_state: &crate::content_state::ContentState,
    ui: &mut UIRoot,
    _selection: &mut SelectionState,
    _active_layer: &mut Option<LayerId>,
    user_prefs: &mut UserPrefs,
) -> DispatchResult {
    use crate::content_command::ContentCommand;
    match action {
        // ── Export/Header/Footer ───────────────────────────────────
        PanelAction::ToggleHdr => {
            let old_hdr = project.settings.export_hdr;
            let cmd = manifold_editing::commands::settings::ToggleExportHdrCommand::new(old_hdr);
            { let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd); boxed.execute(project); ContentCommand::send(content_tx, ContentCommand::Execute(boxed)); }
            log::info!("HDR export → {}", project.settings.export_hdr);
            DispatchResult::handled()
        }
        PanelAction::TogglePercussion => {
            let last_dir = dialog_path_memory::get_last_directory(
                DialogContext::PercussionImport, user_prefs,
            );
            let mut dialog = rfd::FileDialog::new()
                .set_title("Import Audio for Percussion Analysis")
                .add_filter("Audio Files", &["wav", "mp3", "m4a", "aac", "flac", "ogg", "aif", "aiff", "wma", "json"]);
            if !last_dir.is_empty() {
                dialog = dialog.set_directory(&last_dir);
            }
            if let Some(path) = dialog.pick_file() {
                let path_str = path.to_string_lossy().to_string();
                dialog_path_memory::remember_directory(
                    DialogContext::PercussionImport, &path_str, user_prefs,
                );
                ContentCommand::send(content_tx, ContentCommand::PercussionImport(path_str));
                ui.layout.waveform_lane_visible = true;
            }
            DispatchResult::structural()
        }
        PanelAction::ToggleMonitor => {
            DispatchResult {
                handled: true,
                structural_change: false,
                resolution_changed: false,
            }
        }

        PanelAction::NewProject
        | PanelAction::OpenProject
        | PanelAction::OpenRecent
        | PanelAction::SaveProject
        | PanelAction::SaveProjectAs => {
            log::warn!("File action {:?} reached ui_bridge (should be intercepted in app.rs)", action);
            DispatchResult::handled()
        }
        PanelAction::ExportVideo
        | PanelAction::ExportXml => {
            log::info!("Export action: {:?} (not yet wired)", action);
            DispatchResult::handled()
        }

        // ── Dropdown results (context-routed from UIRoot) ────────────
        PanelAction::SetMidiNote(layer_idx, note) => {
            if let Some(layer) = project.timeline.layers.get(*layer_idx) {
                let layer_id = layer.layer_id.clone();
                let old_note = layer.midi_note;
                let cmd = manifold_editing::commands::settings::ChangeLayerMidiNoteCommand::new(
                    layer_id, old_note, *note,
                );
                { let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd); boxed.execute(project); ContentCommand::send(content_tx, ContentCommand::Execute(boxed)); }
            }
            DispatchResult::structural()
        }
        PanelAction::SetMidiChannel(layer_idx, channel) => {
            if let Some(layer) = project.timeline.layers.get(*layer_idx) {
                let layer_id = layer.layer_id.clone();
                let old_channel = layer.midi_channel;
                let cmd = manifold_editing::commands::settings::ChangeLayerMidiChannelCommand::new(
                    layer_id, old_channel, *channel,
                );
                { let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd); boxed.execute(project); ContentCommand::send(content_tx, ContentCommand::Execute(boxed)); }
            }
            DispatchResult::structural()
        }
        PanelAction::SetResolution(preset_idx) => {
            use manifold_core::types::ResolutionPreset;
            let old = project.settings.resolution_preset;
            if let Some(new) = ResolutionPreset::from_index(*preset_idx) {
                let cmd = manifold_editing::commands::settings::ChangeResolutionCommand::new(old, new);
                { let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd); boxed.execute(project); ContentCommand::send(content_tx, ContentCommand::Execute(boxed)); }
            }
            DispatchResult::resolution()
        }
        PanelAction::SetDisplayResolution(w, h) => {
            let old_w = project.settings.output_width;
            let old_h = project.settings.output_height;
            let cmd = manifold_editing::commands::settings::SetDisplayDimensionsCommand::new(
                old_w, old_h, *w, *h,
            );
            { let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd); boxed.execute(project); ContentCommand::send(content_tx, ContentCommand::Execute(boxed)); }
            DispatchResult::resolution()
        }
        PanelAction::SetGenType(opt_layer_id, gen_type_idx) => {
            let resolved_idx = opt_layer_id.as_ref()
                .and_then(|lid| project.timeline.find_layer_index_by_id(lid));
            if let Some(layer_idx) = resolved_idx {
                let available = manifold_core::generator_type_registry::available_generators();
                let layer = &project.timeline.layers[layer_idx];
                let old_type = layer.gen_params()
                    .map(|gp| gp.generator_type().clone())
                    .unwrap_or(GeneratorTypeId::NONE);
                if let Some(reg) = available.get(*gen_type_idx) {
                    let new_type = reg.id.clone();
                    if new_type != old_type {
                        let old_params = layer.gen_params()
                            .map(|gp| gp.param_values.clone())
                            .unwrap_or_default();
                        let old_drivers = layer.gen_params()
                            .and_then(|gp| gp.drivers.clone());
                        let old_envelopes = layer.gen_params()
                            .and_then(|gp| gp.envelopes.clone());
                        let layer_id = layer.layer_id.clone();
                        let cmd = manifold_editing::commands::settings::ChangeGeneratorTypeCommand::new(
                            layer_id.clone(), old_type, new_type.clone(), old_params, old_drivers, old_envelopes,
                        );
                        { let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd); boxed.execute(project); ContentCommand::send(content_tx, ContentCommand::Execute(boxed)); }
                        ContentCommand::send(content_tx, ContentCommand::GeneratorTypeChanged {
                            layer_id,
                            new_type,
                        });
                    }
                }
            }
            DispatchResult::structural()
        }

        // ── Waveform lane ─────────────────────────────────────────
        PanelAction::ImportAudioClicked => {
            let last_dir = dialog_path_memory::get_last_directory(
                DialogContext::PercussionImport, user_prefs,
            );
            let mut dialog = rfd::FileDialog::new()
                .set_title("Import Audio for Percussion Analysis")
                .add_filter("Audio Files", &["wav", "mp3", "m4a", "aac", "flac", "ogg", "aif", "aiff", "wma", "json"]);
            if !last_dir.is_empty() {
                dialog = dialog.set_directory(&last_dir);
            }
            if let Some(path) = dialog.pick_file() {
                let path_str = path.to_string_lossy().to_string();
                dialog_path_memory::remember_directory(
                    DialogContext::PercussionImport, &path_str, user_prefs,
                );
                ContentCommand::send(content_tx, ContentCommand::PercussionImport(path_str));
                ui.layout.waveform_lane_visible = true;
            }
            DispatchResult::structural()
        }
        PanelAction::RemoveAudioClicked => {
            log::info!("Remove audio clicked");
            let old_state = project.percussion_import.clone();
            let cmd = manifold_editing::commands::settings::ClearPercussionCommand::new(old_state);
            { let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd); boxed.execute(project); ContentCommand::send(content_tx, ContentCommand::Execute(boxed)); }
            ContentCommand::send(content_tx, ContentCommand::ResetAudio);
            ContentCommand::send(content_tx, ContentCommand::StemReset);
            ui.waveform_lane.clear_audio();
            ui.stem_lanes.clear_all_stems();
            ui.layout.waveform_lane_visible = true;
            ui.layout.stem_lanes_expanded = false;
            DispatchResult::structural()
        }
        PanelAction::WaveformScrub(local_x, _local_y) => {
            // Events arrive in panel-local coords (offset by wf_rect.x in ui_root),
            // so use local_pixel_to_beat which doesn't subtract tracks_rect.x again.
            let beat = ui.viewport.local_pixel_to_beat(*local_x).max(0.0);
            ContentCommand::send(content_tx, ContentCommand::SeekToBeat(beat));
            DispatchResult::handled()
        }
        PanelAction::WaveformDragDelta(delta_beats) => {
            // Unity EditingService.OnWaveformDragDeltaBeats (lines 1355-1405):
            // On first delta, capture audio start + snapshot ALL clips.
            // Per delta, clamp so nothing goes below beat 0, then move
            // audio AND all clips by the clamped delta.
            if !ui.waveform_lane.has_drag_start_beat() {
                if let Some(state) = project.percussion_import.as_ref() {
                    ui.waveform_lane.set_drag_start_beat(state.audio_start_beat);
                }
                // Snapshot all clips (Unity lines 1366-1377)
                ui.waveform_lane.waveform_drag_clip_snapshots.clear();
                for layer in &project.timeline.layers {
                    for clip in &layer.clips {
                        ui.waveform_lane.waveform_drag_clip_snapshots.push((
                            clip.id.clone(),
                            clip.start_beat,
                            clip.layer_id.clone(),
                        ));
                    }
                }
            }

            // Clamp delta so nothing goes below beat 0 (Unity lines 1380-1388)
            let mut min_current = project.percussion_import
                .as_ref()
                .map_or(f32::MAX, |s| s.audio_start_beat);
            for layer in &project.timeline.layers {
                for clip in &layer.clips {
                    if clip.start_beat < min_current {
                        min_current = clip.start_beat;
                    }
                }
            }
            let clamped = delta_beats.max(-min_current);

            // Move audio (Unity lines 1391-1393)
            if let Some(state) = project.percussion_import.as_mut() {
                state.audio_start_beat = (state.audio_start_beat + clamped).max(0.0);
            }

            // Move ALL clips (Unity lines 1395-1400)
            for layer in &mut project.timeline.layers {
                for clip in &mut layer.clips {
                    clip.start_beat = (clip.start_beat + clamped).max(0.0);
                }
                layer.mark_clips_unsorted();
            }

            // Sync to content thread
            let db = clamped;
            ContentCommand::send(content_tx, ContentCommand::MutateProject(Box::new(move |p| {
                if let Some(state) = p.percussion_import.as_mut() {
                    state.audio_start_beat = (state.audio_start_beat + db).max(0.0);
                }
                for layer in &mut p.timeline.layers {
                    for clip in &mut layer.clips {
                        clip.start_beat = (clip.start_beat + db).max(0.0);
                    }
                    layer.mark_clips_unsorted();
                }
            })));
            DispatchResult::structural()
        }
        PanelAction::WaveformDragEnd(_total_delta) => {
            // Unity EditingService.OnWaveformDragEnd (lines 1407-1464):
            // Build CompositeCommand with SetAudioStartBeatCommand + MoveClipCommand
            // per changed clip, for a single undoable unit.
            if let Some(old_audio_start) = ui.waveform_lane.take_drag_start_beat() {
                let new_audio_start = project.percussion_import
                    .as_ref()
                    .map_or(0.0, |s| s.audio_start_beat);

                let mut commands: Vec<Box<dyn manifold_editing::command::Command>> =
                    Vec::new();

                // Audio command
                if (new_audio_start - old_audio_start).abs() > 0.0001 {
                    commands.push(Box::new(
                        manifold_editing::commands::settings::SetAudioStartBeatCommand::new(
                            old_audio_start, new_audio_start,
                        ),
                    ));
                }

                // Clip move commands (Unity lines 1440-1454)
                let snapshots =
                    std::mem::take(&mut ui.waveform_lane.waveform_drag_clip_snapshots);
                for (clip_id, old_beat, layer_id) in &snapshots {
                    let new_beat = project.timeline.find_clip_by_id(clip_id)
                        .map(|c| c.start_beat);
                    if let Some(new_beat) = new_beat
                        && (new_beat - old_beat).abs() > 0.0001
                    {
                        commands.push(Box::new(
                            manifold_editing::commands::clip::MoveClipCommand::new(
                                clip_id.clone(),
                                *old_beat,
                                new_beat,
                                layer_id.clone(),
                                layer_id.clone(),
                            ),
                        ));
                    }
                }

                if !commands.is_empty() {
                    // State already applied — send for undo stack only
                    let composite = manifold_editing::command::CompositeCommand::new(
                        commands,
                        "Drag audio + clips".into(),
                    );
                    let boxed: Box<dyn manifold_editing::command::Command + Send> =
                        Box::new(composite);
                    ContentCommand::send(content_tx, ContentCommand::Execute(boxed));
                }
            } else {
                // No drag_start_beat means drag was a no-op, just clear snapshots
                ui.waveform_lane.waveform_drag_clip_snapshots.clear();
            }
            DispatchResult::structural()
        }
        PanelAction::ExpandStemsToggled(expanded) => {
            ui.waveform_lane.set_expanded_state(*expanded);
            ui.stem_lanes.set_expanded(*expanded);
            ui.layout.stem_lanes_expanded = *expanded;
            ContentCommand::send(content_tx, ContentCommand::StemSetExpanded(*expanded));

            if *expanded
                && let Some(stem_paths) = project.percussion_import
                    .as_ref()
                    .and_then(|perc| perc.stem_paths.as_ref())
                {
                    for (i, path) in stem_paths.iter().enumerate() {
                        if i >= manifold_playback::stem_audio::STEM_COUNT {
                            break;
                        }
                        match manifold_playback::audio_decoder::decode_audio_to_pcm(path) {
                            Ok(decoded) => {
                                ui.stem_lanes.set_stem_audio(
                                    i,
                                    &decoded.samples,
                                    decoded.channels,
                                    decoded.sample_rate,
                                );
                            }
                            Err(e) => {
                                log::warn!("[StemWaveform] Failed to decode stem {}: {}", i, e);
                            }
                        }
                    }
                }
            DispatchResult::structural()
        }
        PanelAction::ReAnalyzeDrums => {
            ContentCommand::send(content_tx, ContentCommand::ReAnalyzeTriggers("drums".into()));
            DispatchResult::handled()
        }
        PanelAction::ReAnalyzeBass => {
            ContentCommand::send(content_tx, ContentCommand::ReAnalyzeTriggers("bass".into()));
            DispatchResult::handled()
        }
        PanelAction::ReAnalyzeSynth => {
            ContentCommand::send(content_tx, ContentCommand::ReAnalyzeTriggers("synth".into()));
            DispatchResult::handled()
        }
        PanelAction::ReAnalyzeVocal => {
            ContentCommand::send(content_tx, ContentCommand::ReAnalyzeTriggers("vocal".into()));
            DispatchResult::handled()
        }
        PanelAction::ReImportStems => {
            ContentCommand::send(content_tx, ContentCommand::ReImportStems);
            DispatchResult::handled()
        }
        PanelAction::StemMuteToggled(stem_index) => {
            ContentCommand::send(content_tx, ContentCommand::StemToggleMute(*stem_index));
            DispatchResult::handled()
        }
        PanelAction::StemSoloToggled(stem_index) => {
            ContentCommand::send(content_tx, ContentCommand::StemToggleSolo(*stem_index));
            DispatchResult::handled()
        }

        _ => DispatchResult::unhandled(),
    }
}
