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
        PanelAction::SceneSetupParamChanged(layer_id, scope_path, node_doc_id, param_id, value) => {
            if let Some(default) = generator_catalog_default(project, layer_id) {
                let target = manifold_core::GraphTarget::Generator(layer_id.clone());
                let cmd = manifold_editing::commands::graph::SetGraphNodeParamCommand::new(
                    target,
                    *node_doc_id,
                    param_id.clone(),
                    manifold_core::effect_graph_def::SerializedParamValue::Float { value: *value },
                    default,
                )
                .with_scope(scope_path.clone());
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
        // P2 "+ Object"/"+ Light" buttons: the SAME `AddSceneObjectCommand`/
        // `AddSceneLightCommand` the graph editor's own canvas buttons
        // dispatch (SCENE_BUILD P5) — no new mutation path. `next_index`
        // rides on the action (the panel reads it off the live Vm's own
        // `object_count`/`light_count`, same source the canvas button uses).
        // The centroid/pos offsets are cosmetic editor-canvas placement only.
        PanelAction::SceneSetupAddObject(layer_id, render_scene_node_id, next_index) => {
            if let Some(default) = generator_catalog_default(project, layer_id) {
                let target = manifold_core::GraphTarget::Generator(layer_id.clone());
                let centroid = (900.0, 200.0 + 40.0 * *next_index as f32);
                let cmd = manifold_editing::commands::graph::AddSceneObjectCommand::new(
                    target,
                    Vec::new(),
                    *render_scene_node_id,
                    *next_index,
                    centroid,
                    default,
                );
                let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd);
                boxed.execute(project);
                ContentCommand::send(content_tx, ContentCommand::Execute(boxed));
            }
            DispatchResult::structural()
        }
        PanelAction::SceneSetupAddLight(layer_id, render_scene_node_id, next_index) => {
            if let Some(default) = generator_catalog_default(project, layer_id) {
                let target = manifold_core::GraphTarget::Generator(layer_id.clone());
                let pos = (-260.0, 50.0 + 40.0 * *next_index as f32);
                let cmd = manifold_editing::commands::graph::AddSceneLightCommand::new(
                    target,
                    Vec::new(),
                    *render_scene_node_id,
                    *next_index,
                    pos,
                    default,
                );
                let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd);
                boxed.execute(project);
                ContentCommand::send(content_tx, ContentCommand::Execute(boxed));
            }
            DispatchResult::structural()
        }
        // P4 "Import Model…" button (D5): a native file dialog picks a
        // second `.glb`/`.gltf`, `merge_import_into_graph` (via the public
        // `assemble_merge_plan` wrapper — the assembler's own summary type
        // is crate-private, same constraint as `OBJECT_SAFETY_MAX`) builds
        // a `MergePlan` against the layer's CURRENT effective def, and
        // `ImportModelIntoSceneCommand` applies it as one undo unit. Loud
        // failure (log + no-op), never a silent partial merge — same
        // posture as `Application::import_model_file`'s own parse-failure
        // branch.
        PanelAction::SceneSetupImportModelClicked(layer_id, render_scene_node_id) => {
            let Some(default) = generator_catalog_default(project, layer_id) else {
                return DispatchResult::handled();
            };
            let effective_def = project
                .timeline
                .find_layer_by_id(layer_id)
                .and_then(|(_, layer)| layer.generator_graph().cloned())
                .unwrap_or_else(|| default.clone());

            let Some(path) = rfd::FileDialog::new().add_filter("glTF", &["glb", "gltf"]).pick_file()
            else {
                return DispatchResult::handled();
            };

            let plan = match manifold_renderer::node_graph::gltf_import::assemble_merge_plan(
                &effective_def,
                &path,
            ) {
                Ok(plan) => plan,
                Err(e) => {
                    log::warn!(
                        "[Scene Setup] Import Model… merge failed for {}: {e}",
                        path.display()
                    );
                    return DispatchResult::handled();
                }
            };
            debug_assert_eq!(
                plan.render_scene_node_id, *render_scene_node_id,
                "the freshly-built plan must target the SAME render_scene the Vm/action carried"
            );

            let target = manifold_core::GraphTarget::Generator(layer_id.clone());
            let cmd = manifold_editing::commands::graph::ImportModelIntoSceneCommand::new(
                target,
                Vec::new(),
                plan.render_scene_node_id,
                plan.new_nodes,
                plan.new_wires,
                plan.new_objects_count,
                plan.new_card_params,
                plan.new_card_bindings,
                plan.new_string_bindings,
                default,
            );
            let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd);
            boxed.execute(project);
            ContentCommand::send(content_tx, ContentCommand::Execute(boxed));
            if !plan.report_lines.is_empty() {
                log::info!("[Scene Setup] Import Model… report: {}", plan.report_lines.join("; "));
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
/// `pub(crate)` + re-exported from `ui_bridge` (see `mod.rs`) so
/// `Application::handle_text_input_commit`'s `SceneObjectRename` arm can
/// reuse it too — the panel's rename commit is the same "address the layer
/// directly" shape as the four arms below.
pub(crate) fn generator_catalog_default(
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

#[cfg(test)]
mod tests {
    //! SCENE_SETUP_PANEL_DESIGN.md P2 gate: "add-object button emits
    //! AddSceneObjectCommand" — proven end to end through the SAME
    //! `dispatch_project` entry point the panel's "+ Object"/"+ Light"
    //! clicks reach (`ui_bridge::dispatch` routes `SceneSetupAddObject`/
    //! `SceneSetupAddLight` here, per `mod.rs`'s routing list), not just the
    //! command's own already-covered unit test in `manifold-editing`.
    use super::*;
    use manifold_core::effect_graph_def::SerializedParamValue;
    use manifold_core::types::LayerType;

    fn scene_layer_project() -> (Project, LayerId, u32) {
        let mut project = Project::default();
        let idx = project.timeline.add_layer(
            "Scene",
            LayerType::Generator,
            PresetTypeId::from_string("SceneStarter".to_string()),
        );
        let layer_id = project.timeline.layers[idx].layer_id.clone();
        let def = manifold_renderer::node_graph::bundled_preset_def(
            &project.timeline.layers[idx].generator_type().clone(),
        )
        .expect("SceneStarter is a bundled preset");
        let render_scene_id = def
            .nodes
            .iter()
            .find(|n| n.type_id == manifold_renderer::node_graph::scene_vm::RENDER_SCENE_TYPE_ID)
            .expect("SceneStarter has a render_scene node")
            .id;
        (project, layer_id, render_scene_id)
    }

    /// The layer's CURRENT effective def — the per-instance override once
    /// one exists (post-edit), falling back to the bundled catalog default
    /// beforehand (pre-edit: a fresh `SceneStarter` layer has no override
    /// yet, exactly why `AddSceneObjectCommand` needs a `catalog_default` to
    /// lift one — same resolution `state_sync.rs`'s panel-Vm builder uses).
    fn effective_def(project: &Project, layer_id: &LayerId) -> manifold_core::effect_graph_def::EffectGraphDef {
        let (_, layer) = project.timeline.find_layer_by_id(layer_id).unwrap();
        layer.generator_graph().cloned().unwrap_or_else(|| {
            manifold_renderer::node_graph::bundled_preset_def(&layer.generator_type().clone())
                .cloned()
                .expect("SceneStarter is a bundled preset")
        })
    }

    fn objects_param(project: &Project, layer_id: &LayerId, render_scene_id: u32) -> f32 {
        let graph = effective_def(project, layer_id);
        let scene = graph.nodes.iter().find(|n| n.id == render_scene_id).unwrap();
        match scene.params.get("objects") {
            Some(SerializedParamValue::Float { value }) => *value,
            _ => 0.0,
        }
    }

    fn lights_param(project: &Project, layer_id: &LayerId, render_scene_id: u32) -> f32 {
        let graph = effective_def(project, layer_id);
        let scene = graph.nodes.iter().find(|n| n.id == render_scene_id).unwrap();
        match scene.params.get("lights") {
            Some(SerializedParamValue::Float { value }) => *value,
            _ => 0.0,
        }
    }

    /// Minimal harness for `dispatch_project`'s unused-outside-the-matched-
    /// arms params (`_content_state`/`_ui`/`_selection`/`_active_layer`/
    /// `_user_prefs`) — none of the four Scene Setup arms touch them.
    fn dispatch_harness() -> (
        crossbeam_channel::Sender<crate::content_command::ContentCommand>,
        crate::content_state::ContentState,
        UIRoot,
        SelectionState,
        Option<LayerId>,
        UserPrefs,
    ) {
        (
            crossbeam_channel::unbounded().0,
            crate::content_state::ContentState::default(),
            UIRoot::new(),
            manifold_ui::UIState::new(),
            None,
            UserPrefs::load(),
        )
    }

    #[test]
    fn scene_setup_add_object_dispatches_add_scene_object_command() {
        let (mut project, layer_id, render_scene_id) = scene_layer_project();
        let before = objects_param(&project, &layer_id, render_scene_id);
        let (content_tx, content_state, mut ui, mut selection, mut active_layer, mut user_prefs) =
            dispatch_harness();

        let action =
            PanelAction::SceneSetupAddObject(layer_id.clone(), render_scene_id, before as u32);
        let result = dispatch_project(
            &action,
            &mut project,
            &content_tx,
            &content_state,
            &mut ui,
            &mut selection,
            &mut active_layer,
            &mut user_prefs,
        );
        assert!(result.structural_change, "adding an object is a structural graph edit");
        assert_eq!(objects_param(&project, &layer_id, render_scene_id), before + 1.0);
    }

    #[test]
    fn scene_setup_add_light_dispatches_add_scene_light_command() {
        let (mut project, layer_id, render_scene_id) = scene_layer_project();
        let before = lights_param(&project, &layer_id, render_scene_id);
        let (content_tx, content_state, mut ui, mut selection, mut active_layer, mut user_prefs) =
            dispatch_harness();

        let action =
            PanelAction::SceneSetupAddLight(layer_id.clone(), render_scene_id, before as u32);
        let result = dispatch_project(
            &action,
            &mut project,
            &content_tx,
            &content_state,
            &mut ui,
            &mut selection,
            &mut active_layer,
            &mut user_prefs,
        );
        assert!(result.structural_change, "adding a light is a structural graph edit");
        assert_eq!(lights_param(&project, &layer_id, render_scene_id), before + 1.0);
    }

    /// "rename emits the sweep command": `generator_catalog_default` +
    /// `RenameGroupCommand` is the EXACT pair `Application::
    /// handle_text_input_commit`'s `SceneObjectRename` arm calls — proven
    /// here against a real project/layer instead of only via
    /// `RenameGroupCommand`'s own already-covered unit tests, so the panel's
    /// specific "resolve by layer_id, not watched_graph_target" wiring is
    /// what's actually under test.
    #[test]
    fn generator_catalog_default_plus_rename_group_command_renames_the_object() {
        let (mut project, layer_id, render_scene_id) = scene_layer_project();
        let def = generator_catalog_default(&project, &layer_id).expect("resolves for a live layer");
        let group_node_id = def
            .nodes
            .iter()
            .find(|n| n.group.is_some())
            .expect("SceneStarter has at least one named object group")
            .id;

        let target = manifold_core::GraphTarget::Generator(layer_id.clone());
        let mut cmd = manifold_editing::commands::graph::RenameGroupCommand::new(
            target,
            Vec::new(),
            group_node_id,
            "Hero".to_string(),
            def,
        );
        use manifold_editing::command::Command;
        cmd.execute(&mut project);

        let (_, layer) = project.timeline.find_layer_by_id(&layer_id).unwrap();
        let graph = layer.generator_graph().unwrap();
        let renamed = graph.nodes.iter().find(|n| n.id == group_node_id).unwrap();
        assert_eq!(renamed.handle.as_deref(), Some("Hero"));
        // render_scene_id untouched by the rename — sanity that the harness
        // resolved the right node.
        assert!(graph.nodes.iter().any(|n| n.id == render_scene_id));
    }
}
