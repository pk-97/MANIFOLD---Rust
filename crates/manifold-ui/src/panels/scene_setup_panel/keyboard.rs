//! Outliner selection is shared by buttons, menus and keyboard gestures.
use super::*;

#[derive(Clone, Debug)]
pub struct SceneItemAddress {
    pub layer_id: LayerId,
    /// The selected item, by id.
    pub item: ItemId,
    pub kind: ItemKind,
    /// Whether the item can be removed and duplicated.
    pub removable: bool,
}

impl ScenePanel {
    /// Shared guard for scene-item keyboard and context-menu editing.
    pub fn scene_item_edit_allowed(&self, action: crate::panels::actions::CardEditAction) -> bool {
        let Some(item) = self.selected_scene_item() else {
            return true;
        };
        match action {
            // Cut copies first.
            crate::panels::actions::CardEditAction::Copy
            | crate::panels::actions::CardEditAction::Cut
            | crate::panels::actions::CardEditAction::Duplicate
            | crate::panels::actions::CardEditAction::Delete => item.removable,
            _ => true,
        }
    }

    /// The layer a paste into the Scene tab goes to.
    pub fn scene_destination(&self) -> Option<LayerId> {
        Some(self.state.as_live()?.layer_id.clone())
    }

    /// The selected item (a light shows as Lighting, and has no single item).
    pub fn selected_scene_item(&self) -> Option<SceneItemAddress> {
        let vm = self.state.as_live()?;
        let selection = self
            .selection
            .get(&vm.layer_id)
            .cloned()
            .unwrap_or_else(|| Self::default_selection(vm));
        let item = vm.item(selection.item()?)?;
        Some(SceneItemAddress {
            layer_id: vm.layer_id.clone(),
            item: item.id.clone(),
            kind: item.kind,
            removable: item.removable,
        })
    }

    /// Every selected item, in selection order.
    pub fn selected_scene_items(&self) -> Vec<ItemId> {
        self.live_selected()
            .iter()
            .filter_map(|selection| selection.item().cloned())
            .collect()
    }

    pub fn navigate_selection(&mut self, delta: i32) -> Option<PanelAction> {
        let vm = self.state.as_live()?;
        let current = self
            .selection
            .get(&vm.layer_id)
            .cloned()
            .unwrap_or_else(|| Self::default_selection(vm));
        let visible: Vec<_> = self
            .outliner_row_ids
            .iter()
            .map(|(_, selection)| selection.clone())
            .collect();
        if visible.is_empty() {
            return None;
        }
        let index = visible
            .iter()
            .position(|selection| *selection == current)
            .unwrap_or(0);
        let next = (index as i32 + delta).clamp(0, visible.len() as i32 - 1) as usize;
        let layer = vm.layer_id.clone();
        self.set_selection(layer.clone(), visible[next].clone());
        Some(PanelAction::Root(RootAction::SceneSetupSelectionChanged(
            layer,
        )))
    }

    pub fn rename_selection_action(&self) -> Option<PanelAction> {
        let item = self.selected_scene_item()?;
        let name = self.state.as_live()?.item(&item.item)?.name.clone();
        Some(PanelAction::Root(RootAction::SceneRenameClicked(
            item.layer_id,
            item.item,
            name,
        )))
    }

    /// Delete or Backspace: remove every selected item, one undo step.
    pub fn remove_selection_action(&self) -> Option<PanelAction> {
        let vm = self.state.as_live()?;
        let items: Vec<ItemId> = self
            .selected_scene_items()
            .into_iter()
            .filter(|id| vm.item(id).is_some_and(|item| item.removable))
            .collect();
        (!items.is_empty())
            .then(|| PanelAction::Project(ProjectAction::SceneRemove(vm.layer_id.clone(), items)))
    }

    /// Cmd+G: wrap the selected items in a new Group.
    pub fn group_selection_action(&self) -> Option<PanelAction> {
        let vm = self.state.as_live()?;
        let items: Vec<ItemId> = self
            .selected_scene_items()
            .into_iter()
            .filter(|id| vm.item(id).is_some_and(|item| item.removable))
            .collect();
        (!items.is_empty())
            .then(|| PanelAction::Project(ProjectAction::SceneGroup(vm.layer_id.clone(), items)))
    }

    /// Cmd+Shift+G: dissolve the selected Group.
    pub fn ungroup_selection_action(&self) -> Option<PanelAction> {
        let item = self
            .selected_scene_item()
            .filter(|item| item.kind == ItemKind::Group)?;
        Some(PanelAction::Project(ProjectAction::SceneUngroup(
            item.layer_id,
            item.item,
        )))
    }

    pub fn frame_selection_action(&self) -> Option<PanelAction> {
        let item = self
            .selected_scene_item()
            .filter(|item| super::item_header::can_frame(item.kind))?;
        Some(PanelAction::Project(
            ProjectAction::SceneSetupFrameSelected(item.layer_id, item.item),
        ))
    }
}
