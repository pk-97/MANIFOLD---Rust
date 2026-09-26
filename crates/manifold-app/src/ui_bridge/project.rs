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
    ui: &mut UIRoot,
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
        ProjectAction::ToggleSplitSections => {
            let old_split = project.settings.split_at_markers;
            let cmd =
                manifold_editing::commands::settings::ToggleSplitSectionsCommand::new(old_split);
            {
                let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd);
                boxed.execute(project);
                ContentCommand::send(content_tx, ContentCommand::Execute(boxed));
            }
            log::info!(
                "Split export at markers → {}",
                project.settings.split_at_markers
            );
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
            if let Some(new) = ResolutionPreset::from_index(*preset_idx) {
                ContentCommand::send(content_tx, ContentCommand::SetResolution(new));
            }
            DispatchResult::handled()
        }
        ProjectAction::SetDisplayResolution(w, h) => {
            ContentCommand::send(content_tx, ContentCommand::SetDisplayResolution(*w, *h));
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
                    ContentCommand::send(
                        content_tx,
                        ContentCommand::ChangeGeneratorType {
                            layer_id: layer.layer_id.clone(),
                            new_type,
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
        ProjectAction::MaterialParamsSet {
            target,
            object,
            material,
            kind,
            writes,
            description,
        } => {
            dispatch_material_batch(
                project,
                content_tx,
                target,
                object,
                material,
                *kind,
                writes,
                description,
            );
            DispatchResult::handled()
        }
        ProjectAction::MaterialLookApply {
            target,
            object,
            material,
            look,
        } => {
            let Some(target_core) = material_graph_target(target) else {
                ContentCommand::send(
                    content_tx,
                    ContentCommand::GraphEditRejected(
                        "Select a scene material before applying a look".into(),
                    ),
                );
                return DispatchResult::handled();
            };
            let result = super::projection::material::graph_def(project, &target_core)
                .ok_or_else(|| "Material graph is unavailable".to_owned())
                .and_then(|def| {
                    let inst = project
                        .preset_instance(&target_core)
                        .ok_or("Material instance is unavailable")?;
                    let reference = manifold_core::scene_modifier_preset::SceneNodeRef {
                        scope: material.scope.clone(),
                        node: material.node.clone(),
                    };
                    super::material_looks::writes(inst, &def, &reference, *look)
                });
            match result {
                Ok(writes) => dispatch_material_batch(
                    project,
                    content_tx,
                    target,
                    object,
                    material,
                    manifold_ui::panels::actions::MaterialEditKind::Look,
                    &writes,
                    "Apply material look",
                ),
                Err(reason) => {
                    ContentCommand::send(content_tx, ContentCommand::GraphEditRejected(reason))
                }
            }
            DispatchResult::handled()
        }
        ProjectAction::SceneSetupParamChanged(
            layer_id,
            scope_path,
            node_doc_id,
            param_id,
            value,
        ) => {
            if let Some(reason) = crate::scene_modifier_edit::node_parameter_lock_reason(
                project,
                &manifold_core::GraphTarget::Generator(layer_id.clone()),
                scope_path,
                *node_doc_id,
                param_id,
            ) {
                ContentCommand::send(content_tx, ContentCommand::GraphEditRejected(reason.into()));
                return DispatchResult::handled();
            }
            if let Some(cmd) = apply_scene_param_write(
                project,
                layer_id,
                scope_path.clone(),
                *node_doc_id,
                param_id,
                *value,
            ) {
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
                    manifold_renderer::node_graph::scene_exposure::metadata_for_node_type(
                        "node.bake_environment",
                    ),
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
                    manifold_renderer::node_graph::scene_exposure::metadata_for_node_type(
                        "node.atmosphere",
                    ),
                    default,
                );
                let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd);
                boxed.execute(project);
                ContentCommand::send(content_tx, ContentCommand::Execute(boxed));
            }
            DispatchResult::structural()
        }

        ProjectAction::SceneModifierApply(layer, preset) => {
            ContentCommand::send(
                content_tx,
                ContentCommand::SceneModifier(
                    crate::scene_modifier_edit::SceneModifierAction::Add(
                        layer.clone(),
                        preset.clone(),
                    ),
                ),
            );
            DispatchResult::handled()
        }
        ProjectAction::SceneModifierRemove(layer, id) => {
            ContentCommand::send(
                content_tx,
                ContentCommand::SceneModifier(
                    crate::scene_modifier_edit::SceneModifierAction::Remove(
                        layer.clone(),
                        id.clone(),
                    ),
                ),
            );
            DispatchResult::handled()
        }
        ProjectAction::SceneModifierMove(layer, id, index) => {
            ContentCommand::send(
                content_tx,
                ContentCommand::SceneModifier(
                    crate::scene_modifier_edit::SceneModifierAction::Move(
                        layer.clone(),
                        id.clone(),
                        *index,
                    ),
                ),
            );
            DispatchResult::handled()
        }
        ProjectAction::SceneModifiersReorder(layer, order) => {
            ContentCommand::send(
                content_tx,
                ContentCommand::SceneModifier(
                    crate::scene_modifier_edit::SceneModifierAction::Reorder(
                        layer.clone(),
                        order.clone(),
                    ),
                ),
            );
            DispatchResult::handled()
        }
        ProjectAction::SceneModifiersDuplicate(layer, selected) => {
            ContentCommand::send(
                content_tx,
                ContentCommand::SceneModifier(
                    crate::scene_modifier_edit::SceneModifierAction::Duplicate(
                        layer.clone(),
                        selected.clone(),
                    ),
                ),
            );
            DispatchResult::handled()
        }
        ProjectAction::SceneModifiersCopy(layer, selected) => {
            match crate::scene_modifier_transfer::ModifierClipboard::capture(
                project, layer, selected,
            ) {
                Ok(clipboard) => ui.set_scene_modifier_clipboard(Some(clipboard)),
                Err(reason) => {
                    ContentCommand::send(content_tx, ContentCommand::GraphEditRejected(reason))
                }
            }
            DispatchResult::handled()
        }
        ProjectAction::SceneModifiersPaste(layer) => {
            if let Some(clipboard) = ui.scene_modifier_clipboard.clone() {
                ContentCommand::send(
                    content_tx,
                    ContentCommand::SceneModifier(
                        crate::scene_modifier_edit::SceneModifierAction::Paste(
                            layer.clone(),
                            clipboard,
                        ),
                    ),
                );
            }
            DispatchResult::handled()
        }
        ProjectAction::SceneModifiersRemove(layer, selected) => {
            ContentCommand::send(
                content_tx,
                ContentCommand::SceneModifier(
                    crate::scene_modifier_edit::SceneModifierAction::RemoveMany(
                        layer.clone(),
                        selected.clone(),
                    ),
                ),
            );
            DispatchResult::handled()
        }
        ProjectAction::SceneModifierSetTargets(layer, id, objects) => {
            let targets = objects.as_ref().map_or(
                manifold_core::scene_modifier_preset::SceneTargetSelection::AllObjects,
                |objects| manifold_core::scene_modifier_preset::SceneTargetSelection::Explicit {
                    objects: objects
                        .iter()
                        .map(
                            |object| manifold_core::scene_modifier_preset::SceneNodeRef {
                                scope: object.scope.clone(),
                                node: object.node.clone(),
                            },
                        )
                        .collect(),
                },
            );
            ContentCommand::send(
                content_tx,
                ContentCommand::SceneModifier(
                    crate::scene_modifier_edit::SceneModifierAction::Retarget(
                        layer.clone(),
                        id.clone(),
                        targets,
                    ),
                ),
            );
            DispatchResult::handled()
        }
        ProjectAction::SceneModifierToggleEnabled(layer, id) => {
            ContentCommand::send(
                content_tx,
                ContentCommand::SceneModifier(
                    crate::scene_modifier_edit::SceneModifierAction::Toggle(
                        layer.clone(),
                        id.clone(),
                    ),
                ),
            );
            DispatchResult::handled()
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
                    manifold_renderer::node_graph::scene_exposure::metadata_for_node_type(
                        "node.phong_material",
                    ),
                    manifold_renderer::node_graph::scene_exposure::metadata_for_node_type(
                        "node.transform_3d",
                    ),
                    manifold_renderer::node_graph::scene_exposure::metadata_for_node_type(
                        "node.scene_object",
                    ),
                    default,
                )
                .with_physics_world(
                    manifold_renderer::node_graph::scene_exposure::metadata_for_node_type(
                        "node.rigid_body",
                    ),
                    manifold_renderer::node_graph::scene_exposure::metadata_for_node_type(
                        "node.pbr_material",
                    ),
                );
                let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd);
                boxed.execute(project);
                ContentCommand::send(content_tx, ContentCommand::Execute(boxed));
            }
            DispatchResult::structural()
        }
        // BUG-hlw8 "+ Plane" button: mirrors `SceneSetupAddObject` above but
        // dispatches `AddSceneLayerPlaneCommand`, which builds a grouped plane
        // mesh + unlit material + transform. Skin assignment adds the source
        // on demand so an unassigned plane is visible. Width/height come from
        // the project's output resolution (height fixed at 1.0, width = aspect) so the
        // skinned layer composite is undistorted on the sheet.
        ProjectAction::SceneSetupAddLayerPlane(layer_id, render_scene_node_id, next_index) => {
            if let Some(default) = generator_catalog_default(project, layer_id) {
                let target = manifold_core::GraphTarget::Generator(layer_id.clone());
                let centroid = (900.0, 200.0 + 40.0 * *next_index as f32);
                let output_height = project.settings.output_height.max(1) as f32;
                let aspect = project.settings.output_width as f32 / output_height;
                let cmd = manifold_editing::commands::graph::AddSceneLayerPlaneCommand::new(
                    target,
                    Vec::new(),
                    *render_scene_node_id,
                    *next_index,
                    centroid,
                    aspect,
                    1.0,
                    manifold_renderer::node_graph::scene_exposure::metadata_for_node_type(
                        "node.unlit_material",
                    ),
                    manifold_renderer::node_graph::scene_exposure::metadata_for_node_type(
                        "node.transform_3d",
                    ),
                    manifold_renderer::node_graph::scene_exposure::metadata_for_node_type(
                        "node.scene_object",
                    ),
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
                    manifold_renderer::node_graph::scene_exposure::metadata_for_node_type(
                        "node.light",
                    ),
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
                if boxed.was_applied() {
                    ContentCommand::send(content_tx, ContentCommand::Execute(boxed));
                } else if let Some(reason) = boxed.rejection_reason() {
                    ContentCommand::send(
                        content_tx,
                        ContentCommand::GraphEditRejected(reason.to_owned()),
                    );
                }
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
                let cmd =
                    manifold_editing::commands::graph::SetSceneObjectSkinTargetMapCommand::new(
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
            if let Some(mut default) = generator_catalog_default(project, layer_id) {
                // A bundled scene can still have no graph override and no
                // stamped scene exposures. Seed the command's undoable graph
                // baseline before it clones source bindings, so first-use
                // duplicates have the same live controls as migrated scenes.
                manifold_renderer::node_graph::scene_exposure::migrate_scene_exposures(&mut default);
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
                if boxed.was_applied() {
                    ContentCommand::send(content_tx, ContentCommand::Execute(boxed));
                } else if let Some(reason) = boxed.rejection_reason() {
                    // The panel's local mirror runs the command before it is
                    // handed to the content thread. Surface an ownership or
                    // malformed-graph rejection immediately instead of
                    // silently dropping the click.
                    ContentCommand::send(
                        content_tx,
                        ContentCommand::GraphEditRejected(reason.to_owned()),
                    );
                }
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
                                manifold_core::effect_graph_def::SerializedParamValue::Float {
                                    value,
                                } => Some(*value),
                                _ => None,
                            })
                            .unwrap_or(dflt)
                    };
                    let cur_pos = (pf("pos_x", 0.0), pf("pos_y", 0.0), pf("pos_z", 0.0));
                    let cur_tgt = (
                        pf("target_x", 0.0),
                        pf("target_y", 0.0),
                        pf("target_z", 0.0),
                    );
                    let mut dir = (
                        cur_tgt.0 - cur_pos.0,
                        cur_tgt.1 - cur_pos.1,
                        cur_tgt.2 - cur_pos.2,
                    );
                    let len = (dir.0 * dir.0 + dir.1 * dir.1 + dir.2 * dir.2).sqrt();
                    // Degenerate view (camera sitting on its target): fall
                    // back to looking down -Z rather than producing NaNs.
                    dir = if len > 1e-6 {
                        (dir.0 / len, dir.1 / len, dir.2 / len)
                    } else {
                        (0.0, 0.0, -1.0)
                    };
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
                CameraVm::Free(_)
                | CameraVm::Custom { .. }
                | CameraVm::None
                | CameraVm::Loop(_) => {
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
            let batch: Vec<Box<dyn manifold_editing::command::Command>> = writes
                .into_iter()
                .map(|c| c as Box<dyn manifold_editing::command::Command>)
                .collect();
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

            let Some(path) = rfd::FileDialog::new()
                .add_filter("glTF", &["glb", "gltf"])
                .pick_file()
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
                log::info!(
                    "[Scene Setup] Import Model… report: {}",
                    plan.report_lines.join("; ")
                );
            }
            DispatchResult::structural()
        }
        // ── SCENE_SETUP_PANEL_DESIGN P5: the modifier stack (D6) ──
        // All three resolve `GraphTarget::Generator(layer_id)` exactly like
        // the P1/P2 arms above — no new mutation path, just the three named
        // composites `InsertMeshModifierCommand`/`RemoveMeshModifierCommand`/
        // `MoveMeshModifierCommand`.
        ProjectAction::SceneSetupAddModifier(layer_id, group_node_id, type_id) => {
            ContentCommand::send(content_tx, ContentCommand::ObjectModifier(
                crate::object_modifier_transfer::ObjectModifierAction::Add {
                    layer_id: layer_id.clone(), owner_id: *group_node_id,
                    type_id: type_id.clone(), after: None,
                },
            ));
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
                ContentCommand::send(content_tx, ContentCommand::ExecuteOnContent(Box::new(cmd)));
            }
            DispatchResult::structural()
        }
        ProjectAction::SceneSetupMoveModifier(
            layer_id,
            group_node_id,
            modifier_node_id,
            new_position,
        ) => {
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
                ContentCommand::send(content_tx, ContentCommand::ExecuteOnContent(Box::new(cmd)));
            }
            DispatchResult::structural()
        }
        // D7 "New 3D Scene" empty-state action: assign the bundled Scene
        // Starter preset through the existing empty-state command. The browser
        // picker uses content-owned replacement to preserve applied modifiers.
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
                    let mut boxed: Box<dyn manifold_editing::command::Command + Send> =
                        Box::new(cmd);
                    boxed.execute(project);
                    ContentCommand::send(content_tx, ContentCommand::Execute(boxed));
                    ContentCommand::send(
                        content_tx,
                        ContentCommand::GeneratorTypeChanged {
                            layer_id: layer_id.clone(),
                            new_type,
                        },
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
            ContentCommand::send(content_tx, ContentCommand::FinishAutomationRecording);
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
            remove_parameter_automation(project, content_tx, ui, _selection, target, param_id);
            DispatchResult::structural()
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
            if manifold_renderer::node_graph::scene_vm::is_param_exposed(
                &effective_def,
                *node_doc_id,
                param_id,
            ) {
                return DispatchResult::handled();
            }
            let Some(node) = find_node_by_scope(&effective_def, scope_path, *node_doc_id) else {
                return DispatchResult::handled();
            };
            let node_id = node.node_id.clone();
            let node_handle = node
                .handle
                .clone()
                .unwrap_or_else(|| format!("node{node_doc_id}"));
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
        .with_preset_graph_mut(&target, |inst| {
            inst.binding_id_for_node_param(node_doc_id, param_id)
        })
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
            manifold_editing::commands::effects::ChangeGraphParamCommand::new(
                target, pid, old_val, value,
            ),
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

/// Shared parameter/lane menu path: remove the envelope and its stale UI targets.
pub(super) fn remove_parameter_automation(
    project: &mut Project,
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
    let mut command = manifold_editing::commands::automation::RemoveLaneCommand::for_param(
        graph_target,
        param_id.as_ref(),
    );
    command.execute(project);
    crate::content_command::ContentCommand::send(
        content_tx,
        crate::content_command::ContentCommand::Execute(Box::new(command)),
    );
    selection
        .chosen_automation_params
        .retain(|_, (t, p)| t != target || p != param_id);
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

fn material_graph_target(
    target: &manifold_ui::GraphParamTarget,
) -> Option<manifold_core::GraphTarget> {
    match target {
        manifold_ui::GraphParamTarget::GeneratorOf(layer) => {
            Some(manifold_core::GraphTarget::Generator(layer.clone()))
        }
        _ => None,
    }
}

#[allow(clippy::too_many_arguments)]
fn dispatch_material_batch(
    project: &Project,
    content_tx: &crossbeam_channel::Sender<crate::content_command::ContentCommand>,
    target: &manifold_ui::GraphParamTarget,
    object: &manifold_ui::param_surface::ModifierObjectRef,
    material: &manifold_ui::param_surface::ModifierObjectRef,
    kind: manifold_ui::panels::actions::MaterialEditKind,
    writes: &[manifold_ui::panels::actions::MaterialParamWrite],
    description: &str,
) {
    use crate::content_command::ContentCommand;
    use manifold_editing::commands::material::{
        ChangeMaterialParamsCommand, MaterialEditContext, MaterialEditKind, MaterialParamChange,
    };
    let Some(target) = material_graph_target(target) else {
        ContentCommand::send(
            content_tx,
            ContentCommand::GraphEditRejected("Material edit requires its scene layer".into()),
        );
        return;
    };
    let Some(inst) = project.preset_instance(&target) else {
        return;
    };
    let kind = match kind {
        manifold_ui::panels::actions::MaterialEditKind::Feature => MaterialEditKind::Feature,
        manifold_ui::panels::actions::MaterialEditKind::Look => MaterialEditKind::Look,
        manifold_ui::panels::actions::MaterialEditKind::Placement => MaterialEditKind::Placement,
    };
    let context = MaterialEditContext {
        expected_preset_id: inst.effect_type().clone(),
        object: manifold_core::scene_modifier_preset::SceneNodeRef {
            scope: object.scope.clone(),
            node: object.node.clone(),
        },
        material: manifold_core::scene_modifier_preset::SceneNodeRef {
            scope: material.scope.clone(),
            node: material.node.clone(),
        },
        kind,
    };
    let mut changes = Vec::with_capacity(writes.len());
    for write in writes {
        let Some(_) = inst.params.get(&write.param_id) else {
            ContentCommand::send(
                content_tx,
                ContentCommand::GraphEditRejected(format!(
                    "Material parameter {} is unavailable",
                    write.param_id
                )),
            );
            return;
        };
        changes.push(MaterialParamChange {
            param_id: write.param_id.clone(),
            expected: inst.get_base_param(&write.param_id),
            value: write.value,
        });
    }
    let catalog_default = super::projection::material::graph_def(project, &target);
    match manifold_editing::commands::material::validate_material_edit(
        project,
        &target,
        &context,
        &changes,
        catalog_default.as_ref(),
    ) {
        Ok(()) => ContentCommand::send(
            content_tx,
            ContentCommand::ExecuteOnContent(Box::new(ChangeMaterialParamsCommand::new(
                target,
                context,
                changes,
                description.to_owned(),
                catalog_default,
            ))),
        ),
        Err(reason) => ContentCommand::send(content_tx, ContentCommand::GraphEditRejected(reason)),
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

    fn physics_solids_layer_project() -> (Project, LayerId, u32) {
        let mut project = Project::default();
        let idx = project.timeline.add_layer(
            "Physics Solids",
            LayerType::Generator,
            PresetTypeId::from_string("PhysicsSolids".to_string()),
        );
        let layer_id = project.timeline.layers[idx].layer_id.clone();
        let def = manifold_renderer::node_graph::bundled_preset_def(
            &project.timeline.layers[idx].generator_type().clone(),
        )
        .expect("PhysicsSolids is a bundled preset");
        let render_scene_id = def
            .nodes
            .iter()
            .find(|n| n.type_id == manifold_renderer::node_graph::scene_vm::RENDER_SCENE_TYPE_ID)
            .expect("PhysicsSolids has a render_scene node")
            .id;
        (project, layer_id, render_scene_id)
    }

    /// The layer's CURRENT effective def — the per-instance override once
    /// one exists (post-edit), falling back to the bundled catalog default
    /// beforehand (pre-edit: a fresh `SceneStarter` layer has no override
    /// yet, exactly why `AddSceneObjectCommand` needs a `catalog_default` to
    /// lift one — same resolution `state_sync.rs`'s panel-Vm builder uses).
    fn effective_def(
        project: &Project,
        layer_id: &LayerId,
    ) -> manifold_core::effect_graph_def::EffectGraphDef {
        let (_, layer) = project.timeline.find_layer_by_id(layer_id).unwrap();
        layer.generator_graph().cloned().unwrap_or_else(|| {
            manifold_renderer::node_graph::bundled_preset_def(&layer.generator_type().clone())
                .cloned()
                .expect("SceneStarter is a bundled preset")
        })
    }

    fn objects_param(project: &Project, layer_id: &LayerId, render_scene_id: u32) -> f32 {
        let graph = effective_def(project, layer_id);
        let scene = graph
            .nodes
            .iter()
            .find(|n| n.id == render_scene_id)
            .unwrap();
        match scene.params.get("objects") {
            Some(SerializedParamValue::Float { value }) => *value,
            _ => 0.0,
        }
    }

    fn lights_param(project: &Project, layer_id: &LayerId, render_scene_id: u32) -> f32 {
        let graph = effective_def(project, layer_id);
        let scene = graph
            .nodes
            .iter()
            .find(|n| n.id == render_scene_id)
            .unwrap();
        match scene.params.get("lights") {
            Some(SerializedParamValue::Float { value }) => *value,
            _ => 0.0,
        }
    }

    fn find_node_recursive(
        nodes: &[manifold_core::effect_graph_def::EffectGraphNode],
        id: u32,
    ) -> Option<&manifold_core::effect_graph_def::EffectGraphNode> {
        nodes.iter().find_map(|node| {
            (node.id == id).then_some(node).or_else(|| {
                node.group
                    .as_deref()
                    .and_then(|group| find_node_recursive(&group.nodes, id))
            })
        })
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
        assert!(
            result.structural_change,
            "adding an object is a structural graph edit"
        );
        assert_eq!(
            objects_param(&project, &layer_id, render_scene_id),
            before + 1.0
        );
    }

    #[test]
    fn sdr_controls_route_pointer_gestures_to_content_and_undo() {
        use crate::content_command::ContentCommand;
        use manifold_ui::panels::overlay::{Overlay, OverlayPlacement, OverlayResponse};
        use manifold_ui::{PanelAction, PointerAction, Rect, UIInputSystem, UITree, Vec2};

        let mut project = Project::default();
        let before = serde_json::to_value(&project).expect("project serializes");
        let mut content = crate::headless_harness::headless_content_thread(project.clone(), 64, 64);
        let (content_tx, content_rx) = crossbeam_channel::unbounded();
        let content_state = crate::content_state::ContentState::default();
        let mut ui = UIRoot::new();
        let mut selection = manifold_ui::UIState::new();
        let mut active_layer = None;
        let mut user_prefs = UserPrefs::in_memory();
        let mut tree = UITree::new();
        let mut input = UIInputSystem::new();
        ui.settings_popup.open();

        for label in ["SDR", "AgX", "Off"] {
            tree.clear();
            let size = ui.settings_popup.desired_size();
            let region = tree.begin_region(
                Rect::new(0.0, 0.0, 1280.0, 720.0),
                manifold_ui::tree::ZTier::Overlay,
                "settings",
                manifold_ui::UIFlags::empty(),
            );
            let content_start = tree.count();
            ui.settings_popup.build_at(&mut tree, OverlayPlacement {
                rect: Rect::new(0.0, 0.0, size.x, size.y),
                screen: Vec2::new(1280.0, 720.0),
            });
            tree.end_region(region, content_start);
            let bounds = tree.nodes().iter()
                .find(|node| node.text.as_deref() == Some(label))
                .expect("settings control exists").bounds;
            let point = Vec2::new(bounds.x + bounds.width * 0.5, bounds.y + bounds.height * 0.5);
            input.process_pointer(&mut tree, point, PointerAction::Down, 0.0);
            input.process_pointer(&mut tree, point, PointerAction::Up, 0.1);
            for event in input.drain_events() {
                if let OverlayResponse::Consumed(actions) = ui.settings_popup.on_event(&event, &mut tree) {
                    for action in actions {
                        let PanelAction::Project(action) = action else {
                            panic!("settings control emitted unexpected action");
                        };
                        dispatch_project(
                            &action, &mut project, &content_tx, &content_state,
                            &mut ui, &mut selection, &mut active_layer, &mut user_prefs,
                        );
                    }
                }
            }
            let command = content_rx.try_recv().expect("pointer gesture forwarded a content command");
            assert!(content_rx.is_empty(), "one command per gesture");
            assert!(!content.handle_command(command));
            let settings = &content.engine.project().unwrap().settings;
            match label {
                "SDR" => {
                    assert!(content.content_pipeline.sdr_preview());
                    assert_eq!(content.editing_service.data_version(), 0);
                    assert!(!content.editing_service.is_dirty());
                }
                "AgX" => {
                    assert!(settings.tonemap_enabled);
                    assert_eq!(settings.tonemap_curve, manifold_core::TonemapCurve::Agx);
                }
                "Off" => {
                    assert!(!settings.tonemap_enabled);
                    assert_eq!(settings.tonemap_curve, manifold_core::TonemapCurve::Agx);
                }
                _ => unreachable!(),
            }
        }

        assert_eq!(
            serde_json::to_value(&project).expect("project serializes"),
            before,
            "UI gestures must not mutate the UI's project copy"
        );
        assert!(!content.handle_command(ContentCommand::Undo));
        let settings = &content.engine.project().unwrap().settings;
        assert!(settings.tonemap_enabled);
        assert_eq!(settings.tonemap_curve, manifold_core::TonemapCurve::Agx);
        assert!(content.content_pipeline.sdr_preview(), "undo leaves the viewing preference alone");
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
        assert!(
            result.structural_change,
            "adding a light is a structural graph edit"
        );
        assert_eq!(
            lights_param(&project, &layer_id, render_scene_id),
            before + 1.0
        );
    }

    /// BUG-hlw8: the "+ Plane" button dispatches `AddSceneLayerPlaneCommand`
    /// through the same `dispatch_project` entry point, bumping the scene's
    /// `objects` count by one and stamping the new grouped plane into
    /// the graph.
    #[test]
    fn scene_setup_add_layer_plane_dispatches_add_scene_layer_plane_command() {
        let (mut project, layer_id, render_scene_id) = scene_layer_project();
        let before = objects_param(&project, &layer_id, render_scene_id);
        let (content_tx, content_state, mut ui, mut selection, mut active_layer, mut user_prefs) =
            dispatch_harness();

        let action = ProjectAction::SceneSetupAddLayerPlane(
            layer_id.clone(),
            render_scene_id,
            before as u32,
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
        assert!(
            result.structural_change,
            "adding a layer plane is a structural graph edit"
        );
        assert_eq!(
            objects_param(&project, &layer_id, render_scene_id),
            before + 1.0
        );

        let def = effective_def(&project, &layer_id);
        let added_group = def
            .nodes
            .iter()
            .find(|n| {
                n.type_id == manifold_core::effect_graph_def::GROUP_TYPE_ID
                    && n.group.as_ref().is_some_and(|g| {
                        g.nodes.iter().any(|n| n.type_id == "node.plane_mesh")
                    })
            })
            .expect("the new layer plane group is present");
        let body = added_group.group.as_ref().expect("is a group");
        assert!(body.nodes.iter().any(|n| n.type_id == "node.plane_mesh"));
        assert!(!body.nodes.iter().any(|n| n.type_id == "node.layer_source"));
        let vm = manifold_renderer::node_graph::scene_vm::SceneVm::from_def(&def).unwrap();
        let manifold_renderer::node_graph::scene_vm::SceneObjectVm::Known(plane) =
            vm.objects.last().unwrap()
        else {
            panic!("added plane must be editable");
        };
        assert!(plane.skin.is_none(), "unassigned plane must not sample transparent skin");
        assert_eq!(
            super::super::projection::material::default_skin_target(Some(&def), &plane.material),
            manifold_ui::panels::scene_setup_panel::SkinTargetMap::BaseColor,
        );
        let scene_object = body
            .nodes
            .iter()
            .find(|n| n.type_id == "node.scene_object")
            .expect("scene_object inside group");
        assert!(
            body.wires
                .iter()
                .any(|w| w.from_node == scene_object.id && w.from_port == "object")
        );
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
                SceneObjectVm::Known(r) if r.index == 0 => {
                    r.transform.as_ref().map(|t| t.pos_value)
                }
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
        assert!(
            result.structural_change,
            "framing the camera is a param write"
        );

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
        assert!(
            result.structural_change,
            "removing an object is a structural graph edit"
        );
        assert_eq!(
            objects_param(&project, &layer_id, render_scene_id),
            before - 1.0
        );
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
        assert!(
            result.structural_change,
            "removing a light is a structural graph edit"
        );
        assert_eq!(
            lights_param(&project, &layer_id, render_scene_id),
            before - 1.0
        );
    }

    /// A production SceneStarter flow: add an authored object, duplicate it,
    /// then write the duplicate's nested transform through the same panel
    /// action used by the live UI. The duplicate's scene binding must be live
    /// immediately; otherwise `apply_scene_param_write` cannot resolve the
    /// nested row and the write is silently dropped.
    #[test]
    fn scene_setup_duplicate_object_keeps_nested_transform_editable() {
        let (mut project, layer_id, render_scene_id) = scene_layer_project();
        let before = objects_param(&project, &layer_id, render_scene_id) as u32;
        let (content_tx, content_state, mut ui, mut selection, mut active_layer, mut user_prefs) =
            dispatch_harness();

        let add = ProjectAction::SceneSetupAddObject(layer_id.clone(), render_scene_id, before);
        dispatch_project(
            &add,
            &mut project,
            &content_tx,
            &content_state,
            &mut ui,
            &mut selection,
            &mut active_layer,
            &mut user_prefs,
        );
        let source_index = before;
        let duplicate = ProjectAction::SceneSetupDuplicateObject(
            layer_id.clone(),
            render_scene_id,
            source_index,
        );
        dispatch_project(
            &duplicate,
            &mut project,
            &content_tx,
            &content_state,
            &mut ui,
            &mut selection,
            &mut active_layer,
            &mut user_prefs,
        );

        let def = effective_def(&project, &layer_id);
        let vm = manifold_renderer::node_graph::scene_vm::SceneVm::from_def(&def)
            .expect("SceneStarter scene VM after duplicate");
        let transform_id = vm
            .objects
            .iter()
            .find_map(|object| match object {
                manifold_renderer::node_graph::scene_vm::SceneObjectVm::Known(row)
                    if row.index == (source_index + 1) as usize =>
                {
                    row.transform
                        .as_ref()
                        .map(|transform| transform.node_doc_id)
                }
                _ => None,
            })
            .expect("duplicated object has a transform row");

        let write = ProjectAction::SceneSetupParamChanged(
            layer_id.clone(),
            Vec::new(),
            transform_id,
            "pos_x".to_string(),
            3.25,
        );
        dispatch_project(
            &write,
            &mut project,
            &content_tx,
            &content_state,
            &mut ui,
            &mut selection,
            &mut active_layer,
            &mut user_prefs,
        );

        let updated_def = effective_def(&project, &layer_id);
        let updated = find_node_recursive(&updated_def.nodes, transform_id)
            .expect("duplicated transform remains in the authored graph");
        assert_eq!(
            updated.params.get("pos_x"),
            Some(&SerializedParamValue::Float { value: 0.5 }),
            "a bound scene row keeps the authored duplicate default in the graph"
        );
        let binding_id = manifold_core::effects::binding_id_for_node_param_in(
            &updated_def,
            transform_id,
            "pos_x",
        )
        .expect("duplicated transform has its own scene binding");
        let (_, layer) = project.timeline.find_layer_by_id(&layer_id).unwrap();
        let live_value = layer
            .gen_params()
            .and_then(|instance| instance.params.get(&binding_id))
            .map(|param| param.value);
        assert_eq!(
            live_value,
            Some(3.25),
            "the duplicate's nested transform is independently editable"
        );
    }

    /// Production PhysicsSolids flow: the physics duplicate clones its
    /// auto-exposed transform binding, and the panel write lands in the live
    /// generator manifest under that fresh binding id.
    #[test]
    fn scene_setup_physics_duplicate_param_is_live_and_independent() {
        let (mut project, layer_id, render_scene_id) = physics_solids_layer_project();
        let source_index = 0;
        let duplicate_index = objects_param(&project, &layer_id, render_scene_id) as usize;
        let (content_tx, content_state, mut ui, mut selection, mut active_layer, mut user_prefs) =
            dispatch_harness();

        let duplicate = ProjectAction::SceneSetupDuplicateObject(
            layer_id.clone(),
            render_scene_id,
            source_index,
        );
        dispatch_project(
            &duplicate,
            &mut project,
            &content_tx,
            &content_state,
            &mut ui,
            &mut selection,
            &mut active_layer,
            &mut user_prefs,
        );

        let def = effective_def(&project, &layer_id);
        let vm = manifold_renderer::node_graph::scene_vm::SceneVm::from_def(&def)
            .expect("PhysicsSolids scene VM after duplicate");
        let transform_id = vm
            .objects
            .iter()
            .find_map(|object| match object {
                manifold_renderer::node_graph::scene_vm::SceneObjectVm::Known(row)
                    if row.index == duplicate_index =>
                {
                    row.transform
                        .as_ref()
                        .map(|transform| transform.node_doc_id)
                }
                _ => None,
            })
            .expect("physics duplicate has a transform row");
        let sections = crate::ui_bridge::projection::scene::sections_for_doc_ids(
            Some(&def),
            &[transform_id],
        );
        assert!(
            sections.iter().any(|section| section.contains("Transform")),
            "duplicate transform sections: {sections:?}"
        );
        let source_transform_id = vm.objects.iter().find_map(|object| match object {
            manifold_renderer::node_graph::scene_vm::SceneObjectVm::Known(row)
                if row.index == source_index as usize =>
            {
                row.transform.as_ref().map(|transform| transform.node_doc_id)
            }
            _ => None,
        }).expect("source has a transform row");
        let source_sections = crate::ui_bridge::projection::scene::sections_for_doc_ids(
            Some(&def), &[source_transform_id],
        );
        assert!(sections.iter().all(|section| !source_sections.contains(section)),
            "duplicate properties must not include source sections");
        let write = ProjectAction::SceneSetupParamChanged(
            layer_id.clone(),
            Vec::new(),
            transform_id,
            "pos_y".to_string(),
            8.5,
        );
        dispatch_project(
            &write,
            &mut project,
            &content_tx,
            &content_state,
            &mut ui,
            &mut selection,
            &mut active_layer,
            &mut user_prefs,
        );

        let binding_id = manifold_core::effects::binding_id_for_node_param_in(
            &effective_def(&project, &layer_id),
            transform_id,
            "pos_y",
        )
        .expect("physics duplicate has a fresh pos_y binding");
        let (_, layer) = project.timeline.find_layer_by_id(&layer_id).unwrap();
        let section = layer
            .gen_params()
            .and_then(|instance| instance.params.get(&binding_id))
            .and_then(|param| param.spec.section.clone());
        assert!(
            section
                .as_deref()
                .is_some_and(|section| section.contains("Transform")),
            "live duplicate section: {section:?}"
        );
        let live_value = layer
            .gen_params()
            .and_then(|instance| instance.params.get(&binding_id))
            .map(|param| param.value);
        assert_eq!(live_value, Some(8.5));
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
        let def =
            generator_catalog_default(&project, &layer_id).expect("resolves for a live layer");
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
            manifold_renderer::node_graph::scene_exposure::metadata_for_node_type(
                "node.twist_mesh",
            ),
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
        let meta = graph
            .preset_metadata
            .as_ref()
            .expect("P1 stamped exposures into preset_metadata");
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
        let vm =
            manifold_renderer::node_graph::scene_vm::SceneVm::from_def(&def).expect("scene vm");
        let light_node_id = vm
            .lights
            .iter()
            .find_map(|l| match l {
                manifold_renderer::node_graph::scene_vm::SceneLightVm::Known(r) => {
                    Some(r.node_doc_id)
                }
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
            &action,
            &mut project,
            &content_tx,
            &content_state,
            &mut ui,
            &mut selection,
            &mut active_layer,
            &mut user_prefs,
        );
        assert!(
            !result.structural_change,
            "a param scrub is not a structural graph edit"
        );

        let after_def = effective_def(&project, &layer_id);
        let node = after_def
            .nodes
            .iter()
            .find(|n| n.id == light_node_id)
            .unwrap();
        match node.params.get("intensity") {
            Some(SerializedParamValue::Float { value }) => {
                assert_eq!(
                    *value, 7.77,
                    "light intensity should have changed in the def"
                )
            }
            other => panic!("expected Float, got {other:?}"),
        }
    }

    /// BUG-229 diagnosis, camera twin of the light test above.
    #[test]
    fn scene_setup_param_changed_writes_camera_orbit_to_def() {
        let (mut project, layer_id, _render_scene_id) = scene_layer_project();
        let def = effective_def(&project, &layer_id);
        let vm =
            manifold_renderer::node_graph::scene_vm::SceneVm::from_def(&def).expect("scene vm");
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
            &action,
            &mut project,
            &content_tx,
            &content_state,
            &mut ui,
            &mut selection,
            &mut active_layer,
            &mut user_prefs,
        );
        assert!(
            !result.structural_change,
            "a param scrub is not a structural graph edit"
        );

        let after_def = effective_def(&project, &layer_id);
        let node = after_def
            .nodes
            .iter()
            .find(|n| n.id == camera_node_id)
            .unwrap();
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
            &add_fog,
            &mut project,
            &content_tx,
            &content_state,
            &mut ui,
            &mut selection,
            &mut active_layer,
            &mut user_prefs,
        );

        let def = effective_def(&project, &layer_id);
        let vm =
            manifold_renderer::node_graph::scene_vm::SceneVm::from_def(&def).expect("scene vm");
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
            &action,
            &mut project,
            &content_tx,
            &content_state,
            &mut ui,
            &mut selection,
            &mut active_layer,
            &mut user_prefs,
        );
        assert!(
            !result.structural_change,
            "a param scrub is not a structural graph edit"
        );

        let after_def = effective_def(&project, &layer_id);
        let node = after_def
            .nodes
            .iter()
            .find(|n| n.id == fog_node_id)
            .unwrap();
        match node.params.get("fog_density") {
            Some(SerializedParamValue::Float { value }) => {
                assert_eq!(*value, 0.42, "fog density should have changed in the def")
            }
            other => panic!("expected Float, got {other:?}"),
        }
    }

    fn dispatch_modifier_action(
        action: ProjectAction,
    ) -> crate::scene_modifier_edit::SceneModifierAction {
        let (mut project, _layer_id, _) = scene_layer_project();
        let before = serde_json::to_value(&project).expect("project serializes");
        let (content_tx, content_rx) = crossbeam_channel::unbounded();
        let content_state = crate::content_state::ContentState::default();
        let mut ui = UIRoot::new();
        let mut selection = manifold_ui::UIState::new();
        let mut active_layer = None;
        let mut user_prefs = UserPrefs::load();
        dispatch_project(
            &action,
            &mut project,
            &content_tx,
            &content_state,
            &mut ui,
            &mut selection,
            &mut active_layer,
            &mut user_prefs,
        );
        assert_eq!(
            serde_json::to_value(&project).expect("project serializes"),
            before
        );
        let command = content_rx.try_recv().expect("modifier action is forwarded");
        let crate::content_command::ContentCommand::SceneModifier(action) = command else {
            panic!("modifier action must use the content command path");
        };
        action
    }

    #[test]
    fn scene_modifier_add_dispatches_preset_id_without_ui_mutation() {
        let action = dispatch_modifier_action(ProjectAction::SceneModifierApply(
            LayerId::new("owner-layer"),
            "scene_loop_preset".to_string(),
        ));
        assert!(matches!(action,
            crate::scene_modifier_edit::SceneModifierAction::Add(layer, preset)
                if layer == LayerId::new("owner-layer") && preset == "scene_loop_preset"));
    }

    #[test]
    fn scene_modifier_instance_actions_dispatch_stable_id_and_owner() {
        let owner = LayerId::new("owner-layer");
        let instance = manifold_core::NodeId::new("modifier-instance");
        for action in [
            ProjectAction::SceneModifierRemove(owner.clone(), instance.clone()),
            ProjectAction::SceneModifierMove(owner.clone(), instance.clone(), 2),
            ProjectAction::SceneModifierToggleEnabled(owner.clone(), instance.clone()),
            ProjectAction::SceneModifiersReorder(owner.clone(), vec![instance.clone()]),
            ProjectAction::SceneModifiersDuplicate(owner.clone(), vec![instance.clone()]),
            ProjectAction::SceneModifiersRemove(owner.clone(), vec![instance.clone()]),
        ] {
            let routed = dispatch_modifier_action(action);
            match routed {
                crate::scene_modifier_edit::SceneModifierAction::Remove(layer, id)
                | crate::scene_modifier_edit::SceneModifierAction::Toggle(layer, id) => {
                    assert_eq!(layer, owner);
                    assert_eq!(id, instance);
                }
                crate::scene_modifier_edit::SceneModifierAction::Move(layer, id, index) => {
                    assert_eq!(layer, owner);
                    assert_eq!(id, instance);
                    assert_eq!(index, 2);
                }
                crate::scene_modifier_edit::SceneModifierAction::Reorder(layer, ids)
                | crate::scene_modifier_edit::SceneModifierAction::Duplicate(layer, ids)
                | crate::scene_modifier_edit::SceneModifierAction::RemoveMany(layer, ids) => {
                    assert_eq!(layer, owner);
                    assert_eq!(ids, vec![instance.clone()]);
                }
                crate::scene_modifier_edit::SceneModifierAction::Add(..) => {
                    panic!("instance action routed as preset add")
                }
                crate::scene_modifier_edit::SceneModifierAction::Paste(..) => {
                    panic!("instance action routed as paste")
                }
                crate::scene_modifier_edit::SceneModifierAction::Preparation(..)
                | crate::scene_modifier_edit::SceneModifierAction::Retarget(..) => {
                    panic!("instance action routed as preparation")
                }
            }
        }
    }
}
