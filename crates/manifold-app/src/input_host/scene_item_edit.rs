//! Scene tab keyboard edits: copy, paste, duplicate, delete, group, ungroup,
//! reorder, rename and frame. Each queues the same action the panel's
//! buttons send, so there is one path per edit.
use super::*;
use crate::scene_edit::SceneItemClipboard;
use manifold_ui::panels::actions::CardEditAction;
use manifold_ui::{PanelAction, ProjectAction};

impl AppInputHost<'_> {
    pub(super) fn edit_scene_items(&mut self, action: CardEditAction) -> bool {
        if !self.ui_root.object_cards_have_focus {
            return false;
        }
        if !self.ui_root.scene_setup_panel.scene_item_edit_allowed(action) {
            return true;
        }
        let Some(layer) = self.ui_root.scene_setup_panel.scene_destination() else {
            return true;
        };
        let selected = self.ui_root.scene_setup_panel.selected_scene_items();
        let queued = match action {
            CardEditAction::Copy | CardEditAction::Cut => {
                if selected.is_empty() {
                    return true;
                }
                self.ui_root.scene_item_clipboard = Some(SceneItemClipboard { layer, items: selected });
                self.ui_root.effect_clipboard.clear();
                if matches!(action, CardEditAction::Cut) {
                    self.ui_root.scene_setup_panel.remove_selection_action()
                } else {
                    None
                }
            }
            CardEditAction::Delete => self.ui_root.scene_setup_panel.remove_selection_action(),
            CardEditAction::Duplicate => self.ui_root.scene_setup_panel.duplicate_selection_action(),
            CardEditAction::Group => self.ui_root.scene_setup_panel.group_selection_action(),
            CardEditAction::Ungroup => self.ui_root.scene_setup_panel.ungroup_selection_action(),
            CardEditAction::Paste => {
                let Some(clipboard) = self.ui_root.scene_item_clipboard.clone() else {
                    return true;
                };
                if clipboard.layer != layer {
                    ContentCommand::send(
                        self.content_tx,
                        ContentCommand::GraphEditRejected("Paste into the scene the items were copied from".into()),
                    );
                    return true;
                }
                for item in clipboard.items {
                    self.ui_root.pending_keyboard_actions.push(PanelAction::Project(ProjectAction::SceneDuplicate(layer.clone(), item)));
                }
                None
            }
        };
        if let Some(action) = queued {
            self.ui_root.pending_keyboard_actions.push(action);
        }
        true
    }

    pub(super) fn scene_navigation(&mut self, delta: i32, reorder: bool) -> bool {
        if !self.ui_root.object_cards_have_focus {
            return false;
        }
        if reorder {
            if let Some(item) = self.ui_root.scene_setup_panel.selected_scene_item() {
                match crate::scene_edit::nudge(&self.project, &item.layer_id, &item.item, delta) {
                    Ok(Some(edit)) => ContentCommand::send(self.content_tx, ContentCommand::ExecuteOnContent(edit.command)),
                    Ok(None) => {}
                    Err(reason) => ContentCommand::send(self.content_tx, ContentCommand::GraphEditRejected(reason)),
                }
            }
        } else if let Some(action) = self.ui_root.scene_setup_panel.navigate_selection(delta) {
            // The panel's selection version rebuilds Properties.
            self.ui_root.pending_keyboard_actions.push(action);
        }
        true
    }

    pub(super) fn scene_rename_or_frame(&mut self, rename: bool) -> bool {
        if !self.ui_root.object_cards_have_focus {
            return false;
        }
        let action = if rename {
            self.ui_root.scene_setup_panel.rename_selection_action()
        } else {
            self.ui_root.scene_setup_panel.frame_selection_action()
        };
        if let Some(action) = action {
            self.ui_root.pending_keyboard_actions.push(action);
        }
        true
    }
}
