//! Project-related dispatch: file operations, export, audio/percussion, resolution,
//! MIDI note/channel, generator type, waveform/stem actions.

use manifold_core::LayerId;
use manifold_core::project::Project;
use manifold_core::types::GeneratorType;
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
            project.settings.export_hdr = !project.settings.export_hdr;
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
            if let Some(layer) = project.timeline.layers.get_mut(*layer_idx) {
                layer.midi_channel = *channel;
            }
            let li = *layer_idx;
            let ch = *channel;
            ContentCommand::send(content_tx, ContentCommand::MutateProject(Box::new(move |p| {
                if let Some(layer) = p.timeline.layers.get_mut(li) {
                    layer.midi_channel = ch;
                }
            })));
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
            project.settings.output_width = *w;
            project.settings.output_height = *h;
            let ww = *w;
            let hh = *h;
            ContentCommand::send(content_tx, ContentCommand::MutateProject(Box::new(move |p| {
                p.settings.output_width = ww;
                p.settings.output_height = hh;
            })));
            DispatchResult::resolution()
        }
        PanelAction::SetGenType(layer_idx, gen_type_idx) => {
            if let Some(layer) = project.timeline.layers.get(*layer_idx) {
                let old_type = layer.gen_params.as_ref()
                    .map(|gp| gp.generator_type)
                    .unwrap_or(GeneratorType::None);
                if let Some(new_type) = GeneratorType::from_index(*gen_type_idx)
                    && new_type != old_type {
                        let old_params = layer.gen_params.as_ref()
                            .map(|gp| gp.param_values.clone())
                            .unwrap_or_default();
                        let old_drivers = layer.gen_params.as_ref()
                            .and_then(|gp| gp.drivers.clone());
                        let old_envelopes = layer.gen_params.as_ref()
                            .and_then(|gp| gp.envelopes.clone());
                        let layer_id = layer.layer_id.clone();
                        let cmd = manifold_editing::commands::settings::ChangeGeneratorTypeCommand::new(
                            layer_id, old_type, new_type, old_params, old_drivers, old_envelopes,
                        );
                        { let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd); boxed.execute(project); ContentCommand::send(content_tx, ContentCommand::Execute(boxed)); }
                        let layer_id = project.timeline.layers.get(*layer_idx)
                            .map(|l| l.layer_id.clone())
                            .unwrap_or_default();
                        ContentCommand::send(content_tx, ContentCommand::GeneratorTypeChanged {
                            layer_id,
                            new_type,
                        });
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
            // Capture pre-drag state for undo (first delta only).
            if let Some(state) = project.percussion_import.as_ref() {
                ui.waveform_lane.set_drag_start_beat(state.audio_start_beat);
            }
            // Mutate local project copy (live preview).
            if let Some(state) = project.percussion_import.as_mut() {
                state.audio_start_beat = (state.audio_start_beat + *delta_beats).max(0.0);
            }
            // Sync to content thread so audio playback follows.
            let db = *delta_beats;
            ContentCommand::send(content_tx, ContentCommand::MutateProject(Box::new(move |p| {
                if let Some(state) = p.percussion_import.as_mut() {
                    state.audio_start_beat = (state.audio_start_beat + db).max(0.0);
                }
            })));
            DispatchResult::handled()
        }
        PanelAction::WaveformDragEnd(_total_delta) => {
            // Record undo command for the complete drag operation.
            if let Some(old_start) = ui.waveform_lane.take_drag_start_beat() {
                let new_start = project.percussion_import
                    .as_ref()
                    .map_or(0.0, |s| s.audio_start_beat);
                if (new_start - old_start).abs() > 0.0001 {
                    let cmd = manifold_editing::commands::settings::SetAudioStartBeatCommand::new(
                        old_start, new_start,
                    );
                    // State already at new_start — send command for undo stack only.
                    let boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd);
                    ContentCommand::send(content_tx, ContentCommand::Execute(boxed));
                }
            }
            DispatchResult::handled()
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
