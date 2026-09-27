//! Scene edits are content-owned; copying captures an immutable snapshot.
use super::*;
use crate::scene_item_transfer::{SceneItemAction, SceneItemClipboard, SceneItemKind};
use manifold_ui::panels::actions::CardEditAction;

impl AppInputHost<'_> {
    pub(super) fn edit_scene_items(&mut self, action: CardEditAction) -> bool {
        if !self.ui_root.object_cards_have_focus {
            return false;
        }
        let selected = self.ui_root.scene_setup_panel.selected_scene_item();
        if matches!(action, CardEditAction::Copy | CardEditAction::Cut) {
            let Some(item) = selected.as_ref() else {
                return true;
            };
            let kind = if item.is_light {
                SceneItemKind::Light
            } else {
                SceneItemKind::Object
            };
            match SceneItemClipboard::capture(
                self.project,
                &item.layer_id,
                item.scene,
                kind,
                item.index,
            ) {
                Ok(clipboard) => {
                    self.ui_root.scene_item_clipboard = Some(clipboard);
                    self.ui_root.object_modifier_clipboard = None;
                    self.ui_root.scene_modifier_clipboard = None;
                    self.ui_root.effect_clipboard.clear();
                }
                Err(reason) => {
                    ContentCommand::send(
                        self.content_tx,
                        ContentCommand::GraphEditRejected(reason),
                    );
                    return true;
                }
            }
        }
        match action {
            CardEditAction::Copy | CardEditAction::Group | CardEditAction::Ungroup => return true,
            CardEditAction::Cut | CardEditAction::Delete => {
                if let Some(action) = self.ui_root.scene_setup_panel.remove_selection_action() {
                    self.ui_root.pending_keyboard_actions.push(action);
                }
            }
            CardEditAction::Duplicate => {
                let Some(item) = selected else {
                    return true;
                };
                ContentCommand::send(
                    self.content_tx,
                    ContentCommand::SceneItem(SceneItemAction::Duplicate {
                        layer: item.layer_id,
                        scene: item.scene,
                        kind: if item.is_light {
                            SceneItemKind::Light
                        } else {
                            SceneItemKind::Object
                        },
                        index: item.index,
                    }),
                );
            }
            CardEditAction::Paste => {
                let Some(clipboard) = self.ui_root.scene_item_clipboard.clone() else {
                    return true;
                };
                let Some((layer, scene)) = self.ui_root.scene_setup_panel.scene_destination()
                else {
                    return true;
                };
                ContentCommand::send(
                    self.content_tx,
                    ContentCommand::SceneItem(SceneItemAction::Paste {
                        layer,
                        scene,
                        clipboard: Box::new(clipboard),
                    }),
                );
            }
        }
        *self.needs_structural_sync = true;
        *self.needs_rebuild = true;
        true
    }

    pub(super) fn scene_navigation(&mut self, delta: i32, reorder: bool) -> bool {
        if !self.ui_root.object_cards_have_focus
            || self
                .ui_root
                .scene_setup_panel
                .selected_object_modifier()
                .is_some()
        {
            return false;
        }
        if reorder {
            if let Some(item) = self.ui_root.scene_setup_panel.selected_scene_item() {
                ContentCommand::send(
                    self.content_tx,
                    ContentCommand::SceneItem(SceneItemAction::Move {
                        layer: item.layer_id,
                        scene: item.scene,
                        kind: if item.is_light {
                            SceneItemKind::Light
                        } else {
                            SceneItemKind::Object
                        },
                        index: item.index,
                        delta,
                    }),
                );
            }
        } else if let Some(action) = self.ui_root.scene_setup_panel.navigate_selection(delta) {
            self.ui_root.pending_keyboard_actions.push(action);
        }
        *self.needs_structural_sync = true;
        *self.needs_rebuild = true;
        true
    }

    pub(super) fn scene_rename_or_frame(&mut self, rename: bool) -> bool {
        if !self.ui_root.object_cards_have_focus
            || self
                .ui_root
                .scene_setup_panel
                .selected_object_modifier()
                .is_some()
        {
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
        *self.needs_rebuild = true;
        true
    }
}
