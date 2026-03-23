//! Layer-related dispatch: mute/solo/click/chevron/blend/drag/add/delete.

use manifold_core::LayerId;
use manifold_core::project::Project;
use manifold_core::types::{BlendMode, LayerType};
use manifold_core::GeneratorTypeId;
use manifold_editing::commands::layer::{AddLayerCommand, DeleteLayerCommand};
use manifold_editing::commands::settings::ChangeLayerBlendModeCommand;
use manifold_editing::service::EditingService;
use manifold_ui::PanelAction;

use crate::app::SelectionState;
use crate::ui_root::UIRoot;
use super::DispatchResult;

pub(super) fn dispatch_layer(
    action: &PanelAction,
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
        PanelAction::ToggleMute(idx) => {
            // If the clicked layer is part of a multi-selection, apply to all selected layers
            let clicked_id = project.timeline.layers.get(*idx)
                .map(|l| l.layer_id.clone()).unwrap_or_default();
            let target_ids: Vec<LayerId> = if selection.selected_layer_ids.len() > 1
                && selection.is_layer_selected(&clicked_id)
            {
                selection.selected_layer_ids.iter().cloned().collect()
            } else {
                vec![clicked_id]
            };
            // Determine new mute state from clicked layer (toggle)
            let new_muted = project.timeline.layers.get(*idx)
                .map(|l| !l.is_muted).unwrap_or(true);
            for id in &target_ids {
                if let Some((_, layer)) = project.timeline.find_layer_by_id_mut(id) {
                    layer.is_muted = new_muted;
                }
            }
            ContentCommand::send(content_tx, ContentCommand::MutateProject(Box::new(move |p| {
                for id in &target_ids {
                    if let Some((_, layer)) = p.timeline.find_layer_by_id_mut(id) {
                        layer.is_muted = new_muted;
                    }
                }
            })));
            DispatchResult::handled()
        }
        PanelAction::ToggleSolo(idx) => {
            let clicked_id = project.timeline.layers.get(*idx)
                .map(|l| l.layer_id.clone()).unwrap_or_default();
            let target_ids: Vec<LayerId> = if selection.selected_layer_ids.len() > 1
                && selection.is_layer_selected(&clicked_id)
            {
                selection.selected_layer_ids.iter().cloned().collect()
            } else {
                vec![clicked_id]
            };
            let new_solo = project.timeline.layers.get(*idx)
                .map(|l| !l.is_solo).unwrap_or(true);
            for id in &target_ids {
                if let Some((_, layer)) = project.timeline.find_layer_by_id_mut(id) {
                    layer.is_solo = new_solo;
                }
            }
            ContentCommand::send(content_tx, ContentCommand::MutateProject(Box::new(move |p| {
                for id in &target_ids {
                    if let Some((_, layer)) = p.timeline.find_layer_by_id_mut(id) {
                        layer.is_solo = new_solo;
                    }
                }
            })));
            DispatchResult::handled()
        }
        PanelAction::LayerClicked(idx, modifiers) => {
            // From Unity UIState.cs layer selection methods (lines 247-333).
            let layer_id = project.timeline.layers.get(*idx)
                .map(|l| l.layer_id.clone())
                .unwrap_or_default();
            *active_layer = Some(layer_id.clone());

            // Clear effect selection when switching focus to layer headers
            ui.inspector.clear_effect_selection();

            {
                if modifiers.shift {
                    // Shift+Click: range select from primary to target
                    selection.select_layer_range(&layer_id, &project.timeline.layers);
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
        PanelAction::LayerDoubleClicked(_idx) => {
            // Intercepted by app.rs — opens text input for layer rename
            DispatchResult::handled()
        }
        PanelAction::ChevronClicked(idx) => {
            let clicked_id = project.timeline.layers.get(*idx)
                .map(|l| l.layer_id.clone()).unwrap_or_default();
            let target_ids: Vec<LayerId> = if selection.selected_layer_ids.len() > 1
                && selection.is_layer_selected(&clicked_id)
            {
                selection.selected_layer_ids.iter().cloned().collect()
            } else {
                vec![clicked_id]
            };
            let new_collapsed = project.timeline.layers.get(*idx)
                .map(|l| !l.is_collapsed).unwrap_or(true);
            for id in &target_ids {
                if let Some((_, layer)) = project.timeline.find_layer_by_id_mut(id) {
                    layer.is_collapsed = new_collapsed;
                }
            }
            ContentCommand::send(content_tx, ContentCommand::MutateProject(Box::new(move |p| {
                for id in &target_ids {
                    if let Some((_, layer)) = p.timeline.find_layer_by_id_mut(id) {
                        layer.is_collapsed = new_collapsed;
                    }
                }
            })));
            DispatchResult::structural()
        }
        PanelAction::BlendModeClicked(_idx) => {
            // Intercepted by UIRoot::try_open_dropdown (opens dropdown at button).
            DispatchResult::handled()
        }
        PanelAction::SetBlendMode(idx, mode_str) => {
            {
                if let Some(layer) = project.timeline.layers.get(*idx) {
                    let layer_id = layer.layer_id.clone();
                    let old_mode = layer.default_blend_mode;
                    if let Some(new_mode) = BlendMode::ALL.iter().find(|m| format!("{:?}", m) == *mode_str) {
                        let cmd = ChangeLayerBlendModeCommand::new(layer_id, old_mode, *new_mode);
                        { let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd); boxed.execute(project); ContentCommand::send(content_tx, ContentCommand::Execute(boxed)); }
                    }
                }
            }
            DispatchResult::structural()
        }
        PanelAction::ExpandLayer(idx) => {
            if let Some(layer) = project.timeline.layers.get_mut(*idx) {
                layer.is_collapsed = false;
            }
            let id = project.timeline.layers.get(*idx)
                .map(|l| l.layer_id.clone()).unwrap_or_default();
            ContentCommand::send(content_tx, ContentCommand::MutateProject(Box::new(move |p| {
                if let Some((_, layer)) = p.timeline.find_layer_by_id_mut(&id) {
                    layer.is_collapsed = false;
                }
            })));
            DispatchResult::structural()
        }
        PanelAction::CollapseLayer(idx) => {
            if let Some(layer) = project.timeline.layers.get_mut(*idx) {
                layer.is_collapsed = true;
            }
            let id = project.timeline.layers.get(*idx)
                .map(|l| l.layer_id.clone()).unwrap_or_default();
            ContentCommand::send(content_tx, ContentCommand::MutateProject(Box::new(move |p| {
                if let Some((_, layer)) = p.timeline.find_layer_by_id_mut(&id) {
                    layer.is_collapsed = true;
                }
            })));
            DispatchResult::structural()
        }
        PanelAction::FolderClicked(_idx) => {
            log::info!("Folder clicked (file picker not yet implemented)");
            DispatchResult::handled()
        }
        PanelAction::NewClipClicked(idx) => {
            let beat = content_state.current_beat;
            {
                let (cmd, _) = EditingService::create_clip_at_position(project, beat, *idx, 4.0);
                { ContentCommand::send(content_tx, ContentCommand::Execute(cmd)); }
            }
            DispatchResult::structural()
        }
        PanelAction::AddGenClipClicked(idx) => {
            let beat = content_state.current_beat;
            {
                let (cmd, _) = EditingService::create_clip_at_position(project, beat, *idx, 4.0);
                { ContentCommand::send(content_tx, ContentCommand::Execute(cmd)); }
            }
            DispatchResult::structural()
        }
        PanelAction::MidiInputClicked(_idx) => {
            // Intercepted by UIRoot::try_open_dropdown (opens dropdown at button).
            DispatchResult::handled()
        }
        PanelAction::MidiChannelClicked(_idx) => {
            // Intercepted by UIRoot::try_open_dropdown (opens dropdown at button).
            DispatchResult::handled()
        }
        PanelAction::LayerDragStarted(_) | PanelAction::LayerDragMoved(..) => {
            DispatchResult::handled()
        }
        PanelAction::LayerDragEnded(from, to) => {
            // From Unity LayerHeaderPanel.HandleDragEnd + ReorderLayerCommand.
            // Supports multi-select: if dragged layer is part of a selection,
            // all selected layers move as a group.
            if from != to {
                let dragged_id = project.timeline.layers.get(*from)
                    .map(|l| l.layer_id.clone()).unwrap_or_default();
                let is_multi = selection.selected_layer_ids.len() > 1
                    && selection.is_layer_selected(&dragged_id);

                let old_order = project.timeline.layers.clone();
                let mut new_order = old_order.clone();

                if is_multi {
                    // Multi-select: move all selected layers as a group
                    let selected_ids: Vec<LayerId> = selection.selected_layer_ids.iter().cloned().collect();
                    // Remove selected layers (preserving their relative order)
                    let mut moving: Vec<_> = new_order.iter()
                        .filter(|l| selected_ids.contains(&l.layer_id))
                        .cloned()
                        .collect();
                    new_order.retain(|l| !selected_ids.contains(&l.layer_id));

                    // Find insertion point: where the target index maps to after removals
                    let target_insert = (*to).min(new_order.len());

                    // Determine parent group at insertion point
                    let target_parent = if target_insert < new_order.len() {
                        new_order[target_insert].parent_layer_id.clone()
                    } else {
                        new_order.last().and_then(|l| l.parent_layer_id.clone())
                    };

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
                    // Single layer move
                    let layer = new_order.remove(*from);
                    let insert_at = if *to > *from { to.saturating_sub(1) } else { *to };
                    let insert_at = insert_at.min(new_order.len());

                    let target_parent = if insert_at < new_order.len() {
                        new_order[insert_at].parent_layer_id.clone()
                    } else {
                        new_order.last().and_then(|l| l.parent_layer_id.clone())
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
                        old_order, new_order, old_parents, new_parents,
                    );
                    { let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd); boxed.execute(project); ContentCommand::send(content_tx, ContentCommand::Execute(boxed)); }
                }
            }
            DispatchResult::structural()
        }

        // ── Layer management ───────────────────────────────────────
        PanelAction::AddLayerClicked => {
            {
                let count = project.timeline.layers.len();
                let name = format!("Layer {}", count + 1);
                let cmd = AddLayerCommand::new(
                    name,
                    LayerType::Video,
                    GeneratorTypeId::NONE,
                    count,
                    None,
                );
                { let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd); boxed.execute(project); ContentCommand::send(content_tx, ContentCommand::Execute(boxed)); }
            }
            DispatchResult::structural()
        }
        PanelAction::DeleteLayerClicked(idx) => {
            {
                if project.timeline.layers.len() > 1
                    && let Some(layer) = project.timeline.layers.get(*idx) {
                        let layer_clone = layer.clone();
                        let cmd = DeleteLayerCommand::new(layer_clone);
                        { let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd); boxed.execute(project); ContentCommand::send(content_tx, ContentCommand::Execute(boxed)); }
                        // Fix active_layer if deleted layer was active
                        if let Some(al) = active_layer.as_ref()
                            && !project.timeline.layers.iter().any(|l| l.layer_id == *al)
                        {
                            *active_layer = project.timeline.layers.last()
                                .map(|l| l.layer_id.clone());
                        }
                    }
            }
            DispatchResult::structural()
        }

        _ => DispatchResult::unhandled(),
    }
}
