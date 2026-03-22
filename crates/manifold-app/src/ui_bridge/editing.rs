//! Editing-related dispatch: clip interaction, context menus, drag actions.
use manifold_core::{ClipId, LayerId};
use manifold_core::project::Project;
use manifold_core::types::{LayerType, GeneratorType};
use manifold_editing::commands::layer::{AddLayerCommand, DeleteLayerCommand};
use manifold_editing::service::EditingService;
use manifold_ui::PanelAction;

use crate::app::SelectionState;
use crate::dialog_path_memory::{self, DialogContext};
use crate::ui_root::UIRoot;
use crate::user_prefs::UserPrefs;
use super::DispatchResult;

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
            // Find the clip's layer index, layer ID, and end beat for UIState
            let (layer_idx, layer_id, clip_end_beat) = Some(&*project)
                .and_then(|p| p.timeline.layers.iter().enumerate()
                    .find_map(|(i, l)| l.clips.iter()
                        .find(|c| c.id == clip_id)
                        .map(|c| (i, l.layer_id.clone(), c.start_beat + c.duration_beats))))
                .unwrap_or((0, manifold_core::LayerId::default(), 0.0));

            if modifiers.shift {
                // Shift+Click: extend region from anchor to clip end.
                // From Unity InteractionOverlay.OnPointerClick (line 206-207).
                super::select_region_to_with_project(clip_end_beat, layer_idx, selection, &*project);
            } else if modifiers.command || modifiers.ctrl {
                // Cmd/Ctrl+Click: toggle clip in/out of selection, then update region bounds.
                // From Unity InteractionOverlay.OnPointerClick (line 208-211).
                selection.toggle_clip_selection(clip_id.clone(), layer_id);
                // Update region to encompass all selected clips (Fix #3)
                super::update_region_from_clip_selection_inline(selection, &*project);
            } else {
                // Plain click: select single clip (clears region, layers, insert cursor)
                selection.select_clip(clip_id.clone(), layer_id);
            }
            *active_layer = project.timeline.layers.get(layer_idx).map(|l| l.layer_id.clone());
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
                let snapped = ui.viewport.snap_to_grid(*beat);
                super::select_region_to_with_project(snapped, *layer, selection, &*project);
            } else {
                // Plain click: set insert cursor (clears everything, Ableton behavior).
                // From Unity InteractionOverlay.OnPointerClick (line 183).
                let lid = project.timeline.layers.get(*layer)
                    .map(|l| l.layer_id.clone()).unwrap_or_default();
                selection.set_insert_cursor(*beat, lid);
            }
            *active_layer = project.timeline.layers.get(*layer).map(|l| l.layer_id.clone());
            DispatchResult::structural()
        }
        PanelAction::TrackDoubleClicked(beat, layer) => {
            // From Unity InteractionOverlay.OnPointerClick double-click path:
            // Use FloorBeatToGrid (grid cell start), NOT SnapBeatToGrid (nearest line).
            let grid_step = ui.viewport.grid_step();
            let snapped = manifold_ui::snap::floor_beat_to_grid(*beat, grid_step);
            {
                let (cmd, _clip_id) = EditingService::create_clip_at_position(project, snapped, *layer, 4.0);
                { let _ = content_tx.try_send(ContentCommand::Execute(cmd)); }
                // Enforce non-overlap for the newly created clip
                if let Some(new_layer) = project.timeline.layers.get(*layer)
                    && let Some(new_clip) = new_layer.clips.last() {
                        let new_clip_clone = new_clip.clone();
                        let ignore = std::collections::HashSet::new();
                        let spb = 60.0 / project.settings.bpm;
                        let overlap_cmds = EditingService::enforce_non_overlap(
                            project, &new_clip_clone, *layer, &ignore, spb,
                        );
                        for cmd in overlap_cmds {
                            { let _ = content_tx.try_send(ContentCommand::Execute(cmd)); }
                        }
                        // Select the newly created clip
                        let new_lid = project.timeline.layers.get(*layer)
                            .map(|l| l.layer_id.clone()).unwrap_or_default();
                        selection.select_clip(new_clip_clone.id, new_lid);
                    }
            }
            *active_layer = project.timeline.layers.get(*layer).map(|l| l.layer_id.clone());
            DispatchResult::structural()
        }
        PanelAction::ViewportHoverChanged(_clip_id) => {
            // Hover state is already tracked on viewport panel
            DispatchResult::handled()
        }

        // ── Context menu actions ──────────────────────────────────
        PanelAction::ContextSplitAtPlayhead(clip_id) => {
            let beat = content_state.current_beat;
            {
                let spb = 60.0 / project.settings.bpm;
                if let Some(cmd) = EditingService::split_clip_at_beat(project, clip_id, beat, spb) {
                    { let _ = content_tx.try_send(ContentCommand::Execute(cmd)); }
                }
            }
            DispatchResult::structural()
        }
        PanelAction::ContextDeleteClip(clip_id) => {
            let clip_id = ClipId::new(clip_id.as_str());
            {
                let commands = EditingService::delete_clips(project, std::slice::from_ref(&clip_id), None, 0.0);
                if !commands.is_empty() {
                    for c in commands { let _ = content_tx.try_send(ContentCommand::Execute(c)); }
                }
            }
            selection.selected_clip_ids.remove(&clip_id);
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
                let spb = 60.0 / project.settings.bpm.max(1.0);
                let commands = EditingService::duplicate_clips(project, std::slice::from_ref(&clip_id), &region, spb);
                if !commands.is_empty() {
                    for c in commands { let _ = content_tx.try_send(ContentCommand::Execute(c)); }
                }
            }
            DispatchResult::structural()
        }
        PanelAction::ContextPasteAtTrack(beat, _layer) => {
            let _snapped = ui.viewport.snap_to_grid(*beat);
            {
                let _spb = 60.0 / project.settings.bpm;
                // TODO: browser paste not yet wired
                let result = manifold_editing::service::PasteResult { commands: Vec::new(), pasted_clip_ids: Vec::new(), skip_reason: None, skipped_count: 0 };
                if !result.commands.is_empty() {
                    for c in result.commands { let _ = content_tx.try_send(ContentCommand::Execute(c)); }
                    selection.selected_clip_ids.clear();
                    for id in result.pasted_clip_ids {
                        selection.selected_clip_ids.insert(id);
                    }
                    selection.primary_selected_clip_id = selection.selected_clip_ids.iter().next().cloned();
                    selection.selection_version += 1;
                }
            }
            DispatchResult::structural()
        }
        PanelAction::ContextAddVideoLayer(after_layer) => {
            {
                let idx = after_layer + 1;
                let name = format!("Layer {}", project.timeline.layers.len() + 1);
                let cmd = AddLayerCommand::new(
                    name,
                    LayerType::Video,
                    GeneratorType::None,
                    idx,
                    None,
                );
                { let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd); boxed.execute(project); let _ = content_tx.try_send(ContentCommand::Execute(boxed)); }
            }
            DispatchResult::structural()
        }
        PanelAction::ContextAddGeneratorLayer(after_layer) => {
            {
                let idx = after_layer + 1;
                let name = format!("Gen {}", project.timeline.layers.len() + 1);
                let cmd = AddLayerCommand::new(
                    name,
                    LayerType::Generator,
                    GeneratorType::Plasma,
                    idx,
                    None,
                );
                { let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd); boxed.execute(project); let _ = content_tx.try_send(ContentCommand::Execute(boxed)); }
            }
            DispatchResult::structural()
        }

        PanelAction::ContextDeleteLayer(layer_idx) => {
            {
                let idx = *layer_idx;
                if project.timeline.layers.len() > 1 && idx < project.timeline.layers.len() {
                    let layer = project.timeline.layers[idx].clone();
                    let cmd = DeleteLayerCommand::new(layer, idx);
                    { let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd); boxed.execute(project); let _ = content_tx.try_send(ContentCommand::Execute(boxed)); }
                }
            }
            DispatchResult::structural()
        }

        PanelAction::LayerHeaderRightClicked(_) => {
            // Handled by UIRoot::try_open_dropdown — should not reach dispatch
            DispatchResult::handled()
        }

        // Context menu items — not yet wired to subsystems
        PanelAction::ContextPasteAtLayer(layer_idx) => {
            // TODO: Wire to EditingService.Paste when clipboard is ported
            log::warn!("Paste at layer {} — not yet implemented", layer_idx);
            DispatchResult::handled()
        }
        PanelAction::ContextImportMidi(layer_idx) => {
            // Open file dialog for MIDI import
            let last_dir = dialog_path_memory::get_last_directory(
                DialogContext::MidiImport, user_prefs,
            );
            let mut dialog = rfd::FileDialog::new()
                .set_title("Import MIDI File")
                .add_filter("MIDI Files", &["mid", "midi"]);
            if !last_dir.is_empty() {
                dialog = dialog.set_directory(&last_dir);
            }
            if let Some(path) = dialog.pick_file() {
                let path_str = path.to_string_lossy().to_string();
                dialog_path_memory::remember_directory(
                    DialogContext::MidiImport, &path_str, user_prefs,
                );
                // Parse MIDI file and import to layer
                let notes = manifold_playback::midi_parser::MidiFileParser::parse_file(&path_str);
                if !notes.is_empty() {
                    let result = manifold_playback::midi_import::MidiImportService::import_to_layer(
                        &notes, *layer_idx, 0.0, project,
                    );
                    if result.success {
                        if let Some(undo_cmd) = result.undo_command {
                            let _ = content_tx.try_send(ContentCommand::Execute(undo_cmd));
                        }
                        log::info!("Imported {} clips from MIDI to layer {}", result.added_clips, layer_idx);
                    }
                }
            }
            DispatchResult::structural()
        }
        PanelAction::ContextGroupSelectedLayers => {
            // TODO: Wire to EditingService.GroupSelectedLayers
            log::warn!("Group selected layers — not yet implemented");
            DispatchResult::handled()
        }
        PanelAction::ContextUngroup(layer_idx) => {
            // TODO: Wire to EditingService.Ungroup
            log::warn!("Ungroup layer {} — not yet implemented", layer_idx);
            DispatchResult::handled()
        }

        // Right-click actions (intercepted by UIRoot for dropdown; should not reach dispatch)
        PanelAction::ClipRightClicked(_) | PanelAction::TrackRightClicked(_, _) => {
            DispatchResult::handled()
        }

        // Generic dropdown fallback (should not normally fire)
        PanelAction::DropdownSelected(index) => {
            log::debug!("Dropdown selected: {} (no context)", index);
            DispatchResult::handled()
        }

        _ => DispatchResult::unhandled(),
    }
}
