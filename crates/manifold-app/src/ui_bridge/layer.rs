//! Layer-related dispatch: mute/solo/click/chevron/blend/drag/add/delete.

use manifold_core::PresetTypeId;
use manifold_core::project::Project;
use manifold_core::types::{BlendMode, LayerType};
use manifold_core::{Beats, LayerId};
use manifold_editing::commands::audio_setup::SetLayerAudioSendCommand;
use manifold_editing::commands::layer::{AddLayerCommand, DeleteLayerCommand};
use manifold_editing::commands::settings::ChangeLayerBlendModeCommand;
use manifold_editing::service::EditingService;
use manifold_ui::LayerAction;

use super::DispatchResult;
use crate::app::SelectionState;
use crate::ui_root::UIRoot;

pub(super) fn dispatch_layer(
    action: &LayerAction,
    project: &mut Project,
    content_tx: &crossbeam_channel::Sender<crate::content_command::ContentCommand>,
    content_state: &crate::content_state::ContentState,
    ui: &mut UIRoot,
    selection: &mut SelectionState,
    active_layer: &mut Option<LayerId>,
) -> DispatchResult {
    use crate::content_command::ContentCommand;
    match action {
        // ── Layer operations ───────────────────────────────────────
        LayerAction::ToggleMute(id) => {
            // If the clicked layer is part of a multi-selection, apply to all selected layers
            let target_ids: Vec<LayerId> = if selection.selected_layer_ids.len() > 1
                && selection.is_layer_selected(id)
            {
                selection.selected_layer_ids.iter().cloned().collect()
            } else {
                vec![id.clone()]
            };
            // Determine new mute state from clicked layer (toggle)
            let new_muted = project
                .timeline
                .find_layer_by_id(id)
                .map(|(_, l)| !l.is_muted)
                .unwrap_or(true);
            for lid in &target_ids {
                if let Some((_, layer)) = project.timeline.find_layer_by_id_mut(lid) {
                    layer.is_muted = new_muted;
                }
            }
            ContentCommand::send(
                content_tx,
                ContentCommand::MutateProject(Box::new(move |p| {
                    for lid in &target_ids {
                        if let Some((_, layer)) = p.timeline.find_layer_by_id_mut(lid) {
                            layer.is_muted = new_muted;
                        }
                    }
                })),
            );
            DispatchResult::handled()
        }
        LayerAction::ToggleAnalysisOnly(id) => {
            // Audio "analysis-only" output: silent to master, still feeding the
            // send. Direct dual-write (local + content thread) like mute/solo —
            // a live perform toggle, not an undo step. Multi-select aware.
            let target_ids: Vec<LayerId> = if selection.selected_layer_ids.len() > 1
                && selection.is_layer_selected(id)
            {
                selection.selected_layer_ids.iter().cloned().collect()
            } else {
                vec![id.clone()]
            };
            let new_analysis = project
                .timeline
                .find_layer_by_id(id)
                .map(|(_, l)| !l.analysis_only)
                .unwrap_or(true);
            for lid in &target_ids {
                if let Some((_, layer)) = project.timeline.find_layer_by_id_mut(lid) {
                    layer.analysis_only = new_analysis;
                }
            }
            ContentCommand::send(
                content_tx,
                ContentCommand::MutateProject(Box::new(move |p| {
                    for lid in &target_ids {
                        if let Some((_, layer)) = p.timeline.find_layer_by_id_mut(lid) {
                            layer.analysis_only = new_analysis;
                        }
                    }
                })),
            );
            DispatchResult::handled()
        }
        LayerAction::ToggleSolo(id) => {
            let target_ids: Vec<LayerId> = if selection.selected_layer_ids.len() > 1
                && selection.is_layer_selected(id)
            {
                selection.selected_layer_ids.iter().cloned().collect()
            } else {
                vec![id.clone()]
            };
            let new_solo = project
                .timeline
                .find_layer_by_id(id)
                .map(|(_, l)| !l.is_solo)
                .unwrap_or(true);
            for lid in &target_ids {
                if let Some((_, layer)) = project.timeline.find_layer_by_id_mut(lid) {
                    layer.is_solo = new_solo;
                }
            }
            ContentCommand::send(
                content_tx,
                ContentCommand::MutateProject(Box::new(move |p| {
                    for lid in &target_ids {
                        if let Some((_, layer)) = p.timeline.find_layer_by_id_mut(lid) {
                            layer.is_solo = new_solo;
                        }
                    }
                })),
            );
            DispatchResult::handled()
        }
        LayerAction::ToggleLed(id) => {
            let target_ids: Vec<LayerId> = if selection.selected_layer_ids.len() > 1
                && selection.is_layer_selected(id)
            {
                selection.selected_layer_ids.iter().cloned().collect()
            } else {
                vec![id.clone()]
            };
            let new_led = project
                .timeline
                .find_layer_by_id(id)
                .map(|(_, l)| !l.blit_to_led)
                .unwrap_or(true);
            for lid in &target_ids {
                if let Some((_, layer)) = project.timeline.find_layer_by_id_mut(lid) {
                    layer.blit_to_led = new_led;
                }
            }
            ContentCommand::send(
                content_tx,
                ContentCommand::MutateProject(Box::new(move |p| {
                    for lid in &target_ids {
                        if let Some((_, layer)) = p.timeline.find_layer_by_id_mut(lid) {
                            layer.blit_to_led = new_led;
                        }
                    }
                })),
            );
            DispatchResult::handled()
        }
        LayerAction::LayerClicked(id, modifiers) => {
            // From Unity UIState.cs layer selection methods (lines 247-333).
            let layer_id = id.clone();
            *active_layer = Some(layer_id.clone());

            // Clear effect selection when switching focus to layer headers
            ui.inspector.clear_effect_selection(&mut ui.tree);

            {
                if modifiers.shift {
                    // Shift+Click: range select from primary to target
                    let ui_layers = crate::ui_translate::layers_to_ui(&project.timeline.layers);
                    selection.select_layer_range(&layer_id, &ui_layers);
                } else if modifiers.ctrl || modifiers.command {
                    // Cmd/Ctrl+Click: toggle layer in/out of selection
                    selection.toggle_layer_selection(layer_id);
                } else {
                    // Plain click: select single layer (clears clips, region, insert cursor)
                    selection.select_layer(layer_id);
                }
            }

            DispatchResult::structural()
        }
        LayerAction::LayerDoubleClicked(_id) => {
            // Intercepted by app.rs — opens text input for layer rename
            DispatchResult::handled()
        }
        LayerAction::ChevronClicked(id) => {
            let target_ids: Vec<LayerId> = if selection.selected_layer_ids.len() > 1
                && selection.is_layer_selected(id)
            {
                selection.selected_layer_ids.iter().cloned().collect()
            } else {
                vec![id.clone()]
            };
            let new_collapsed = project
                .timeline
                .find_layer_by_id(id)
                .map(|(_, l)| !l.is_collapsed)
                .unwrap_or(true);
            for lid in &target_ids {
                if let Some((_, layer)) = project.timeline.find_layer_by_id_mut(lid) {
                    layer.is_collapsed = new_collapsed;
                }
            }
            ContentCommand::send(
                content_tx,
                ContentCommand::MutateProject(Box::new(move |p| {
                    for lid in &target_ids {
                        if let Some((_, layer)) = p.timeline.find_layer_by_id_mut(lid) {
                            layer.is_collapsed = new_collapsed;
                        }
                    }
                })),
            );
            DispatchResult::structural()
        }
        LayerAction::BlendModeClicked(_id) => {
            // Intercepted by UIRoot::try_open_dropdown (opens dropdown at button).
            DispatchResult::handled()
        }
        LayerAction::SetBlendMode(id, mode_str) => {
            {
                if let Some((_, layer)) = project.timeline.find_layer_by_id(id) {
                    let layer_id = layer.layer_id.clone();
                    let old_mode = layer.default_blend_mode;
                    if let Some(new_mode) = BlendMode::ALL
                        .iter()
                        .find(|m| format!("{:?}", m) == *mode_str)
                    {
                        let cmd = ChangeLayerBlendModeCommand::new(layer_id, old_mode, *new_mode);
                        {
                            let mut boxed: Box<dyn manifold_editing::command::Command + Send> =
                                Box::new(cmd);
                            boxed.execute(project);
                            ContentCommand::send(content_tx, ContentCommand::Execute(boxed));
                        }
                    }
                }
            }
            DispatchResult::structural()
        }
        LayerAction::SetLayerAudioSend(id, send_id) => {
            // Layer-centric routing: this layer feeds the chosen send (or none).
            // Additive — the target send keeps its capture flag, so routing a
            // layer onto a default send makes a live capture+layer mix.
            if let Some((_, layer)) = project.timeline.find_layer_by_id(id) {
                let layer_id = layer.layer_id.clone();
                let mut boxed: Box<dyn manifold_editing::command::Command + Send> =
                    Box::new(SetLayerAudioSendCommand::new(layer_id, send_id.clone()));
                boxed.execute(project);
                ContentCommand::send(content_tx, ContentCommand::Execute(boxed));
            }
            DispatchResult::structural()
        }
        LayerAction::ExpandLayer(id) => {
            // Collapse/expand is a view-state toggle (MutateProject, not undoable),
            // multi-select aware — same pattern as ChevronClicked.
            let target_ids: Vec<LayerId> = if selection.selected_layer_ids.len() > 1
                && selection.is_layer_selected(id)
            {
                selection.selected_layer_ids.iter().cloned().collect()
            } else {
                vec![id.clone()]
            };
            for lid in &target_ids {
                if let Some((_, layer)) = project.timeline.find_layer_by_id_mut(lid) {
                    layer.is_collapsed = false;
                }
            }
            ContentCommand::send(
                content_tx,
                ContentCommand::MutateProject(Box::new(move |p| {
                    for lid in &target_ids {
                        if let Some((_, layer)) = p.timeline.find_layer_by_id_mut(lid) {
                            layer.is_collapsed = false;
                        }
                    }
                })),
            );
            DispatchResult::structural()
        }
        LayerAction::CollapseLayer(id) => {
            // Collapse/expand is a view-state toggle (MutateProject, not undoable),
            // multi-select aware — same pattern as ChevronClicked.
            let target_ids: Vec<LayerId> = if selection.selected_layer_ids.len() > 1
                && selection.is_layer_selected(id)
            {
                selection.selected_layer_ids.iter().cloned().collect()
            } else {
                vec![id.clone()]
            };
            for lid in &target_ids {
                if let Some((_, layer)) = project.timeline.find_layer_by_id_mut(lid) {
                    layer.is_collapsed = true;
                }
            }
            ContentCommand::send(
                content_tx,
                ContentCommand::MutateProject(Box::new(move |p| {
                    for lid in &target_ids {
                        if let Some((_, layer)) = p.timeline.find_layer_by_id_mut(lid) {
                            layer.is_collapsed = true;
                        }
                    }
                })),
            );
            DispatchResult::structural()
        }
        LayerAction::FolderClicked(_id) => {
            log::info!("Folder clicked (file picker not yet implemented)");
            DispatchResult::handled()
        }
        LayerAction::NewClipClicked(id) => {
            let beat = content_state.current_beat;
            let spb = 60.0 / project.settings.bpm.0.max(1.0);
            // create_clip_at_position takes a positional index; resolve the
            // stable id to its current row against the live model.
            if let Some((layer_idx, _)) = project.timeline.find_layer_by_id(id)
                && let Some((cmd, _)) = EditingService::create_clip_at_position(
                    project,
                    beat,
                    layer_idx,
                    Beats(4.0),
                    spb,
                )
            {
                ContentCommand::send(content_tx, ContentCommand::Execute(cmd));
            }
            DispatchResult::structural()
        }
        LayerAction::AddGenClipClicked(id) => {
            let beat = content_state.current_beat;
            let spb = 60.0 / project.settings.bpm.0.max(1.0);
            if let Some((layer_idx, _)) = project.timeline.find_layer_by_id(id)
                && let Some((cmd, _)) = EditingService::create_clip_at_position(
                    project,
                    beat,
                    layer_idx,
                    Beats(4.0),
                    spb,
                )
            {
                ContentCommand::send(content_tx, ContentCommand::Execute(cmd));
            }
            DispatchResult::structural()
        }
        LayerAction::MidiInputClicked(_id) => {
            // Intercepted by UIRoot::try_open_dropdown (opens dropdown at button).
            DispatchResult::handled()
        }
        LayerAction::MidiChannelClicked(_id) => {
            // Intercepted by UIRoot::try_open_dropdown (opens dropdown at button).
            DispatchResult::handled()
        }
        LayerAction::MidiDeviceClicked(_id) => {
            // Intercepted by UIRoot::try_open_dropdown (opens dropdown at button).
            DispatchResult::handled()
        }
        LayerAction::LayerDragStarted(_) | LayerAction::LayerDragMoved(..) => {
            DispatchResult::handled()
        }
        LayerAction::LayerDragEnded(from, to) => {
            // From Unity LayerHeaderPanel.HandleDragEnd + ReorderLayerCommand.
            // Supports multi-select: if dragged layer is part of a selection,
            // all selected layers move as a group.
            if from != to {
                let dragged_id = project
                    .timeline
                    .layers
                    .get(*from)
                    .map(|l| l.layer_id.clone())
                    .unwrap_or_default();
                let is_multi = selection.selected_layer_ids.len() > 1
                    && selection.is_layer_selected(&dragged_id);

                let old_order = project.timeline.layers.clone();
                let mut new_order = old_order.clone();

                // Determine parent for the moved layer(s). Dropping ON a group
                // means "join that group" (parent = group), not "adopt the group's
                // own parent".
                let target_layer = old_order.get(*to);
                let target_is_group = target_layer.is_some_and(|l| l.is_group());
                let target_parent = if target_is_group {
                    target_layer.map(|l| l.layer_id.clone())
                } else {
                    target_layer.and_then(|l| l.parent_layer_id.clone())
                };

                if is_multi {
                    // Multi-select: move all selected layers as a group
                    let selected_ids: Vec<LayerId> =
                        selection.selected_layer_ids.iter().cloned().collect();

                    // If target is within the selection, reordering is a no-op
                    let target_id = old_order.get(*to).map(|l| &l.layer_id);
                    if target_id.is_some_and(|tid| selected_ids.contains(tid)) {
                        return DispatchResult::handled();
                    }

                    // Remove selected layers (preserving their relative order)
                    let mut moving: Vec<_> = new_order
                        .iter()
                        .filter(|l| selected_ids.contains(&l.layer_id))
                        .cloned()
                        .collect();
                    new_order.retain(|l| !selected_ids.contains(&l.layer_id));

                    // Find insertion point by locating the target layer in the
                    // reduced array (raw index is invalid after removals).
                    let target_layer_id = old_order.get(*to).map(|l| &l.layer_id);
                    let target_insert = target_layer_id
                        .and_then(|tid| new_order.iter().position(|l| l.layer_id == *tid))
                        .map(|pos| {
                            if target_is_group {
                                // Insert as first child (right after the group header)
                                pos + 1
                            } else if *to > *from {
                                pos + 1
                            } else {
                                pos
                            }
                        })
                        .unwrap_or_else(|| (*to).min(new_order.len()));

                    // Update parent for all moved layers
                    for layer in &mut moving {
                        layer.parent_layer_id = target_parent.clone();
                    }

                    // Insert the group at target
                    for (offset, layer) in moving.into_iter().enumerate() {
                        let pos = (target_insert + offset).min(new_order.len());
                        new_order.insert(pos, layer);
                    }
                } else if *from < new_order.len() && *to <= new_order.len() {
                    // Single layer move.
                    let layer = new_order.remove(*from);

                    let insert_at = if target_is_group {
                        // Dropping on a group: insert as first child (right
                        // after the group header in the post-remove array).
                        let group_id = &old_order[*to].layer_id;
                        new_order
                            .iter()
                            .position(|l| l.layer_id == *group_id)
                            .map(|pos| pos + 1)
                            .unwrap_or_else(|| (*to).min(new_order.len()))
                    } else {
                        // Insert at *to: indicator shows "after target" when
                        // moving down and "before target" when moving up. After
                        // removing the source, inserting at *to lands exactly
                        // there.
                        (*to).min(new_order.len())
                    };

                    new_order.insert(insert_at, layer);
                    new_order[insert_at].parent_layer_id = target_parent;
                }

                // Build parent ID maps for undo
                let mut old_parents = std::collections::HashMap::new();
                let mut new_parents = std::collections::HashMap::new();
                for l in &old_order {
                    old_parents.insert(l.layer_id.clone(), l.parent_layer_id.clone());
                }
                for l in &new_order {
                    new_parents.insert(l.layer_id.clone(), l.parent_layer_id.clone());
                }

                if old_order.iter().map(|l| &l.layer_id).collect::<Vec<_>>()
                    != new_order.iter().map(|l| &l.layer_id).collect::<Vec<_>>()
                {
                    let cmd = manifold_editing::commands::layer::ReorderLayerCommand::new(
                        old_order,
                        new_order,
                        old_parents,
                        new_parents,
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

        // ── Layer management ───────────────────────────────────────
        LayerAction::AddLayerClicked => {
            {
                let count = project.timeline.layers.len();
                let name = format!("Layer {}", count + 1);
                let cmd = AddLayerCommand::new(
                    name,
                    LayerType::Video,
                    PresetTypeId::NONE,
                    count,
                    None,
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
        LayerAction::DeleteLayerClicked(id) => {
            {
                if project.timeline.layers.len() > 1
                    && let Some((_, layer)) = project.timeline.find_layer_by_id(id)
                {
                    let layer_clone = layer.clone();
                    let cmd = DeleteLayerCommand::new(layer_clone);
                    {
                        let mut boxed: Box<dyn manifold_editing::command::Command + Send> =
                            Box::new(cmd);
                        boxed.execute(project);
                        ContentCommand::send(content_tx, ContentCommand::Execute(boxed));
                    }
                    // Fix active_layer if deleted layer was active
                    if let Some(al) = active_layer.as_ref()
                        && !project.timeline.layers.iter().any(|l| l.layer_id == *al)
                    {
                        *active_layer = project.timeline.layers.last().map(|l| l.layer_id.clone());
                    }
                }
            }
            DispatchResult::structural()
        }

    }
}
