//! Scene tab keyboard edits: copy, paste, duplicate, delete, group, ungroup,
//! reorder, rename and frame. Each queues the same action the panel's
//! buttons send, so there is one path per edit.
use super::*;
use manifold_ui::panels::actions::CardEditAction;

impl AppInputHost<'_> {
    pub(super) fn edit_scene_items(&mut self, action: CardEditAction) -> bool {
        if !self.ui_root.object_cards_have_focus {
            return false;
        }
        if !self
            .ui_root
            .scene_setup_panel
            .scene_item_edit_allowed(action)
        {
            return true;
        }
        let Some(layer) = self.ui_root.scene_setup_panel.scene_destination() else {
            return true;
        };
        let selected = self.ui_root.scene_setup_panel.selected_scene_items();
        let queued = match action {
            CardEditAction::Copy | CardEditAction::Cut => {
                let Some(clipboard) =
                    crate::scene_clipboard::copy(&self.project, &layer, &selected)
                else {
                    return true;
                };
                self.ui_root.scene_item_clipboard = Some(clipboard);
                self.ui_root.effect_clipboard.clear();
                if matches!(action, CardEditAction::Cut) {
                    self.ui_root.scene_setup_panel.remove_selection_action()
                } else {
                    None
                }
            }
            CardEditAction::Delete => self.ui_root.scene_setup_panel.remove_selection_action(),
            CardEditAction::Duplicate => {
                crate::ui_bridge::project::send_scene_edit(
                    self.content_tx,
                    &layer,
                    crate::scene_edit::duplicate(&self.project, &layer, &selected).map(Some),
                );
                None
            }
            CardEditAction::Group => self.ui_root.scene_setup_panel.group_selection_action(),
            CardEditAction::Ungroup => self.ui_root.scene_setup_panel.ungroup_selection_action(),
            CardEditAction::Paste => {
                let Some(clipboard) = self.ui_root.scene_item_clipboard.as_ref() else {
                    return true;
                };
                let built =
                    crate::scene_clipboard::paste(&self.project, &layer, clipboard).map(Some);
                crate::ui_bridge::project::send_scene_edit(self.content_tx, &layer, built);
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
                    Ok(Some(edit)) => {
                        ContentCommand::send(self.content_tx, ContentCommand::Execute(edit.command))
                    }
                    Ok(None) => {}
                    Err(reason) => ContentCommand::send(
                        self.content_tx,
                        ContentCommand::GraphEditRejected(reason),
                    ),
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
