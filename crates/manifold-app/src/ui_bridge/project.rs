//! Project-related dispatch: file operations, export, audio/percussion, resolution,
//! MIDI note/channel, generator type, waveform/stem actions.

use manifold_core::LayerId;
use manifold_core::PresetTypeId;
use manifold_core::project::Project;
use manifold_ui::PanelAction;

use super::DispatchResult;
use crate::app::SelectionState;
use crate::ui_root::UIRoot;
use crate::user_prefs::UserPrefs;

pub(super) fn dispatch_project(
    action: &PanelAction,
    project: &mut Project,
    content_tx: &crossbeam_channel::Sender<crate::content_command::ContentCommand>,
    _content_state: &crate::content_state::ContentState,
    _ui: &mut UIRoot,
    _selection: &mut SelectionState,
    _active_layer: &mut Option<LayerId>,
    _user_prefs: &mut UserPrefs,
) -> DispatchResult {
    use crate::content_command::ContentCommand;
    match action {
        // ── Export/Header/Footer ───────────────────────────────────
        PanelAction::ToggleHdr => {
            let old_hdr = project.settings.export_hdr;
            let cmd = manifold_editing::commands::settings::ToggleExportHdrCommand::new(old_hdr);
            {
                let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd);
                boxed.execute(project);
                ContentCommand::send(content_tx, ContentCommand::Execute(boxed));
            }
            log::info!("HDR export → {}", project.settings.export_hdr);
            DispatchResult::handled()
        }
        PanelAction::ToggleLiveRecording
        | PanelAction::SelectAudioInputDevice
        | PanelAction::SetAudioInputDevice(_)
        | PanelAction::ToggleMonitor => DispatchResult::handled(),
        PanelAction::EnterPerformMode => DispatchResult::handled(),

        PanelAction::NewProject
        | PanelAction::OpenProject
        | PanelAction::OpenRecent
        | PanelAction::SaveProject
        | PanelAction::SaveProjectAs => {
            log::warn!(
                "File action {:?} reached ui_bridge (should be intercepted in app.rs)",
                action
            );
            DispatchResult::handled()
        }
        PanelAction::ExportVideo | PanelAction::ExportFrame | PanelAction::ExportXml => {
            log::info!("Export action: {:?} (not yet wired)", action);
            DispatchResult::handled()
        }

        // ── Dropdown results (context-routed from UIRoot) ────────────
        PanelAction::SetMidiNote(id, note) => {
            if let Some((_, layer)) = project.timeline.find_layer_by_id(id) {
                let layer_id = layer.layer_id.clone();
                let old_note = layer.midi_note;
                let cmd = manifold_editing::commands::settings::ChangeLayerMidiNoteCommand::new(
                    layer_id, old_note, *note,
                );
                {
                    let mut boxed: Box<dyn manifold_editing::command::Command + Send> =
                        Box::new(cmd);
                    boxed.execute(project);
                    ContentCommand::send(content_tx, ContentCommand::Execute(boxed));
                }
            }
            DispatchResult::structural()
        }
        PanelAction::SetMidiChannel(id, channel) => {
            if let Some((_, layer)) = project.timeline.find_layer_by_id(id) {
                let layer_id = layer.layer_id.clone();
                let old_channel = layer.midi_channel;
                let cmd = manifold_editing::commands::settings::ChangeLayerMidiChannelCommand::new(
                    layer_id,
                    old_channel,
                    *channel,
                );
                {
                    let mut boxed: Box<dyn manifold_editing::command::Command + Send> =
                        Box::new(cmd);
                    boxed.execute(project);
                    ContentCommand::send(content_tx, ContentCommand::Execute(boxed));
                }
            }
            DispatchResult::structural()
        }
        PanelAction::SetMidiDevice(id, device) => {
            if let Some((_, layer)) = project.timeline.find_layer_by_id(id) {
                let layer_id = layer.layer_id.clone();
                let old_device = layer.midi_device.clone();
                let cmd = manifold_editing::commands::settings::ChangeLayerMidiDeviceCommand::new(
                    layer_id,
                    old_device,
                    device.clone(),
                );
                let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd);
                boxed.execute(project);
                ContentCommand::send(content_tx, ContentCommand::Execute(boxed));
            }
            DispatchResult::structural()
        }
        PanelAction::MidiTriggerModeClicked(id) => {
            use manifold_core::types::MidiTriggerMode;
            if let Some((_, layer)) = project.timeline.find_layer_by_id(id) {
                let layer_id = layer.layer_id.clone();
                let old_mode = layer.midi_trigger_mode;
                let new_mode = match old_mode {
                    MidiTriggerMode::SingleNote => MidiTriggerMode::AllNotes,
                    MidiTriggerMode::AllNotes => MidiTriggerMode::SingleNote,
                };
                let cmd =
                    manifold_editing::commands::settings::ChangeLayerMidiTriggerModeCommand::new(
                        layer_id, old_mode, new_mode,
                    );
                let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd);
                boxed.execute(project);
                ContentCommand::send(content_tx, ContentCommand::Execute(boxed));
            }
            DispatchResult::structural()
        }
        PanelAction::SetMidiTriggerMode(id, new_mode) => {
            if let Some((_, layer)) = project.timeline.find_layer_by_id(id) {
                let layer_id = layer.layer_id.clone();
                let old_mode = layer.midi_trigger_mode;
                let cmd =
                    manifold_editing::commands::settings::ChangeLayerMidiTriggerModeCommand::new(
                        layer_id,
                        old_mode,
                        crate::ui_translate::midi_trigger_mode_to_core(*new_mode),
                    );
                let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd);
                boxed.execute(project);
                ContentCommand::send(content_tx, ContentCommand::Execute(boxed));
            }
            DispatchResult::structural()
        }
        PanelAction::SetResolution(preset_idx) => {
            use manifold_core::types::ResolutionPreset;
            let old = project.settings.resolution_preset;
            if let Some(new) = ResolutionPreset::from_index(*preset_idx) {
                let cmd =
                    manifold_editing::commands::settings::ChangeResolutionCommand::new(old, new);
                {
                    let mut boxed: Box<dyn manifold_editing::command::Command + Send> =
                        Box::new(cmd);
                    boxed.execute(project);
                    ContentCommand::send(content_tx, ContentCommand::Execute(boxed));
                }
            }
            DispatchResult::resolution()
        }
        PanelAction::SetDisplayResolution(w, h) => {
            let old_w = project.settings.output_width;
            let old_h = project.settings.output_height;
            let cmd = manifold_editing::commands::settings::SetDisplayDimensionsCommand::new(
                old_w, old_h, *w, *h,
            );
            {
                let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd);
                boxed.execute(project);
                ContentCommand::send(content_tx, ContentCommand::Execute(boxed));
            }
            DispatchResult::resolution()
        }
        PanelAction::SetRenderScale(scale) => {
            let old_scale = project.settings.render_scale;
            let new_scale = scale.clamp(0.01, 1.0);
            if (new_scale - old_scale).abs() > 0.01 {
                let cmd = manifold_editing::commands::settings::ChangeRenderScaleCommand::new(
                    old_scale, new_scale,
                );
                {
                    let mut boxed: Box<dyn manifold_editing::command::Command + Send> =
                        Box::new(cmd);
                    boxed.execute(project);
                    ContentCommand::send(content_tx, ContentCommand::Execute(boxed));
                }
            }
            DispatchResult::resolution()
        }
        PanelAction::SetTonemapCurve(curve) => {
            let old_curve = project.settings.tonemap_curve;
            let curve = crate::ui_translate::tonemap_curve_to_core(*curve);
            if curve != old_curve {
                let cmd = manifold_editing::commands::settings::ChangeTonemapCurveCommand::new(
                    old_curve, curve,
                );
                {
                    let mut boxed: Box<dyn manifold_editing::command::Command + Send> =
                        Box::new(cmd);
                    boxed.execute(project);
                    ContentCommand::send(content_tx, ContentCommand::Execute(boxed));
                }
            }
            DispatchResult::handled()
        }
        PanelAction::SetGenType(opt_layer_id, new_type) => {
            let new_type = crate::ui_translate::preset_type_id_to_core(new_type);
            let resolved_idx = opt_layer_id
                .as_ref()
                .and_then(|lid| project.timeline.find_layer_index_by_id(lid));
            if let Some(layer_idx) = resolved_idx {
                let layer = &project.timeline.layers[layer_idx];
                let old_type = layer
                    .gen_params()
                    .map(|gp| gp.generator_type().clone())
                    .unwrap_or(PresetTypeId::NONE);
                // The action carries the chosen preset id directly (registry
                // entries AND project-embedded presets), so no index lookup.
                if new_type != old_type {
                    let old_params: Vec<f32> = layer
                        .gen_params()
                        .map(|gp| gp.params.iter().map(|s| s.value).collect())
                        .unwrap_or_default();
                    let old_drivers = layer.gen_params().and_then(|gp| gp.drivers.clone());
                    let old_envelopes = layer.gen_params().and_then(|gp| gp.envelopes.clone());
                    let layer_id = layer.layer_id.clone();
                    let cmd =
                        manifold_editing::commands::settings::ChangeGeneratorTypeCommand::new(
                            layer_id.clone(),
                            old_type,
                            new_type.clone(),
                            old_params,
                            old_drivers,
                            old_envelopes,
                        );
                    {
                        let mut boxed: Box<dyn manifold_editing::command::Command + Send> =
                            Box::new(cmd);
                        boxed.execute(project);
                        ContentCommand::send(content_tx, ContentCommand::Execute(boxed));
                    }
                    ContentCommand::send(
                        content_tx,
                        ContentCommand::GeneratorTypeChanged {
                            layer_id,
                            new_type: new_type.clone(),
                        },
                    );
                }
            }
            DispatchResult::structural()
        }

        // ── SCENE_SETUP_PANEL_DESIGN P1: the panel's fourth-surface writes ──
        // All four resolve `GraphTarget::Generator(layer_id)` + the layer's
        // bundled-preset catalog default exactly like
        // `Application::watch_generator_graph` does, then dispatch the SAME
        // command a card/node-face/group-face write would — no new mutation
        // path (§4).
        PanelAction::SceneSetupParamChanged(layer_id, node_doc_id, param_id, value) => {
            if let Some(default) = generator_catalog_default(project, layer_id) {
                let target = manifold_core::GraphTarget::Generator(layer_id.clone());
                let cmd = manifold_editing::commands::graph::SetGraphNodeParamCommand::new(
                    target,
                    *node_doc_id,
                    param_id.clone(),
                    manifold_core::effect_graph_def::SerializedParamValue::Float { value: *value },
                    default,
                );
                let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd);
                boxed.execute(project);
                ContentCommand::send(content_tx, ContentCommand::Execute(boxed));
            }
            DispatchResult::handled()
        }
        PanelAction::SceneSetupAddEnvironment(layer_id, render_scene_node_id) => {
            if let Some(default) = generator_catalog_default(project, layer_id) {
                let target = manifold_core::GraphTarget::Generator(layer_id.clone());
                let cmd = manifold_editing::commands::graph::AddSceneEnvironmentCommand::new(
                    target,
                    Vec::new(),
                    *render_scene_node_id,
                    (0.0, 0.0),
                    default,
                );
                let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd);
                boxed.execute(project);
                ContentCommand::send(content_tx, ContentCommand::Execute(boxed));
            }
            DispatchResult::structural()
        }
        PanelAction::SceneSetupAddFog(layer_id, render_scene_node_id) => {
            if let Some(default) = generator_catalog_default(project, layer_id) {
                let target = manifold_core::GraphTarget::Generator(layer_id.clone());
                let cmd = manifold_editing::commands::graph::AddSceneFogCommand::new(
                    target,
                    Vec::new(),
                    *render_scene_node_id,
                    (0.0, 0.0),
                    default,
                );
                let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd);
                boxed.execute(project);
                ContentCommand::send(content_tx, ContentCommand::Execute(boxed));
            }
            DispatchResult::structural()
        }
        // D7 "New 3D Scene" empty-state action: assign the bundled Scene
        // Starter preset via the SAME `ChangeGeneratorTypeCommand` the
        // browser-popup generator picker's `SetGenType` dispatches (§1 VERIFY
        // marker, resolved).
        PanelAction::SceneSetupNewScene(layer_id) => {
            let new_type = manifold_core::PresetTypeId::from_string("SceneStarter".to_string());
            if let Some((_, layer)) = project.timeline.find_layer_by_id(layer_id) {
                let old_type = layer
                    .gen_params()
                    .map(|gp| gp.generator_type().clone())
                    .unwrap_or(PresetTypeId::NONE);
                if new_type != old_type {
                    let old_params: Vec<f32> = layer
                        .gen_params()
                        .map(|gp| gp.params.iter().map(|s| s.value).collect())
                        .unwrap_or_default();
                    let old_drivers = layer.gen_params().and_then(|gp| gp.drivers.clone());
                    let old_envelopes = layer.gen_params().and_then(|gp| gp.envelopes.clone());
                    let cmd = manifold_editing::commands::settings::ChangeGeneratorTypeCommand::new(
                        layer_id.clone(),
                        old_type,
                        new_type.clone(),
                        old_params,
                        old_drivers,
                        old_envelopes,
                    );
                    let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd);
                    boxed.execute(project);
                    ContentCommand::send(content_tx, ContentCommand::Execute(boxed));
                    ContentCommand::send(
                        content_tx,
                        ContentCommand::GeneratorTypeChanged { layer_id: layer_id.clone(), new_type },
                    );
                }
            }
            DispatchResult::structural()
        }

        _ => DispatchResult::unhandled(),
    }
}

/// Resolve `layer_id`'s generator-graph target's catalog default — the same
/// lookup `Application::watch_generator_graph` performs, factored out so the
/// Scene Setup panel's dispatch arms above don't need the graph editor to be
/// open (they address the layer directly, not `watched_graph_target`).
fn generator_catalog_default(
    project: &Project,
    layer_id: &LayerId,
) -> Option<manifold_core::effect_graph_def::EffectGraphDef> {
    let (_, layer) = project.timeline.find_layer_by_id(layer_id)?;
    let gt = layer.generator_type().clone();
    if gt.is_none() {
        return None;
    }
    let json = manifold_renderer::node_graph::bundled_preset_json(&gt)?;
    serde_json::from_str(&json).ok()
}
