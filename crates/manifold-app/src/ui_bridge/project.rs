//! Project-related dispatch: file operations, export, audio/percussion, resolution,
//! MIDI note/channel, generator type, waveform/stem actions.

use manifold_core::LayerId;
use manifold_core::PresetTypeId;
use manifold_ui::ProjectAction;

use super::DispatchResult;
use crate::app::SelectionState;
use crate::ui_root::UIRoot;

pub(super) fn dispatch_project(
    action: &ProjectAction,
    project: &mut crate::ui_bridge::ProjectRef<'_>,
    content_tx: &crossbeam_channel::Sender<crate::content_command::ContentCommand>,
    _content_state: &crate::content_state::ContentState,
    ui: &mut UIRoot,
    _selection: &mut SelectionState,
    _active_layer: &mut Option<LayerId>,
    user_prefs: &mut crate::user_prefs::UserPrefs,
    dialogs: crate::file_dialogs::DialogHost,
) -> DispatchResult {
    use crate::content_command::ContentCommand;
    match action {
        // ── Export/Header/Footer ───────────────────────────────────
        ProjectAction::ToggleHdr => {
            let old_hdr = project.settings.export_hdr;
            let cmd = manifold_editing::commands::settings::ToggleExportHdrCommand::new(old_hdr);
            {
                let boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd);
                project.execute(boxed);
            }
            log::info!("HDR export → {}", project.settings.export_hdr);
            DispatchResult::handled()
        }
        ProjectAction::ToggleSplitSections => {
            let old_split = project.settings.split_at_markers;
            let cmd =
                manifold_editing::commands::settings::ToggleSplitSectionsCommand::new(old_split);
            {
                let boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd);
                project.execute(boxed);
            }
            log::info!(
                "Split export at markers → {}",
                project.settings.split_at_markers
            );
            DispatchResult::handled()
        }
        ProjectAction::ToggleExportGizmos => {
            let cmd = manifold_editing::commands::settings::ToggleExportGizmosCommand::new(project.settings.export_gizmos);
            let boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd);
            project.execute(boxed);
            DispatchResult::handled()
        }
        ProjectAction::ToggleLiveRecording
        | ProjectAction::SelectAudioInputDevice
        | ProjectAction::SetAudioInputDevice(_)
        | ProjectAction::ToggleMonitor => DispatchResult::handled(),
        ProjectAction::EnterPerformMode => DispatchResult::handled(),

        ProjectAction::NewProject
        | ProjectAction::OpenProject
        | ProjectAction::OpenRecent
        | ProjectAction::SaveProject
        | ProjectAction::SaveProjectAs => {
            log::warn!(
                "File action {:?} reached ui_bridge (should be intercepted in app.rs)",
                action
            );
            DispatchResult::handled()
        }
        ProjectAction::ExportVideo | ProjectAction::CancelExport | ProjectAction::ExportFrame | ProjectAction::ExportXml => {
            log::info!("Export action: {:?} (not yet wired)", action);
            DispatchResult::handled()
        }

        // ── Dropdown results (context-routed from UIRoot) ────────────
        ProjectAction::SetMidiNote(id, note) => {
            if let Some((_, layer)) = project.timeline.find_layer_by_id(id) {
                let layer_id = layer.layer_id.clone();
                let old_note = layer.midi_note;
                let cmd = manifold_editing::commands::settings::ChangeLayerMidiNoteCommand::new(
                    layer_id, old_note, *note,
                );
                {
                    let boxed: Box<dyn manifold_editing::command::Command + Send> =
                        Box::new(cmd);
                    project.execute(boxed);
                }
            }
            DispatchResult::handled()
        }
        ProjectAction::SetMidiChannel(id, channel) => {
            if let Some((_, layer)) = project.timeline.find_layer_by_id(id) {
                let layer_id = layer.layer_id.clone();
                let old_channel = layer.midi_channel;
                let cmd = manifold_editing::commands::settings::ChangeLayerMidiChannelCommand::new(
                    layer_id,
                    old_channel,
                    *channel,
                );
                {
                    let boxed: Box<dyn manifold_editing::command::Command + Send> =
                        Box::new(cmd);
                    project.execute(boxed);
                }
            }
            DispatchResult::handled()
        }
        ProjectAction::SetMidiDevice(id, device) => {
            if let Some((_, layer)) = project.timeline.find_layer_by_id(id) {
                let layer_id = layer.layer_id.clone();
                let old_device = layer.midi_device.clone();
                let cmd = manifold_editing::commands::settings::ChangeLayerMidiDeviceCommand::new(
                    layer_id,
                    old_device,
                    device.clone(),
                );
                let boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd);
                project.execute(boxed);
            }
            DispatchResult::handled()
        }
        ProjectAction::MidiTriggerModeClicked(id) => {
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
                let boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd);
                project.execute(boxed);
            }
            DispatchResult::handled()
        }
        ProjectAction::SetMidiTriggerMode(id, new_mode) => {
            if let Some((_, layer)) = project.timeline.find_layer_by_id(id) {
                let layer_id = layer.layer_id.clone();
                let old_mode = layer.midi_trigger_mode;
                let cmd =
                    manifold_editing::commands::settings::ChangeLayerMidiTriggerModeCommand::new(
                        layer_id,
                        old_mode,
                        crate::ui_translate::midi_trigger_mode_to_core(*new_mode),
                    );
                let boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd);
                project.execute(boxed);
            }
            DispatchResult::handled()
        }
        ProjectAction::SetResolution(preset_idx) => {
            use manifold_core::types::ResolutionPreset;
            if let Some(new) = ResolutionPreset::from_index(*preset_idx) {
                ContentCommand::send(content_tx, ContentCommand::SetResolution(new));
            }
            DispatchResult::handled()
        }
        ProjectAction::SetDisplayResolution(w, h) => {
            ContentCommand::send(content_tx, ContentCommand::SetDisplayResolution(*w, *h));
            DispatchResult::handled()
        }
        ProjectAction::SetSimRate(rate) => {
            if project.settings.physics.sim_rate != *rate {
                let command = manifold_editing::commands::settings::ChangeSimRateCommand::new(
                    project.settings.physics.sim_rate, *rate,
                );
                ContentCommand::send(content_tx, ContentCommand::Execute(Box::new(command)));
            }
            DispatchResult::handled()
        }
        ProjectAction::SetRenderScale(scale) => {
            ContentCommand::send(content_tx, ContentCommand::SetRenderScale(*scale));
            DispatchResult::handled()
        }
        ProjectAction::SetTonemapCurve(curve) => {
            let curve = crate::ui_translate::tonemap_curve_to_core(*curve);
            ContentCommand::send(content_tx, ContentCommand::SetTonemapCurve(curve));
            DispatchResult::handled()
        }
        ProjectAction::SetTonemapEnabled(enabled) => {
            ContentCommand::send(content_tx, ContentCommand::SetTonemapEnabled(*enabled));
            DispatchResult::handled()
        }
        ProjectAction::SetSdrPreview(enabled) => {
            ContentCommand::send(content_tx, ContentCommand::SetSdrPreview(*enabled));
            DispatchResult::handled()
        }
        ProjectAction::ChangeRtQuality(new_settings) => {
            let old_settings = project.settings.rt_quality;
            if *new_settings != old_settings {
                let cmd = manifold_editing::commands::settings::ChangeRtQualityCommand::new(
                    old_settings,
                    *new_settings,
                );
                {
                    let boxed: Box<dyn manifold_editing::command::Command + Send> =
                        Box::new(cmd);
                    project.execute(boxed);
                }
            }
            // The panel's tier labels are baked into the tree at build; the
            // Settings stamp rebuilds it so the click is visible.
            DispatchResult::handled()
        }
        ProjectAction::SetGenType(opt_layer_id, new_type) => {
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
                    ContentCommand::send(
                        content_tx,
                        ContentCommand::ChangeGeneratorType {
                            layer_id: layer.layer_id.clone(),
                            new_type,
                        },
                    );
                }
            }
            DispatchResult::handled()
        }

        // ── The Scene tab (SCENE_DOCUMENT_DESIGN.md section 8): every edit is
        // built here from the document and sent as one undo step.
        // An entry with no fields is "Mesh from file": the native picker, then
        // the same import a drop runs, into this layer.
        ProjectAction::SceneAdd { layer_id, entry, parent, selected } => {
            if manifold_core::scene_schema::add_entry(entry).is_some_and(|found| found.fields.is_none()) {
                let Some(path) = crate::file_dialogs::pick_file(crate::file_dialogs::DialogAction::ImportModel, dialogs, user_prefs) else {
                    return DispatchResult::handled();
                };
                let target = crate::import_worker::ImportTarget::Into { layer: layer_id.clone(), parent: parent.clone() };
                return DispatchResult { import_model: Some((path, target)), ..DispatchResult::handled() };
            }
            let built = crate::scene_edit::add(project, layer_id, entry, parent.as_ref(), selected).map(Some);
            send_scene_edit(content_tx, layer_id, built)
        }
        ProjectAction::SceneRemove(layer_id, item) => {
            let built = crate::scene_edit::remove(project, layer_id, item).map(Some);
            send_scene_edit(content_tx, layer_id, built)
        }
        ProjectAction::SceneDuplicate(layer_id, item) => {
            let built = crate::scene_edit::duplicate(project, layer_id, std::slice::from_ref(item)).map(Some);
            send_scene_edit(content_tx, layer_id, built)
        }
        ProjectAction::SceneGroup(layer_id, items) => {
            let built = crate::scene_edit::group(project, layer_id, items).map(Some);
            send_scene_edit(content_tx, layer_id, built)
        }
        ProjectAction::SceneUngroup(layer_id, group) => {
            let built = crate::scene_edit::ungroup(project, layer_id, group).map(Some);
            send_scene_edit(content_tx, layer_id, built)
        }
        ProjectAction::SceneFieldSet { layer_id, item, field, pick } => {
            let built = crate::scene_edit::set_field(project, layer_id, item, field, pick);
            send_scene_edit(content_tx, layer_id, built)
        }
        ProjectAction::SceneMaterialFeature { layer_id, material, feature, add } => {
            let built = crate::scene_edit::material_feature(project, layer_id, material, *feature, *add);
            send_scene_edit(content_tx, layer_id, built)
        }
        ProjectAction::SceneChainMove { layer_id, item, to } => {
            let built = crate::scene_edit::chain_move(project, layer_id, item, *to).map(Some);
            send_scene_edit(content_tx, layer_id, built)
        }
        ProjectAction::SceneMaterialLook { layer_id, material, look } => {
            let built = crate::scene_edit::material_look(project, layer_id, material, *look);
            send_scene_edit(content_tx, layer_id, built)
        }
        ProjectAction::SceneMakeMaterialUnique(layer_id, owner) => {
            let built = crate::scene_edit::make_material_unique(project, layer_id, owner).map(Some);
            send_scene_edit(content_tx, layer_id, built)
        }
        ProjectAction::SceneSetupAimSunAtHdri(layer_id, file) => {
            let built = crate::scene_edit::aim_sun_at_hdri(project, layer_id, file).map(Some);
            send_scene_edit(content_tx, layer_id, built)
        }
        ProjectAction::SceneSetupFrameSelected(layer_id, item) => {
            let built = crate::scene_edit::frame(project, layer_id, item).map(Some);
            send_scene_edit(content_tx, layer_id, built)
        }
        // A generator layer with no scene takes the base scene.
        ProjectAction::SceneNew(layer_id) => {
            ContentCommand::send(
                content_tx,
                ContentCommand::ChangeGeneratorType { layer_id: layer_id.clone(), new_type: PresetTypeId::SCENE },
            );
            DispatchResult::handled()
        }
        // Model File swap on an imported Group or Object: the picked file
        // replaces the Source and the parts are reconciled by file node path,
        // in one undo step, once the import worker has parsed it off the UI
        // thread. A file that cannot be read, or an item that is no import,
        // toasts.
        ProjectAction::SceneSetupSwapModelFile(layer_id, item) => {
            let Some(path) = crate::file_dialogs::pick_file(
                crate::file_dialogs::DialogAction::ImportModel,
                dialogs,
                user_prefs,
            ) else {
                return DispatchResult::handled();
            };
            let target = crate::import_worker::ImportTarget::Swap { layer: layer_id.clone(), item: item.clone() };
            DispatchResult { import_model: Some((path, target)), ..DispatchResult::handled() }
        }
        // BUG-184: the automation-lane right-click context menu's two items
        // — `ClearLaneCommand`/`RemoveLaneCommand` had zero UI callers before
        // this. Same `to_graph_target` conversion `editing_host.rs`'s
        // automation-point arms use.
        ProjectAction::ContextClearAutomationLane(target, param_id) => {
            ContentCommand::send(content_tx, ContentCommand::FinishAutomationRecording);
            let graph_target = crate::editing_host::to_graph_target(target);
            let cmd = manifold_editing::commands::automation::ClearLaneCommand::new(
                graph_target,
                param_id.as_ref(),
            );
            project.execute(Box::new(cmd));
            DispatchResult::handled()
        }
        ProjectAction::ContextRemoveAutomationLane(target, param_id) => {
            remove_parameter_automation(project, content_tx, ui, _selection, target, param_id);
            DispatchResult::handled()
        }
        ProjectAction::ContextRestoreAutomationLane(target, param_id) => {
            ContentCommand::send(
                content_tx,
                ContentCommand::AutomationResumeParameter(
                    crate::editing_host::to_graph_target(target),
                    param_id.clone(),
                ),
            );
            DispatchResult::handled()
        }
    }
}

/// Shared parameter/lane menu path: remove the envelope and its stale UI targets.
pub(super) fn remove_parameter_automation(
    project: &mut crate::ui_bridge::ProjectRef<'_>,
    content_tx: &crossbeam_channel::Sender<crate::content_command::ContentCommand>,
    ui: &mut UIRoot,
    selection: &mut SelectionState,
    target: &manifold_ui::view::UiGraphTarget,
    param_id: &manifold_core::effects::ParamId,
) {
    let graph_target = crate::editing_host::to_graph_target(target);
    // A pending first recording take can create the lane at this boundary,
    // before the removal resolves its parameter on the content thread.
    crate::content_command::ContentCommand::send(
        content_tx,
        crate::content_command::ContentCommand::FinishAutomationRecording,
    );
    let command = manifold_editing::commands::automation::RemoveLaneCommand::for_param(
        graph_target,
        param_id.as_ref(),
    );
    project.execute(Box::new(command));
    selection.unchoose_automation_lane(target, param_id);
    selection
        .selected_automation_points
        .retain(|point| point.target != *target || point.param_id != *param_id);
    if selection
        .selected_automation_point
        .as_ref()
        .is_some_and(|p| p.target == *target && p.param_id == *param_id)
    {
        selection.selected_automation_point = None;
    }
    if selection
        .automation_paste_context
        .as_ref()
        .is_some_and(|(t, p)| t == target && p == param_id)
    {
        selection.automation_paste_context = None;
    }
    if selection
        .automation_feedback
        .as_ref()
        .is_some_and(|f| f.target == *target && f.param_id == *param_id)
    {
        selection.automation_feedback = None;
    }
    if ui
        .pending_automation_reveal
        .as_ref()
        .is_some_and(|(t, p)| t == target && p == param_id)
    {
        ui.pending_automation_reveal = None;
    }
}

/// Send a built Scene tab edit to the content thread, selecting the item it
/// added; a refusal surfaces as a toast.
pub(crate) fn send_scene_edit(
    content_tx: &crossbeam_channel::Sender<crate::content_command::ContentCommand>,
    layer_id: &LayerId,
    built: Result<Option<crate::scene_edit::SceneEdit>, String>,
) -> DispatchResult {
    use crate::content_command::ContentCommand;
    match built {
        Ok(Some(edit)) => match edit.select {
            Some(item) => ContentCommand::send(
                content_tx,
                ContentCommand::ExecuteSelecting(edit.command, crate::edit_selection::SelectAfterEdit::NewItem(layer_id.clone(), item)),
            ),
            None => ContentCommand::send(content_tx, ContentCommand::ExecuteOnContent(edit.command)),
        },
        Ok(None) => {}
        Err(reason) => ContentCommand::send(content_tx, ContentCommand::GraphEditRejected(reason)),
    }
    DispatchResult::handled()
}
