//! Editing-related dispatch: clip interaction, context menus, drag actions.
use manifold_core::PresetTypeId;
use manifold_core::project::Project;
use manifold_core::types::LayerType;
use manifold_core::{Beats, ClipId, LayerId};
use manifold_editing::commands::layer::{AddLayerCommand, DeleteLayerCommand};
use manifold_editing::service::EditingService;
use manifold_ui::PanelAction;

use super::DispatchResult;
use crate::app::SelectionState;
use crate::dialog_path_memory::{self, DialogContext};
use crate::ui_root::UIRoot;
use crate::user_prefs::UserPrefs;

pub(super) fn dispatch_editing(
    action: &PanelAction,
    project: &mut Project,
    content_tx: &crossbeam_channel::Sender<crate::content_command::ContentCommand>,
    content_state: &crate::content_state::ContentState,
    ui: &mut UIRoot,
    selection: &mut SelectionState,
    active_layer: &mut Option<LayerId>,
    user_prefs: &mut UserPrefs,
) -> DispatchResult {
    use crate::content_command::ContentCommand;
    match action {
        // ── Viewport clip interaction ─────────────────────────────
        PanelAction::ClipClicked(clip_id, modifiers) => {
            let clip_id = ClipId::new(clip_id.as_str());
            // Find the clip's layer index and layer ID for UIState. The end
            // beat this used to carry for the shift-click region extension
            // (D2's now-deleted `select_region_to_with_project` call below)
            // is no longer needed — the clip-range gesture looks up its own
            // anchor/target beats internally.
            let (layer_idx, layer_id) = Some(&*project)
                .and_then(|p| {
                    p.timeline.layers.iter().enumerate().find_map(|(i, l)| {
                        l.clips
                            .iter()
                            .find(|c| c.id == clip_id)
                            .map(|_| (i, l.layer_id.clone()))
                    })
                })
                .unwrap_or((0, manifold_core::LayerId::default()));

            if modifiers.shift {
                // D2: shift-click on a CLIP is a clip-range selection
                // (contiguous whole clips on the anchor's layer), not a
                // region — the Unity `select_region_to_with_project` port
                // firing here was S1/S3's root
                // (`docs/TIMELINE_INTERACTION_P1_SPEC.md`). The empty-lane
                // shift path (`TrackClicked` below) keeps calling
                // `select_region_to_with_project`.
                super::select_clip_range_to_with_project(&clip_id, selection, &*project);
            } else if modifiers.command || modifiers.ctrl {
                // Cmd/Ctrl+Click: toggle clip in/out of selection. D1: no region
                // is synthesised from the clip set (the old
                // `update_region_from_clip_selection_inline` sync is deleted) —
                // a multi-clip selection is a pure `Clips` selection, so the
                // redundant region band no longer renders (begins the S1 fix).
                selection.toggle_clip_selection(clip_id.clone(), layer_id);
            } else {
                // Plain click: select single clip (clears region, layers, insert cursor)
                selection.select_clip(clip_id.clone(), layer_id);
            }
            *active_layer = project
                .timeline
                .layers
                .get(layer_idx)
                .map(|l| l.layer_id.clone());
            DispatchResult::structural()
        }
        PanelAction::ClipDoubleClicked(_clip_id) => {
            // Future: open clip properties or enter clip editing mode
            DispatchResult::handled()
        }
        PanelAction::TrackClicked(beat, layer, modifiers) => {
            if modifiers.shift {
                // Shift+Click on empty area: extend region from anchor to beat/layer.
                // From Unity InteractionOverlay.OnPointerClick (line 177-180).
                let snapped = ui.viewport.snap_to_grid(Beats::from_f32(*beat));
                super::select_region_to_with_project(snapped, *layer, selection, &*project);
            } else {
                // Plain click: set insert cursor (clears everything, Ableton behavior).
                // From Unity InteractionOverlay.OnPointerClick (line 183).
                let lid = project
                    .timeline
                    .layers
                    .get(*layer)
                    .map(|l| l.layer_id.clone())
                    .unwrap_or_default();
                selection.set_insert_cursor(Beats::from_f32(*beat), lid);
            }
            *active_layer = project
                .timeline
                .layers
                .get(*layer)
                .map(|l| l.layer_id.clone());
            DispatchResult::structural()
        }
        PanelAction::TrackDoubleClicked(beat, layer) => {
            // From Unity InteractionOverlay.OnPointerClick double-click path:
            // Use FloorBeatToGrid (grid cell start), NOT SnapBeatToGrid (nearest line).
            let grid_step = Beats::from_f32(ui.viewport.grid_step());
            let snapped = manifold_ui::snap::floor_beat_to_grid(Beats::from_f32(*beat), grid_step);
            {
                let spb = 60.0 / project.settings.bpm.0.max(1.0);
                // AddClipCommand enforces non-overlap internally.
                if let Some((cmd, clip_id)) = EditingService::create_clip_at_position(
                    project,
                    snapped,
                    *layer,
                    Beats::from_f32(4.0),
                    spb,
                ) {
                    ContentCommand::send(content_tx, ContentCommand::Execute(cmd));

                    // Select the newly created clip
                    let new_lid = project
                        .timeline
                        .layers
                        .get(*layer)
                        .map(|l| l.layer_id.clone())
                        .unwrap_or_default();
                    selection.select_clip(clip_id, new_lid);
                }
            }
            *active_layer = project
                .timeline
                .layers
                .get(*layer)
                .map(|l| l.layer_id.clone());
            DispatchResult::structural()
        }
        PanelAction::ViewportHoverChanged(_clip_id) => {
            // Hover state is already tracked on viewport panel
            DispatchResult::handled()
        }

        // ── Context menu actions ──────────────────────────────────
        PanelAction::ContextSplitAtPlayhead(clip_id) => {
            let beat = content_state.current_beat.as_f32();
            {
                let spb = 60.0 / project.settings.bpm.0;
                if let Some(cmd) =
                    EditingService::split_clip_at_beat(project, clip_id, Beats::from_f32(beat), spb)
                {
                    {
                        ContentCommand::send(content_tx, ContentCommand::Execute(Box::new(cmd)));
                    }
                }
            }
            DispatchResult::structural()
        }
        PanelAction::ContextDeleteClip(clip_id) => {
            let clip_id = ClipId::new(clip_id.as_str());
            // If the right-clicked clip is part of a multi-selection, delete every
            // selected clip in one undo step; otherwise just this clip. Mirrors the
            // selection-aware shape of ContextDuplicateLayer.
            let target_ids: Vec<ClipId> = if selection.is_selected(&clip_id) {
                selection.get_selected_clip_ids()
            } else {
                vec![clip_id.clone()]
            };
            let spb = 60.0 / project.settings.bpm.0.max(1.0);
            let commands = EditingService::delete_clips(project, &target_ids, None, spb);
            if !commands.is_empty() {
                ContentCommand::send(
                    content_tx,
                    ContentCommand::ExecuteBatch(commands, "Delete clips".to_string()),
                );
            }
            selection.deselect_clips(&target_ids);
            DispatchResult::structural()
        }
        PanelAction::ContextDuplicateClip(clip_id) => {
            let clip_id = ClipId::new(clip_id.as_str());
            {
                // Calculate region from the single clip for proper offset
                let mut region = manifold_core::selection::SelectionRegion::default();
                if let Some(clip) = project.timeline.find_clip_by_id(&clip_id) {
                    region.start_beat = clip.start_beat;
                    region.end_beat = clip.start_beat + clip.duration_beats;
                    region.is_active = true;
                }
                let spb = 60.0 / project.settings.bpm.0.max(1.0);
                let commands = EditingService::duplicate_clips(
                    project,
                    std::slice::from_ref(&clip_id),
                    &region,
                    spb,
                );
                if !commands.is_empty() {
                    ContentCommand::send(
                        content_tx,
                        ContentCommand::ExecuteBatch(commands, "Duplicate clip".to_string()),
                    );
                }
            }
            DispatchResult::structural()
        }
        PanelAction::ContextPasteAtTrack(beat, layer) => {
            // Paste the clip clipboard at the clicked beat/layer — same content-thread
            // PasteClips path as Cmd+V (EditingService owns the clipboard).
            let snapped = ui.viewport.snap_to_grid(Beats::from_f32(*beat));
            let (tx, rx) = std::sync::mpsc::channel();
            ContentCommand::send(
                content_tx,
                ContentCommand::PasteClips {
                    target_beat: snapped,
                    target_layer: *layer as i32,
                    result_tx: tx,
                },
            );
            // Brief wait for the pasted IDs so we can select them (matches the
            // keyboard paste in input_host::paste_clips).
            if let Ok(pasted_ids) = rx.recv_timeout(std::time::Duration::from_millis(100))
                && !pasted_ids.is_empty()
            {
                selection.select_clips(pasted_ids);
            }
            DispatchResult::structural()
        }
        PanelAction::ContextAddVideoLayer(after_layer) => {
            {
                // Re-resolve the target layer's current index at execution time
                // (BUG-031) — the id survives any reordering between menu-open
                // and click; a baked-in index wouldn't.
                if let Some((idx, _)) = project.timeline.find_layer_by_id(after_layer.as_str()) {
                    let idx = idx + 1;
                    let name = format!("Layer {}", project.timeline.layers.len() + 1);
                    let cmd =
                        AddLayerCommand::new(name, LayerType::Video, PresetTypeId::NONE, idx, None);
                    {
                        let mut boxed: Box<dyn manifold_editing::command::Command + Send> =
                            Box::new(cmd);
                        boxed.execute(project);
                        ContentCommand::send(content_tx, ContentCommand::Execute(boxed));
                    }
                }
            }
            DispatchResult::structural()
        }
        PanelAction::ContextAddGeneratorLayer(after_layer) => {
            {
                if let Some((idx, _)) = project.timeline.find_layer_by_id(after_layer.as_str()) {
                    let idx = idx + 1;
                    let name = format!("Gen {}", project.timeline.layers.len() + 1);
                    let cmd = AddLayerCommand::new(
                        name,
                        LayerType::Generator,
                        PresetTypeId::PLASMA,
                        idx,
                        None,
                    );
                    {
                        let mut boxed: Box<dyn manifold_editing::command::Command + Send> =
                            Box::new(cmd);
                        boxed.execute(project);
                        ContentCommand::send(content_tx, ContentCommand::Execute(boxed));
                    }
                }
            }
            DispatchResult::structural()
        }
        PanelAction::ContextAddAudioLayer(after_layer) => {
            {
                if let Some((idx, _)) = project.timeline.find_layer_by_id(after_layer.as_str()) {
                    let idx = idx + 1;
                    let name = format!("Audio {}", project.timeline.layers.len() + 1);
                    let cmd =
                        AddLayerCommand::new(name, LayerType::Audio, PresetTypeId::NONE, idx, None);
                    {
                        let mut boxed: Box<dyn manifold_editing::command::Command + Send> =
                            Box::new(cmd);
                        boxed.execute(project);
                        ContentCommand::send(content_tx, ContentCommand::Execute(boxed));
                    }
                }
            }
            DispatchResult::structural()
        }

        PanelAction::ContextDeleteLayer(layer_id) => {
            {
                if project.timeline.layers.len() > 1
                    && let Some((_, layer)) = project.timeline.find_layer_by_id(layer_id.as_str())
                {
                    let layer = layer.clone();
                    let cmd = DeleteLayerCommand::new(layer);
                    {
                        let mut boxed: Box<dyn manifold_editing::command::Command + Send> =
                            Box::new(cmd);
                        boxed.execute(project);
                        ContentCommand::send(content_tx, ContentCommand::Execute(boxed));
                    }
                }
            }
            DispatchResult::structural()
        }

        PanelAction::ContextDuplicateLayer(layer_id) => {
            {
                if project
                    .timeline
                    .find_layer_by_id(layer_id.as_str())
                    .is_some()
                {
                    // If the right-clicked layer is among the selected layers, duplicate the
                    // full selection; otherwise duplicate just this layer.
                    let ids: Vec<LayerId> = if selection.selected_layer_ids.contains(layer_id) {
                        selection.selected_layer_ids.iter().cloned().collect()
                    } else {
                        vec![layer_id.clone()]
                    };
                    if let Some(mut cmd) = EditingService::duplicate_layers(project, &ids) {
                        cmd.execute(project);
                        ContentCommand::send(
                            content_tx,
                            ContentCommand::ExecuteBatch(vec![cmd], "Duplicate Layers".to_string()),
                        );
                    }
                }
            }
            DispatchResult::structural()
        }

        PanelAction::LayerHeaderRightClicked(_) => {
            // Handled by UIRoot::try_open_dropdown — should not reach dispatch
            DispatchResult::handled()
        }

        // Context menu items — not yet wired to subsystems
        PanelAction::ContextPasteAtLayer(layer_id) => {
            // Paste at the current playhead beat on the right-clicked layer.
            // Re-resolve the id to its current index at execution time (BUG-031).
            let Some((idx, _)) = project.timeline.find_layer_by_id(layer_id.as_str()) else {
                return DispatchResult::structural();
            };
            let (tx, rx) = std::sync::mpsc::channel();
            ContentCommand::send(
                content_tx,
                ContentCommand::PasteClips {
                    target_beat: content_state.current_beat,
                    target_layer: idx as i32,
                    result_tx: tx,
                },
            );
            if let Ok(pasted_ids) = rx.recv_timeout(std::time::Duration::from_millis(100))
                && !pasted_ids.is_empty()
            {
                selection.select_clips(pasted_ids);
            }
            DispatchResult::structural()
        }
        PanelAction::ContextImportMidi(layer_id) => {
            // Open file dialog for MIDI import
            let last_dir =
                dialog_path_memory::get_last_directory(DialogContext::MidiImport, user_prefs);
            let mut dialog = rfd::FileDialog::new()
                .set_title("Import MIDI File")
                .add_filter("MIDI Files", &["mid", "midi"]);
            if !last_dir.is_empty() {
                dialog = dialog.set_directory(&last_dir);
            }
            if let Some(path) = dialog.pick_file() {
                let path_str = path.to_string_lossy().to_string();
                dialog_path_memory::remember_directory(
                    DialogContext::MidiImport,
                    &path_str,
                    user_prefs,
                );
                // Parse MIDI file and import to layer
                let notes = manifold_playback::midi_parser::MidiFileParser::parse_file(&path_str);
                if !notes.is_empty() {
                    // `layer_id` is already the stable target — no index resolution needed.
                    let target_layer_id = layer_id.clone();
                    let result = manifold_playback::midi_import::MidiImportService::import_to_layer(
                        &notes,
                        &target_layer_id,
                        0.0,
                        project,
                    );
                    if result.success {
                        if let Some(undo_cmd) = result.undo_command {
                            ContentCommand::send(content_tx, ContentCommand::Execute(undo_cmd));
                        }
                        log::info!(
                            "Imported {} clips from MIDI to layer '{}'",
                            result.added_clips,
                            target_layer_id
                        );
                    }
                }
            }
            DispatchResult::structural()
        }
        PanelAction::ContextGroupSelectedLayers => {
            let selected_ids: Vec<manifold_core::LayerId> =
                selection.selected_layer_ids.iter().cloned().collect();
            if selected_ids.len() >= 2 {
                let valid = selected_ids.iter().all(|id| {
                    project
                        .timeline
                        .layers
                        .iter()
                        .find(|l| l.layer_id == *id)
                        .is_some_and(|l| l.parent_layer_id.is_none() && !l.is_group())
                });
                if valid {
                    let original_order = project.timeline.layers.clone();
                    let cmd = manifold_editing::commands::layer::GroupLayersCommand::new(
                        selected_ids,
                        original_order,
                    );
                    {
                        let mut boxed: Box<dyn manifold_editing::command::Command + Send> =
                            Box::new(cmd);
                        boxed.execute(project);
                        ContentCommand::send(content_tx, ContentCommand::Execute(boxed));
                    }
                    selection.clear_selection();
                }
            }
            ContentCommand::send(content_tx, ContentCommand::MarkCompositorDirty);
            DispatchResult::structural()
        }
        PanelAction::ContextUngroup(layer_id) => {
            if let Some((_, layer)) = project.timeline.find_layer_by_id(layer_id.as_str())
                && layer.is_group()
            {
                let group_layer = layer.clone();
                let group_id = group_layer.layer_id.clone();
                let child_ids: Vec<manifold_core::LayerId> = project
                    .timeline
                    .layers
                    .iter()
                    .filter(|l| l.parent_layer_id.as_ref() == Some(&group_id))
                    .map(|l| l.layer_id.clone())
                    .collect();
                let original_order = project.timeline.layers.clone();
                let cmd = manifold_editing::commands::layer::UngroupLayersCommand::new(
                    group_layer,
                    child_ids,
                    original_order,
                );
                {
                    let mut boxed: Box<dyn manifold_editing::command::Command + Send> =
                        Box::new(cmd);
                    boxed.execute(project);
                    ContentCommand::send(content_tx, ContentCommand::Execute(boxed));
                }
            }
            ContentCommand::send(content_tx, ContentCommand::MarkCompositorDirty);
            DispatchResult::structural()
        }

        PanelAction::ContextSetLayerColor(layer_id, color) => {
            use crate::content_command::ContentCommand;
            let r = color.r as f32 / 255.0;
            let g = color.g as f32 / 255.0;
            let b = color.b as f32 / 255.0;
            let a = color.a as f32 / 255.0;
            // Local project mutation — resolved by id, not a baked-in index (BUG-031).
            if let Some((_, layer)) = project.timeline.find_layer_by_id_mut(layer_id.as_str()) {
                layer.layer_color = manifold_core::color::Color { r, g, b, a };
            }
            // Mirror to content thread — this closure runs later, on a different
            // thread, so it re-resolves the id there too rather than capturing
            // an index that may be stale by the time it executes.
            let layer_id = layer_id.clone();
            ContentCommand::send(
                content_tx,
                ContentCommand::MutateProject(Box::new(move |p| {
                    if let Some((_, layer)) = p.timeline.find_layer_by_id_mut(layer_id.as_str()) {
                        layer.layer_color = manifold_core::color::Color { r, g, b, a };
                    }
                })),
            );
            DispatchResult::structural()
        }

        // Right-click actions (intercepted by UIRoot for dropdown; should not reach dispatch)
        PanelAction::ClipRightClicked(_)
        | PanelAction::TrackRightClicked(_, _)
        | PanelAction::AutomationLaneRightClicked(..) => DispatchResult::handled(),

        // Generic dropdown fallback (should not normally fire)
        PanelAction::DropdownSelected(index) => {
            log::debug!("Dropdown selected: {} (no context)", index);
            DispatchResult::handled()
        }

        _ => DispatchResult::unhandled(),
    }
}
