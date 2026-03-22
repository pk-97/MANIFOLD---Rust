//! Layer-related dispatch: mute/solo/click/chevron/blend/drag/add/delete.

use manifold_core::LayerId;
use manifold_core::project::Project;
use manifold_core::types::{BlendMode, LayerType, GeneratorType};
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
    _ui: &mut UIRoot,
    selection: &mut SelectionState,
    active_layer: &mut Option<LayerId>,
) -> DispatchResult {
    use crate::content_command::ContentCommand;
    match action {
        // ── Layer operations ───────────────────────────────────────
        PanelAction::ToggleMute(idx) => {
            if let Some(layer) = project.timeline.layers.get_mut(*idx) {
                layer.is_muted = !layer.is_muted;
            }
            let id = project.timeline.layers.get(*idx)
                .map(|l| l.layer_id.clone()).unwrap_or_default();
            ContentCommand::send(content_tx, ContentCommand::MutateProject(Box::new(move |p| {
                if let Some((_, layer)) = p.timeline.find_layer_by_id_mut(&id) {
                    layer.is_muted = !layer.is_muted;
                }
            })));
            DispatchResult::handled()
        }
        PanelAction::ToggleSolo(idx) => {
            if let Some(layer) = project.timeline.layers.get_mut(*idx) {
                layer.is_solo = !layer.is_solo;
            }
            let id = project.timeline.layers.get(*idx)
                .map(|l| l.layer_id.clone()).unwrap_or_default();
            ContentCommand::send(content_tx, ContentCommand::MutateProject(Box::new(move |p| {
                if let Some((_, layer)) = p.timeline.find_layer_by_id_mut(&id) {
                    layer.is_solo = !layer.is_solo;
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
            if let Some(layer) = project.timeline.layers.get_mut(*idx) {
                layer.is_collapsed = !layer.is_collapsed;
            }
            let id = project.timeline.layers.get(*idx)
                .map(|l| l.layer_id.clone()).unwrap_or_default();
            ContentCommand::send(content_tx, ContentCommand::MutateProject(Box::new(move |p| {
                if let Some((_, layer)) = p.timeline.find_layer_by_id_mut(&id) {
                    layer.is_collapsed = !layer.is_collapsed;
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
                    let old_mode = layer.default_blend_mode;
                    if let Some(new_mode) = BlendMode::ALL.iter().find(|m| format!("{:?}", m) == *mode_str) {
                        let cmd = ChangeLayerBlendModeCommand::new(*idx, old_mode, *new_mode);
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
            // Atomically reorder layers and update parent_layer_id when moving into/out of groups.
            if from != to {
                {
                    let old_order = project.timeline.layers.clone();
                    let mut new_order = old_order.clone();

                    if *from < new_order.len() && *to <= new_order.len() {
                        let layer = new_order.remove(*from);
                        let insert_at = if *to > *from { to.saturating_sub(1) } else { *to };
                        let insert_at = insert_at.min(new_order.len());

                        // Determine parent group for the target position
                        let target_parent = if insert_at < new_order.len() {
                            new_order[insert_at].parent_layer_id.clone()
                        } else if !new_order.is_empty() {
                            new_order.last().and_then(|l| l.parent_layer_id.clone())
                        } else {
                            None
                        };

                        new_order.insert(insert_at, layer);

                        // Build parent ID maps for undo
                        let mut old_parents = std::collections::HashMap::new();
                        let mut new_parents = std::collections::HashMap::new();
                        for l in old_order.iter() {
                            old_parents.insert(l.layer_id.clone(), l.parent_layer_id.clone());
                        }
                        // Update moved layer's parent
                        for l in &new_order {
                            new_parents.insert(l.layer_id.clone(), l.parent_layer_id.clone());
                        }
                        let moved_id = new_order[insert_at].layer_id.clone();
                        new_parents.insert(moved_id, target_parent);

                        let cmd = manifold_editing::commands::layer::ReorderLayerCommand::new(
                            old_order, new_order, old_parents, new_parents,
                        );
                        { let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd); boxed.execute(project); ContentCommand::send(content_tx, ContentCommand::Execute(boxed)); }
                    }
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
                    GeneratorType::None,
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
                        let cmd = DeleteLayerCommand::new(layer_clone, *idx);
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
