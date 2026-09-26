//! Object cards share edit gestures with the inspector; the selected owner
//! supplies the address and the content thread performs every project edit.
use super::*;
use crate::object_modifier_transfer::{ObjectModifierAction, ObjectModifierClipboard};
use manifold_ui::panels::actions::CardEditAction;

impl AppInputHost<'_> {
    pub(super) fn edit_object_cards(&mut self, action: CardEditAction) -> bool {
        if !self.ui_root.object_cards_have_focus { return false; }
        let selected = self.ui_root.scene_setup_panel.selected_object_modifier();
        if matches!(action, CardEditAction::Copy | CardEditAction::Cut) {
            let Some(address) = selected.as_ref() else { return true; };
            match ObjectModifierClipboard::capture(
                self.project, &address.layer_id,
                address.group_node_id.unwrap_or(address.object_id), address.node_doc_id,
            ) {
                Ok(clipboard) => {
                    self.ui_root.object_modifier_clipboard = Some(clipboard);
                    self.ui_root.effect_clipboard.clear();
                    self.ui_root.scene_modifier_clipboard = None;
                }
                Err(reason) => {
                    ContentCommand::send(self.content_tx, ContentCommand::GraphEditRejected(reason));
                    return true;
                }
            }
        }
        match action {
            CardEditAction::Copy | CardEditAction::Group | CardEditAction::Ungroup => return true,
            CardEditAction::Cut | CardEditAction::Delete => {
                let Some(address) = selected else { return true; };
                let target = manifold_core::GraphTarget::Generator(address.layer_id);
                let Some(default) = crate::graph_target::owner_default(self.project, &target) else {
                    return true;
                };
                ContentCommand::send(self.content_tx, ContentCommand::ExecuteOnContent(Box::new(
                    manifold_editing::commands::graph::RemoveMeshModifierCommand::new(
                        target, Vec::new(), address.group_node_id.unwrap_or(address.object_id),
                        address.node_doc_id, default,
                    ),
                )));
            }
            CardEditAction::Duplicate => {
                let Some(address) = selected else { return true; };
                ContentCommand::send(self.content_tx, ContentCommand::ObjectModifier(
                    ObjectModifierAction::Duplicate {
                        layer_id: address.layer_id,
                        owner_id: address.group_node_id.unwrap_or(address.object_id),
                        node_doc_id: address.node_doc_id,
                    },
                ));
            }
            CardEditAction::Paste => {
                let Some(clipboard) = self.ui_root.object_modifier_clipboard.clone() else { return true; };
                let Some((layer_id, owner_id)) = self.ui_root.scene_setup_panel.object_modifier_destination() else {
                    return true;
                };
                ContentCommand::send(self.content_tx, ContentCommand::ObjectModifier(
                    ObjectModifierAction::Paste {
                        layer_id, owner_id, after: selected.map(|address| address.node_doc_id), clipboard,
                    },
                ));
            }
        }
        *self.needs_structural_sync = true;
        *self.needs_rebuild = true;
        true
    }
}
