//! Project-related dispatch: file operations, export, audio/percussion, resolution,
//! MIDI note/channel, generator type, waveform/stem actions.

use manifold_core::LayerId;
use manifold_core::PresetTypeId;
use manifold_core::project::Project;
use manifold_editing::command::Command;
use manifold_ui::ProjectAction;

use super::DispatchResult;
use crate::app::SelectionState;
use crate::ui_root::UIRoot;
use crate::user_prefs::UserPrefs;

pub(super) fn dispatch_project(
    action: &ProjectAction,
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
        ProjectAction::ToggleHdr => {
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
        ProjectAction::ExportVideo | ProjectAction::ExportFrame | ProjectAction::ExportXml => {
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
                    let mut boxed: Box<dyn manifold_editing::command::Command + Send> =
                        Box::new(cmd);
                    boxed.execute(project);
                    ContentCommand::send(content_tx, ContentCommand::Execute(boxed));
                }
            }
            DispatchResult::structural()
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
                    let mut boxed: Box<dyn manifold_editing::command::Command + Send> =
                        Box::new(cmd);
                    boxed.execute(project);
                    ContentCommand::send(content_tx, ContentCommand::Execute(boxed));
                }
            }
            DispatchResult::structural()
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
                let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd);
                boxed.execute(project);
                ContentCommand::send(content_tx, ContentCommand::Execute(boxed));
            }
            DispatchResult::structural()
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
                let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd);
                boxed.execute(project);
                ContentCommand::send(content_tx, ContentCommand::Execute(boxed));
            }
            DispatchResult::structural()
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
                let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd);
                boxed.execute(project);
                ContentCommand::send(content_tx, ContentCommand::Execute(boxed));
            }
            DispatchResult::structural()
        }
        ProjectAction::SetResolution(preset_idx) => {
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
        ProjectAction::SetDisplayResolution(w, h) => {
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
        ProjectAction::SetRenderScale(scale) => {
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
        ProjectAction::SetTonemapCurve(curve) => {
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
        ProjectAction::ChangeRtQuality(new_settings) => {
            let old_settings = project.settings.rt_quality;
            if *new_settings != old_settings {
                let cmd = manifold_editing::commands::settings::ChangeRtQualityCommand::new(
                    old_settings, *new_settings,
                );
                {
                    let mut boxed: Box<dyn manifold_editing::command::Command + Send> =
                        Box::new(cmd);
                    boxed.execute(project);
                    ContentCommand::send(content_tx, ContentCommand::Execute(boxed));
                }
            }
            // The panel's tier labels are baked into the tree at build —
            // structural so the overlay rebuilds and the click is visible.
            DispatchResult::structural()
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
        // path (section 4).
        ProjectAction::SceneSetupParamChanged(layer_id, scope_path, node_doc_id, param_id, value) => {
            if let Some(cmd) =
                apply_scene_param_write(project, layer_id, scope_path.clone(), *node_doc_id, param_id, *value)
            {
                ContentCommand::send(content_tx, ContentCommand::Execute(cmd));
            }
            DispatchResult::handled()
        }
        ProjectAction::SceneSetupAddEnvironment(layer_id, render_scene_node_id) => {
            if let Some(default) = generator_catalog_default(project, layer_id) {
                let target = manifold_core::GraphTarget::Generator(layer_id.clone());
                let cmd = manifold_editing::commands::graph::AddSceneEnvironmentCommand::new(
                    target,
                    Vec::new(),
                    *render_scene_node_id,
                    (0.0, 0.0),
                    manifold_renderer::node_graph::scene_exposure::metadata_for_node_type("node.bake_environment"),
                    default,
                );
                let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd);
                boxed.execute(project);
                ContentCommand::send(content_tx, ContentCommand::Execute(boxed));
            }
            DispatchResult::structural()
        }
        ProjectAction::SceneSetupAddFog(layer_id, render_scene_node_id) => {
            if let Some(default) = generator_catalog_default(project, layer_id) {
                let target = manifold_core::GraphTarget::Generator(layer_id.clone());
                let cmd = manifold_editing::commands::graph::AddSceneFogCommand::new(
                    target,
                    Vec::new(),
                    *render_scene_node_id,
                    (0.0, 0.0),
                    manifold_renderer::node_graph::scene_exposure::metadata_for_node_type("node.atmosphere"),
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
        ProjectAction::SceneSetupAddObject(layer_id, render_scene_node_id, next_index) => {
            if let Some(default) = generator_catalog_default(project, layer_id) {
                let target = manifold_core::GraphTarget::Generator(layer_id.clone());
                let centroid = (900.0, 200.0 + 40.0 * *next_index as f32);
                let cmd = manifold_editing::commands::graph::AddSceneObjectCommand::new(
                    target,
                    Vec::new(),
                    *render_scene_node_id,
                    *next_index,
                    centroid,
                    manifold_renderer::node_graph::scene_exposure::metadata_for_node_type("node.phong_material"),
                    manifold_renderer::node_graph::scene_exposure::metadata_for_node_type("node.transform_3d"),
                    manifold_renderer::node_graph::scene_exposure::metadata_for_node_type("node.scene_object"),
                    default,
                );
                let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd);
                boxed.execute(project);
                ContentCommand::send(content_tx, ContentCommand::Execute(boxed));
            }
            DispatchResult::structural()
        }
        ProjectAction::SceneSetupAddLight(layer_id, render_scene_node_id, next_index) => {
            if let Some(default) = generator_catalog_default(project, layer_id) {
                let target = manifold_core::GraphTarget::Generator(layer_id.clone());
                let pos = (-260.0, 50.0 + 40.0 * *next_index as f32);
                let cmd = manifold_editing::commands::graph::AddSceneLightCommand::new(
                    target,
                    Vec::new(),
                    *render_scene_node_id,
                    *next_index,
                    pos,
                    manifold_renderer::node_graph::scene_exposure::metadata_for_node_type("node.light"),
                    default,
                );
                let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd);
                boxed.execute(project);
                ContentCommand::send(content_tx, ContentCommand::Execute(boxed));
            }
            DispatchResult::structural()
        }
        // BUG-193 per-row "✕": the inverse of SceneSetupAddObject/
        // SceneSetupAddLight above — `object_index`/`light_index` ride on
        // the action exactly as `next_index` does for the Add commands (the
        // panel's own live Vm row index, not re-derived here).
        ProjectAction::SceneSetupRemoveObject(layer_id, render_scene_node_id, object_index) => {
            if let Some(default) = generator_catalog_default(project, layer_id) {
                let target = manifold_core::GraphTarget::Generator(layer_id.clone());
                let cmd = manifold_editing::commands::graph::RemoveSceneObjectCommand::new(
                    target,
                    Vec::new(),
                    *render_scene_node_id,
                    *object_index,
                    default,
                );
                let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd);
                boxed.execute(project);
                ContentCommand::send(content_tx, ContentCommand::Execute(boxed));
            }
            DispatchResult::structural()
        }
        ProjectAction::SceneSetupRemoveLight(layer_id, render_scene_node_id, light_index) => {
            if let Some(default) = generator_catalog_default(project, layer_id) {
                let target = manifold_core::GraphTarget::Generator(layer_id.clone());
                let cmd = manifold_editing::commands::graph::RemoveSceneLightCommand::new(
                    target,
                    Vec::new(),
                    *render_scene_node_id,
                    *light_index,
                    default,
                );
                let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd);
                boxed.execute(project);
                ContentCommand::send(content_tx, ContentCommand::Execute(boxed));
            }
            DispatchResult::structural()
        }

        // P4b: Skin row source/target edits — same shape as the modifier-stack
        // arms above: resolve `GraphTarget::Generator(layer_id)`, grab the
        // catalog default, execute locally + send to content thread.
        ProjectAction::SceneSetupSkinSourceSet {
            layer_id,
            scope_path,
            scene_object_id,
            source_node_id,
            target_map,
            source,
        } => {
            if let Some(default) = generator_catalog_default(project, layer_id) {
                let target = manifold_core::GraphTarget::Generator(layer_id.clone());
                let source_string = source.as_ref().map(|id| id.to_string());
                let cmd = manifold_editing::commands::graph::SetSceneObjectSkinSourceCommand::new(
                    target,
                    scope_path.clone(),
                    *scene_object_id,
                    *source_node_id,
                    map_skin_target_map(*target_map),
                    source_string,
                    default,
                );
                let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd);
                boxed.execute(project);
                ContentCommand::send(content_tx, ContentCommand::Execute(boxed));
            }
            DispatchResult::structural()
        }
        ProjectAction::SceneSetupSkinTargetMapSet {
            layer_id,
            scope_path,
            scene_object_id,
            source_node_id,
            target_map,
        } => {
            if let Some(default) = generator_catalog_default(project, layer_id) {
                let target = manifold_core::GraphTarget::Generator(layer_id.clone());
                let cmd = manifold_editing::commands::graph::SetSceneObjectSkinTargetMapCommand::new(
                    target,
                    scope_path.clone(),
                    *scene_object_id,
                    *source_node_id,
                    map_skin_target_map(*target_map),
                    default,
                );
                let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd);
                boxed.execute(project);
                ContentCommand::send(content_tx, ContentCommand::Execute(boxed));
            }
            DispatchResult::structural()
        }

        // P5 properties-header "Duplicate" (Object selection, D11): the same
        // `DuplicateSceneObjectCommand` construction shape as
        // `SceneSetupRemoveObject` above.
        ProjectAction::SceneSetupDuplicateObject(layer_id, render_scene_node_id, source_index) => {
            if let Some(default) = generator_catalog_default(project, layer_id) {
                let target = manifold_core::GraphTarget::Generator(layer_id.clone());
                let cmd = manifold_editing::commands::graph::DuplicateSceneObjectCommand::new(
                    target,
                    Vec::new(),
                    *render_scene_node_id,
                    *source_index,
                    default,
                );
                let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd);
                boxed.execute(project);
                ContentCommand::send(content_tx, ContentCommand::Execute(boxed));
            }
            DispatchResult::structural()
        }
        // scene-panel-ux: "Frame" button (Object selection). Reads the
        // effective def through the SAME SceneVm the panel builds, takes the
        // object's current translate as the focus point, and writes camera
        // params through `apply_scene_param_write` — the one write path every
        // scene-panel control shares (bound → binding slot, else def write).
        // All writes land as ONE CompositeCommand so a frame is one undo.
        ProjectAction::SceneSetupFrameSelected(layer_id, _render_scene_node_id, object_index) => {
            use manifold_renderer::node_graph::scene_vm::{CameraVm, SceneObjectVm, SceneVm};
            let Some(default) = generator_catalog_default(project, layer_id) else {
                return DispatchResult::handled();
            };
            // The layer's override graph when it has one — the panel's VM
            // reads the same effective def, so framing agrees with the rows
            // the user was looking at when they clicked.
            let effective = project
                .timeline
                .find_layer_by_id(layer_id)
                .and_then(|(_, l)| l.generator_graph().cloned())
                .unwrap_or_else(|| default.clone());
            let Some(vm) = SceneVm::from_def(&effective) else {
                eprintln!("[Scene] frame-selected: no scene in this graph");
                return DispatchResult::handled();
            };
            let Some(pos) = vm.objects.iter().find_map(|o| match o {
                SceneObjectVm::Known(r) if r.index == *object_index => {
                    r.transform.as_ref().map(|t| t.pos_value)
                }
                _ => None,
            }) else {
                eprintln!("[Scene] frame-selected: object {object_index} has no transform row");
                return DispatchResult::handled();
            };
            // Scene radius from the item-2 bounds chain: half the largest
            // axis extent, floored at 1.0 — the scale the importer framed at.
            let radius = vm
                .scene_bounds
                .map(|(mn, mx)| (0..3).map(|a| (mx[a] - mn[a]) * 0.5).fold(1.0f32, f32::max))
                .unwrap_or(1.0);
            let distance = 2.2 * radius;
            let mut writes: Vec<Box<dyn manifold_editing::command::Command + Send>> = Vec::new();
            match &vm.camera {
                // camera_orbit pivots at (0, look_y, 0) — it has no target
                // params, so exact aim is impossible. Frame = pull back far
                // enough that the object at its offset from the pivot fits
                // (distance covers radius + horizontal offset), and lift the
                // pivot to the object's height.
                CameraVm::Orbit(row) => {
                    let offset = (pos.0 * pos.0 + pos.2 * pos.2).sqrt();
                    let d = 2.2 * (radius + offset);
                    for (pid, v) in [("look_y", pos.1), ("distance", d)] {
                        if let Some(cmd) = apply_scene_param_write(
                            project,
                            layer_id,
                            Vec::new(),
                            row.node_doc_id,
                            pid,
                            v,
                        ) {
                            writes.push(cmd);
                        }
                    }
                }
                CameraVm::LookAt(row) => {
                    let node = effective.nodes.iter().find(|n| n.id == row.node_doc_id);
                    let pf = |name: &str, dflt: f32| {
                        node.and_then(|n| n.params.get(name))
                            .and_then(|v| match v {
                                manifold_core::effect_graph_def::SerializedParamValue::Float { value } => Some(*value),
                                _ => None,
                            })
                            .unwrap_or(dflt)
                    };
                    let cur_pos = (pf("pos_x", 0.0), pf("pos_y", 0.0), pf("pos_z", 0.0));
                    let cur_tgt = (pf("target_x", 0.0), pf("target_y", 0.0), pf("target_z", 0.0));
                    let mut dir = (
                        cur_tgt.0 - cur_pos.0,
                        cur_tgt.1 - cur_pos.1,
                        cur_tgt.2 - cur_pos.2,
                    );
                    let len = (dir.0 * dir.0 + dir.1 * dir.1 + dir.2 * dir.2).sqrt();
                    // Degenerate view (camera sitting on its target): fall
                    // back to looking down -Z rather than producing NaNs.
                    dir = if len > 1e-6 { (dir.0 / len, dir.1 / len, dir.2 / len) } else { (0.0, 0.0, -1.0) };
                    let new_pos = (
                        pos.0 - dir.0 * distance,
                        pos.1 - dir.1 * distance,
                        pos.2 - dir.2 * distance,
                    );
                    for (pid, v) in [
                        ("target_x", pos.0),
                        ("target_y", pos.1),
                        ("target_z", pos.2),
                        ("pos_x", new_pos.0),
                        ("pos_y", new_pos.1),
                        ("pos_z", new_pos.2),
                    ] {
                        if let Some(cmd) = apply_scene_param_write(
                            project,
                            layer_id,
                            Vec::new(),
                            row.node_doc_id,
                            pid,
                            v,
                        ) {
                            writes.push(cmd);
                        }
                    }
                }
                CameraVm::Free(_) | CameraVm::Custom { .. } | CameraVm::None => {
                    eprintln!("[Scene] frame-selected unsupported for this camera type");
                    return DispatchResult::handled();
                }
            }
            if writes.is_empty() {
                return DispatchResult::handled();
            }
            // One undo unit for the whole camera move (ExecuteBatch records
            // the batch; the local write already happened per-param inside
            // apply_scene_param_write, same as the single-slider path).
            let batch: Vec<Box<dyn manifold_editing::command::Command>> =
                writes.into_iter().map(|c| c as Box<dyn manifold_editing::command::Command>).collect();
            ContentCommand::send(
                content_tx,
                ContentCommand::ExecuteBatch(batch, "Frame camera on object".to_string()),
            );
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
        ProjectAction::SceneSetupImportModelClicked(layer_id, render_scene_node_id) => {
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
        // ── SCENE_SETUP_PANEL_DESIGN P5: the modifier stack (D6) ──
        // All three resolve `GraphTarget::Generator(layer_id)` exactly like
        // the P1/P2 arms above — no new mutation path, just the three named
        // composites `InsertMeshModifierCommand`/`RemoveMeshModifierCommand`/
        // `MoveMeshModifierCommand`.
        ProjectAction::SceneSetupAddModifier(layer_id, group_node_id, type_id) => {
            if let Some(default) = generator_catalog_default(project, layer_id) {
                let target = manifold_core::GraphTarget::Generator(layer_id.clone());
                let cmd = manifold_editing::commands::graph::InsertMeshModifierCommand::new(
                    target,
                    Vec::new(),
                    *group_node_id,
                    type_id.clone(),
                    None,
                    manifold_renderer::node_graph::scene_exposure::metadata_for_node_type(type_id),
                    default,
                );
                let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd);
                boxed.execute(project);
                ContentCommand::send(content_tx, ContentCommand::Execute(boxed));
            }
            DispatchResult::structural()
        }
        ProjectAction::SceneSetupRemoveModifier(layer_id, group_node_id, modifier_node_id) => {
            if let Some(default) = generator_catalog_default(project, layer_id) {
                let target = manifold_core::GraphTarget::Generator(layer_id.clone());
                let cmd = manifold_editing::commands::graph::RemoveMeshModifierCommand::new(
                    target,
                    Vec::new(),
                    *group_node_id,
                    *modifier_node_id,
                    default,
                );
                let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd);
                boxed.execute(project);
                ContentCommand::send(content_tx, ContentCommand::Execute(boxed));
            }
            DispatchResult::structural()
        }
        ProjectAction::SceneSetupMoveModifier(layer_id, group_node_id, modifier_node_id, new_position) => {
            if let Some(default) = generator_catalog_default(project, layer_id) {
                let target = manifold_core::GraphTarget::Generator(layer_id.clone());
                let cmd = manifold_editing::commands::graph::MoveMeshModifierCommand::new(
                    target,
                    Vec::new(),
                    *group_node_id,
                    *modifier_node_id,
                    *new_position as usize,
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
        // browser-popup generator picker's `SetGenType` dispatches (section 1 VERIFY
        // marker, resolved).
        ProjectAction::SceneSetupNewScene(layer_id) => {
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

        // BUG-184: the automation-lane right-click context menu's two items
        // — `ClearLaneCommand`/`RemoveLaneCommand` had zero UI callers before
        // this. Same `to_graph_target` conversion `editing_host.rs`'s
        // automation-point arms use.
        ProjectAction::ContextClearAutomationLane(target, param_id) => {
            let graph_target = crate::editing_host::to_graph_target(target);
            let mut cmd = manifold_editing::commands::automation::ClearLaneCommand::new(
                graph_target,
                param_id.as_ref(),
            );
            cmd.execute(project);
            ContentCommand::send(content_tx, ContentCommand::Execute(Box::new(cmd)));
            DispatchResult::structural()
        }
        ProjectAction::ContextRemoveAutomationLane(target, param_id) => {
            let graph_target = crate::editing_host::to_graph_target(target);
            let param_id_str = param_id.as_ref();
            let index = project.preset_instance(&graph_target).and_then(|inst| {
                inst.automation_lanes
                    .as_ref()
                    .and_then(|lanes| lanes.iter().position(|l| l.param_id.as_ref() == param_id_str))
            });
            if let Some(index) = index {
                let mut cmd = manifold_editing::commands::automation::RemoveLaneCommand::new(
                    graph_target,
                    param_id_str,
                    index,
                );
                cmd.execute(project);
                ContentCommand::send(content_tx, ContentCommand::Execute(Box::new(cmd)));
            }
            DispatchResult::structural()
        }

        // UX-P3a (SCENE_PANEL_UX_DESIGN.md D8, sizing amendment): expose the
        // scene row's inner param on the layer's generator card via the SAME
        // `ToggleNodeParamExposeCommand` the graph editor's expose glyph
        // dispatches (`app_render.rs`'s `GraphEditCommand::ToggleNodeParamExpose`
        // handling), constructed here instead of there because the scene
        // panel never opens the graph editor's canvas (no `watched_graph_target`
        // to piggyback on) — same "resolve `GraphTarget::Generator` + the
        // bundled catalog default" shape as every other fourth-surface write
        // in this file.
        //
        // One-way per P3a: if the param is ALREADY exposed (read via
        // `scene_vm::is_param_exposed` — same "free read off the def" the
        // panel's own `RowValue::exposed` uses), this is a no-op — a second
        // click never un-exposes and never mints a second binding. The panel
        // emits regardless of lit state (see the action's own doc), so this
        // guard is the actual one-way enforcement point.
        ProjectAction::SceneSetupExposeParam {
            layer_id,
            scope_path,
            node_doc_id,
            param_id,
            object_label,
            param_label,
            min,
            max,
            default_value,
            is_angle,
        } => {
            let Some(default) = generator_catalog_default(project, layer_id) else {
                return DispatchResult::handled();
            };
            let effective_def = project
                .timeline
                .find_layer_by_id(layer_id)
                .and_then(|(_, layer)| layer.generator_graph().cloned())
                .unwrap_or_else(|| default.clone());
            if manifold_renderer::node_graph::scene_vm::is_param_exposed(&effective_def, *node_doc_id, param_id) {
                return DispatchResult::handled();
            }
            let Some(node) = find_node_by_scope(&effective_def, scope_path, *node_doc_id) else {
                return DispatchResult::handled();
            };
            let node_id = node.node_id.clone();
            let node_handle = node.handle.clone().unwrap_or_else(|| format!("node{node_doc_id}"));
            let target = manifold_core::GraphTarget::Generator(layer_id.clone());
            let cmd = manifold_editing::commands::graph::ToggleNodeParamExposeCommand::new(
                target,
                node_id,
                *node_doc_id,
                node_handle,
                param_id.clone(),
                true,
                default,
                format!("{object_label} \u{b7} {param_label}"),
                *min,
                *max,
                *default_value,
                manifold_core::effects::ParamConvert::Float,
                *is_angle,
                Vec::new(),
            )
            .with_scope(scope_path.clone());
            let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd);
            boxed.execute(project);
            ContentCommand::send(content_tx, ContentCommand::Execute(boxed));
            DispatchResult::structural()
        }

    }
}

/// Find the `EffectGraphNode` with doc id `node_doc_id` at `scope_path`
/// (a path of group-node doc ids to descend into, empty = document root) —
/// the same addressing every graph command's `.with_scope` takes. Used by
/// `SceneSetupExposeParam` to read the node's stable `node_id`/`handle`
/// before constructing `ToggleNodeParamExposeCommand`, which (unlike every
/// other fourth-surface command in this file) needs that identity as a
/// constructor argument rather than resolving it internally. BUG-249:
/// `pub(crate)` so `inspector.rs`'s scene modulation redirect can resolve
/// the same identity when it materializes an exposure on first arm.
pub(crate) fn find_node_by_scope<'a>(
    def: &'a manifold_core::effect_graph_def::EffectGraphDef,
    scope_path: &[u32],
    node_doc_id: u32,
) -> Option<&'a manifold_core::effect_graph_def::EffectGraphNode> {
    let mut nodes = def.nodes.as_slice();
    for group_id in scope_path {
        let group_node = nodes.iter().find(|n| n.id == *group_id)?;
        nodes = &group_node.group.as_ref()?.nodes;
    }
    nodes.iter().find(|n| n.id == node_doc_id)
}

/// Resolve `layer_id`'s generator-graph target's catalog default — the same
/// lookup `Application::watch_generator_graph` performs, factored out so the
/// Scene Setup panel's dispatch arms above don't need the graph editor to be
/// open (they address the layer directly, not `watched_graph_target`).
/// `pub(crate)` + re-exported from `ui_bridge` (see `mod.rs`) so
/// `Application::handle_text_input_commit`'s `SceneObjectRename` arm can
/// reuse it too — the panel's rename commit is the same "address the layer
/// directly" shape as the four arms below.
/// The ONE write path every scene-panel param change takes. Bound param →
/// edit the binding's instance slot, never the def — a def write on a bound
/// param is re-seeded over on rebuild (the importer-camera deadness this
/// guards against). Unbound → def-level `SetGraphNodeParamCommand`. Applies
/// the local write itself and returns the command for the content thread —
/// sent singly by `SceneSetupParamChanged`, or batched under one
/// `CompositeCommand` by frame-selected so a camera frame is one undo unit.
/// `None` when the layer has no generator default or the bound value is
/// unchanged (the epsilon no-change guard).
fn apply_scene_param_write(
    project: &mut Project,
    layer_id: &LayerId,
    scope_path: Vec<u32>,
    node_doc_id: u32,
    param_id: &str,
    value: f32,
) -> Option<Box<dyn manifold_editing::command::Command + Send>> {
    let default = generator_catalog_default(project, layer_id)?;
    let target = manifold_core::GraphTarget::Generator(layer_id.clone());
    let bound = project
        .with_preset_graph_mut(&target, |inst| inst.binding_id_for_node_param(node_doc_id, param_id))
        .flatten()
        // Tracking instance (graph: None — fresh imports).
        .or_else(|| {
            manifold_core::effects::binding_id_for_node_param_in(&default, node_doc_id, param_id)
        });
    if let Some(id) = bound {
        let pid = manifold_core::effects::ParamId::from(id);
        let old_val = project
            .with_preset_graph_mut(&target, |inst| {
                inst.params
                    .contains(pid.as_ref())
                    .then(|| inst.get_base_param(pid.as_ref()))
            })
            .flatten()?;
        if (old_val - value).abs() <= f32::EPSILON {
            return None;
        }
        project.with_preset_graph_mut(&target, |inst| {
            inst.set_base_param(pid.as_ref(), value);
        });
        return Some(Box::new(
            manifold_editing::commands::effects::ChangeGraphParamCommand::new(target, pid, old_val, value),
        ));
    }
    let mut cmd: Box<dyn manifold_editing::command::Command + Send> = Box::new(
        manifold_editing::commands::graph::SetGraphNodeParamCommand::new(
            target,
            node_doc_id,
            param_id.to_string(),
            manifold_core::effect_graph_def::SerializedParamValue::Float { value },
            default,
        )
        .with_scope(scope_path),
    );
    cmd.execute(project);
    Some(cmd)
}

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

/// P4b: translate the UI's SkinTargetMap into the editing command's enum.
fn map_skin_target_map(
    target: manifold_ui::panels::scene_setup_panel::SkinTargetMap,
) -> manifold_editing::commands::graph::SkinTargetMap {
    use manifold_ui::panels::scene_setup_panel::SkinTargetMap as Ui;
    match target {
        Ui::Emissive => manifold_editing::commands::graph::SkinTargetMap::Emissive,
        Ui::BaseColor => manifold_editing::commands::graph::SkinTargetMap::BaseColor,
    }
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
            ProjectAction::SceneSetupAddObject(layer_id.clone(), render_scene_id, before as u32);
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
            ProjectAction::SceneSetupAddLight(layer_id.clone(), render_scene_id, before as u32);
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

    /// scene-panel-ux gate: the properties-header "Frame" button drives the
    /// orbit camera to frame the selected object through the SAME
    /// dispatch_project arm a click reaches. Orbit cameras pivot at
    /// (0, look_y, 0), so framing = look_y at the object's height and
    /// distance covering radius + the object's horizontal offset.
    #[test]
    fn scene_setup_frame_selected_writes_orbit_camera_params() {
        use manifold_renderer::node_graph::scene_vm::{CameraVm, SceneObjectVm, SceneVm};
        let (mut project, layer_id, render_scene_id) = scene_layer_project();
        let def_before = effective_def(&project, &layer_id);
        let vm = SceneVm::from_def(&def_before).expect("SceneStarter resolves as a scene");
        let cam_id = match &vm.camera {
            CameraVm::Orbit(r) => r.node_doc_id,
            other => panic!("SceneStarter camera should be orbit, got {other:?}"),
        };
        let obj_pos = vm
            .objects
            .iter()
            .find_map(|o| match o {
                SceneObjectVm::Known(r) if r.index == 0 => r.transform.as_ref().map(|t| t.pos_value),
                _ => None,
            })
            .expect("SceneStarter object 0 has a transform");
        let (content_tx, content_state, mut ui, mut selection, mut active_layer, mut user_prefs) =
            dispatch_harness();

        let action = ProjectAction::SceneSetupFrameSelected(layer_id.clone(), render_scene_id, 0);
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
        assert!(result.structural_change, "framing the camera is a param write");

        let def_after = effective_def(&project, &layer_id);
        let cam = def_after
            .nodes
            .iter()
            .find(|n| n.id == cam_id)
            .expect("camera node still present");
        let get = |pid: &str| match cam.params.get(pid) {
            Some(SerializedParamValue::Float { value }) => *value,
            other => panic!("{pid} should be a float param, got {other:?}"),
        };
        let radius = vm
            .scene_bounds
            .map(|(mn, mx)| (0..3).map(|a| (mx[a] - mn[a]) * 0.5).fold(1.0f32, f32::max))
            .unwrap_or(1.0);
        let offset = (obj_pos.0 * obj_pos.0 + obj_pos.2 * obj_pos.2).sqrt();
        let expected_distance = 2.2 * (radius + offset);
        assert!(
            (get("distance") - expected_distance).abs() < 1e-4,
            "distance = 2.2 × (radius + offset): got {}, want {expected_distance}",
            get("distance")
        );
        assert!(
            (get("look_y") - obj_pos.1).abs() < 1e-4,
            "look_y lifts to the object's height: got {}, want {}",
            get("look_y"),
            obj_pos.1
        );
    }

    /// BUG-193 gate: "remove-object button emits RemoveSceneObjectCommand" —
    /// proven end to end through the SAME `dispatch_project` entry point the
    /// panel's per-row "✕" click reaches. Removes the LAST existing object
    /// (SceneStarter ships with at least one), then confirms `objects`
    /// dropped by one — the panel-visible count `state_sync` re-derives on
    /// its next structural sync (the "headless flow proving remove-object
    /// updates the panel" gate: `objects_param` reads the exact same
    /// `render_scene` param the Vm's `object_count` comes from).
    #[test]
    fn scene_setup_remove_object_dispatches_remove_scene_object_command() {
        let (mut project, layer_id, render_scene_id) = scene_layer_project();
        let before = objects_param(&project, &layer_id, render_scene_id);
        assert!(before >= 1.0, "SceneStarter ships with at least one object");
        let (content_tx, content_state, mut ui, mut selection, mut active_layer, mut user_prefs) =
            dispatch_harness();

        let action = ProjectAction::SceneSetupRemoveObject(
            layer_id.clone(),
            render_scene_id,
            (before - 1.0) as u32,
        );
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
        assert!(result.structural_change, "removing an object is a structural graph edit");
        assert_eq!(objects_param(&project, &layer_id, render_scene_id), before - 1.0);
    }

    /// BUG-193 gate: the light-row twin of the object-removal gate above.
    #[test]
    fn scene_setup_remove_light_dispatches_remove_scene_light_command() {
        let (mut project, layer_id, render_scene_id) = scene_layer_project();
        let before = lights_param(&project, &layer_id, render_scene_id);
        assert!(before >= 1.0, "SceneStarter ships with at least one light");
        let (content_tx, content_state, mut ui, mut selection, mut active_layer, mut user_prefs) =
            dispatch_harness();

        let action = ProjectAction::SceneSetupRemoveLight(
            layer_id.clone(),
            render_scene_id,
            (before - 1.0) as u32,
        );
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
        assert!(result.structural_change, "removing a light is a structural graph edit");
        assert_eq!(lights_param(&project, &layer_id, render_scene_id), before - 1.0);
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

    /// P5 gate: `InsertMeshModifierCommand` spliced into a REAL
    /// `SceneStarter`-based def lands the new node inside the object's own
    /// group body, in the shape `graph_tool validate --kind generator` +
    /// `graph_tool fusion` accept — proven by hand this session against this
    /// exact def (dumped via `serde_json::to_string_pretty` and run through
    /// both CLI commands; see the P5 landing report). `validate_def` itself
    /// needs a live `GpuDevice` (behind the `gpu-proofs` feature), so this
    /// `--lib` test asserts the structural shape the CLI run already proved
    /// valid, rather than re-deriving a GPU-backed validation call here.
    #[test]
    fn insert_modifier_on_scene_starter_lands_in_the_object_group_body() {
        let (mut project, layer_id, _render_scene_id) = scene_layer_project();
        let def = generator_catalog_default(&project, &layer_id).expect("SceneStarter resolves");
        let group_node_id = def
            .nodes
            .iter()
            .find(|n| n.group.is_some())
            .expect("SceneStarter has at least one named object group")
            .id;

        let target = manifold_core::GraphTarget::Generator(layer_id.clone());
        let mut cmd = manifold_editing::commands::graph::InsertMeshModifierCommand::new(
            target,
            Vec::new(),
            group_node_id,
            "node.twist_mesh".to_string(),
            None,
            manifold_renderer::node_graph::scene_exposure::metadata_for_node_type("node.twist_mesh"),
            def,
        );
        use manifold_editing::command::Command;
        cmd.execute(&mut project);

        let (_, layer) = project.timeline.find_layer_by_id(&layer_id).unwrap();
        let graph = layer.generator_graph().unwrap();
        let inserted_group = graph.nodes.iter().find(|n| n.id == group_node_id).unwrap();
        let body = inserted_group.group.as_deref().unwrap();
        let inserted = body
            .nodes
            .iter()
            .find(|n| n.type_id == "node.twist_mesh")
            .expect("the twist node lands inside the object's own group body");

        // P1 (SCENE_PANEL_EXPOSURE_CONVERGENCE_DESIGN.md): against a REAL
        // SceneStarter def and the real registry-backed metadata, the
        // inserted modifier's params land in the def's top-level
        // `preset_metadata`, targeting its bare NodeId — an app-level
        // round-trip proof, not just the hand-built editing-crate fixtures.
        let meta = graph.preset_metadata.as_ref().expect("P1 stamped exposures into preset_metadata");
        assert!(
            meta.bindings.iter().any(|b| matches!(
                &b.target,
                manifold_core::effect_graph_def::BindingTarget::Node { node_id, .. } if *node_id == inserted.node_id
            )),
            "the twist modifier's params are exposed, targeting its bare NodeId"
        );
    }
    /// BUG-229 diagnosis (SCENE_PANEL_CARD_CONVERGENCE_DESIGN.md C-P1, orchestrator
    /// addition): Peter reported "the params ... for cameras, world, lights ... do
    /// nothing." Before this test, `SceneSetupParamChanged` had zero dispatch-level
    /// coverage for these three families — only Add/Remove/rename were proven
    /// through `dispatch_project`; the value-write path itself was unverified past
    /// the panel's own click/drag unit tests (which only assert an action gets
    /// *built*, never that it changes anything). Value-level, not dispatch-log-level,
    /// per the escalation brief: reads the def's actual `params` map after dispatch.
    ///
    /// Result: for a FRESH `SceneStarter` layer (no per-instance override yet — the
    /// common real case, since a scene layer never diverges until you scrub
    /// something), the write DOES land — `SetGraphNodeParamCommand` + the traced
    /// `RowAddr` (root scope, `node_doc_id` off the same `SceneVm::from_def` state_sync
    /// walks) are correct for all three families. This rules out the
    /// addressing/dispatch layer as BUG-229's root cause. The live "does nothing"
    /// symptom therefore lives above this layer — most likely in the bespoke
    /// per-family click/drag routing (`build_light_numeric_row`/
    /// `build_camera_numeric_row`/the World rows' equivalents) that C-P1 deletes
    /// wholesale in favor of the card's proven `build_param_row` click path. Logged
    /// as BUG-229 in `docs/BUG_BACKLOG.md` with this finding; not fixed this session
    /// (see design doc status — C-P1's full row-swap was not completed).
    #[test]
    fn scene_setup_param_changed_writes_light_intensity_to_def() {
        let (mut project, layer_id, _render_scene_id) = scene_layer_project();
        let def = effective_def(&project, &layer_id);
        let vm = manifold_renderer::node_graph::scene_vm::SceneVm::from_def(&def).expect("scene vm");
        let light_node_id = vm
            .lights
            .iter()
            .find_map(|l| match l {
                manifold_renderer::node_graph::scene_vm::SceneLightVm::Known(r) => Some(r.node_doc_id),
                _ => None,
            })
            .expect("SceneStarter ships with at least one known light");

        let (content_tx, content_state, mut ui, mut selection, mut active_layer, mut user_prefs) =
            dispatch_harness();
        let action = ProjectAction::SceneSetupParamChanged(
            layer_id.clone(),
            Vec::new(),
            light_node_id,
            "intensity".to_string(),
            7.77,
        );
        let result = dispatch_project(
            &action, &mut project, &content_tx, &content_state, &mut ui, &mut selection, &mut active_layer,
            &mut user_prefs,
        );
        assert!(!result.structural_change, "a param scrub is not a structural graph edit");

        let after_def = effective_def(&project, &layer_id);
        let node = after_def.nodes.iter().find(|n| n.id == light_node_id).unwrap();
        match node.params.get("intensity") {
            Some(SerializedParamValue::Float { value }) => {
                assert_eq!(*value, 7.77, "light intensity should have changed in the def")
            }
            other => panic!("expected Float, got {other:?}"),
        }
    }

    /// BUG-229 diagnosis, camera twin of the light test above.
    #[test]
    fn scene_setup_param_changed_writes_camera_orbit_to_def() {
        let (mut project, layer_id, _render_scene_id) = scene_layer_project();
        let def = effective_def(&project, &layer_id);
        let vm = manifold_renderer::node_graph::scene_vm::SceneVm::from_def(&def).expect("scene vm");
        let camera_node_id = match vm.camera {
            manifold_renderer::node_graph::scene_vm::CameraVm::Orbit(c) => c.node_doc_id,
            other => panic!("SceneStarter's default camera should be Orbit, got {other:?}"),
        };

        let (content_tx, content_state, mut ui, mut selection, mut active_layer, mut user_prefs) =
            dispatch_harness();
        let action = ProjectAction::SceneSetupParamChanged(
            layer_id.clone(),
            Vec::new(),
            camera_node_id,
            "orbit".to_string(),
            2.5,
        );
        let result = dispatch_project(
            &action, &mut project, &content_tx, &content_state, &mut ui, &mut selection, &mut active_layer,
            &mut user_prefs,
        );
        assert!(!result.structural_change, "a param scrub is not a structural graph edit");

        let after_def = effective_def(&project, &layer_id);
        let node = after_def.nodes.iter().find(|n| n.id == camera_node_id).unwrap();
        match node.params.get("orbit") {
            Some(SerializedParamValue::Float { value }) => {
                assert_eq!(*value, 2.5, "camera orbit should have changed in the def")
            }
            other => panic!("expected Float, got {other:?}"),
        }
    }

    /// BUG-229 diagnosis, fog/atmosphere twin. SceneStarter ships with NO fog node
    /// by default (`AtmosphereVm::None`) — add one first through the SAME
    /// `SceneSetupAddFog` dispatch the panel's "+ Fog" button uses, exactly like a
    /// real session would, then scrub `fog_density` through it.
    #[test]
    fn scene_setup_param_changed_writes_fog_density_to_def() {
        let (mut project, layer_id, render_scene_id) = scene_layer_project();
        let (content_tx, content_state, mut ui, mut selection, mut active_layer, mut user_prefs) =
            dispatch_harness();

        let add_fog = ProjectAction::SceneSetupAddFog(layer_id.clone(), render_scene_id);
        dispatch_project(
            &add_fog, &mut project, &content_tx, &content_state, &mut ui, &mut selection, &mut active_layer,
            &mut user_prefs,
        );

        let def = effective_def(&project, &layer_id);
        let vm = manifold_renderer::node_graph::scene_vm::SceneVm::from_def(&def).expect("scene vm");
        let fog_node_id = match vm.atmosphere {
            manifold_renderer::node_graph::scene_vm::AtmosphereVm::Wired(a) => a.node_doc_id,
            manifold_renderer::node_graph::scene_vm::AtmosphereVm::None => {
                panic!("SceneSetupAddFog should have wired an atmosphere node")
            }
        };

        let action = ProjectAction::SceneSetupParamChanged(
            layer_id.clone(),
            Vec::new(),
            fog_node_id,
            "fog_density".to_string(),
            0.42,
        );
        let result = dispatch_project(
            &action, &mut project, &content_tx, &content_state, &mut ui, &mut selection, &mut active_layer,
            &mut user_prefs,
        );
        assert!(!result.structural_change, "a param scrub is not a structural graph edit");

        let after_def = effective_def(&project, &layer_id);
        let node = after_def.nodes.iter().find(|n| n.id == fog_node_id).unwrap();
        match node.params.get("fog_density") {
            Some(SerializedParamValue::Float { value }) => {
                assert_eq!(*value, 0.42, "fog density should have changed in the def")
            }
            other => panic!("expected Float, got {other:?}"),
        }
    }
}
