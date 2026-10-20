//! Outliner selection is shared by buttons, menus and keyboard gestures.
use super::*;
use manifold_foundation::scene_surface::SceneObjectCapabilities;

#[derive(Clone, Debug)]
pub struct SceneItemAddress {
    pub layer_id: LayerId,
    pub scene: u32,
    pub index: u32,
    pub is_light: bool,
    /// What the selected row's commands accept (the scene object model's
    /// capabilities): a Water look hides instead of being removed, and a
    /// Water cannot be copied.
    pub capabilities: SceneObjectCapabilities,
}

impl ScenePanel {
    /// Shared guard for scene-item keyboard and context-menu editing.
    pub fn scene_item_edit_allowed(
        &self,
        action: crate::panels::actions::CardEditAction,
    ) -> bool {
        let Some(item) = self.selected_scene_item() else {
            return true;
        };
        match action {
            // Cut copies first.
            crate::panels::actions::CardEditAction::Copy
            | crate::panels::actions::CardEditAction::Cut
            | crate::panels::actions::CardEditAction::Duplicate => item.capabilities.duplicate,
            _ => true,
        }
    }

    pub fn scene_destination(&self) -> Option<(LayerId, u32)> {
        let vm = self.state.as_live()?;
        Some((vm.layer_id.clone(), vm.scene_root_node_id))
    }

    pub fn selected_scene_item(&self) -> Option<SceneItemAddress> {
        let vm = self.state.as_live()?;
        let selection = self
            .selection
            .get(&vm.layer_id)
            .cloned()
            .unwrap_or_else(|| Self::default_selection(vm));
        let (index, is_light, capabilities) = match selection {
            SceneSelection::Object(id) => {
                let row = vm.objects.iter().find_map(|o| match o {
                    ObjectRowVm::Known(row) if row.object_node_id == id => Some(row),
                    _ => None,
                })?;
                (row.index, false, row.capabilities)
            }
            SceneSelection::Light(id) => (
                vm.lights.iter().find_map(|l| match l {
                    LightRowVm::Known(row) if row.node_doc_id == id => Some(row.index),
                    _ => None,
                })?,
                true,
                SceneObjectCapabilities { rename: true, delete: true, duplicate: true, ..SceneObjectCapabilities::default() },
            ),
            _ => return None,
        };
        Some(SceneItemAddress {
            layer_id: vm.layer_id.clone(),
            scene: vm.scene_root_node_id,
            index: index as u32,
            is_light,
            capabilities,
        })
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
            .filter(|selection| !matches!(selection, SceneSelection::OutlinerFold(_)))
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
        let vm = self.state.as_live()?;
        let item = self.selected_scene_item()?;
        if item.is_light {
            let row = vm.lights.iter().find_map(|l| match l {
                LightRowVm::Known(r) if r.index == item.index as usize => Some(r),
                _ => None,
            })?;
            Some(PanelAction::Root(RootAction::SceneSetupRenameLightClicked(
                item.layer_id,
                row.node_doc_id,
                row.name.clone(),
            )))
        } else {
            let row = vm.objects.iter().find_map(|o| match o {
                ObjectRowVm::Known(r) if r.index == item.index as usize => Some(r),
                _ => None,
            })?;
            Some(PanelAction::Root(
                RootAction::SceneSetupRenameObjectClicked(
                    item.layer_id,
                    row.object_node_id,
                    row.name.clone(),
                ),
            ))
        }
    }

    pub fn remove_selection_action(&self) -> Option<PanelAction> {
        if let Some(vm) = self.state.as_live()
            && let Some(SceneSelection::Source(node)) = self.selection.get(&vm.layer_id)
        {
            return Some(PanelAction::Project(ProjectAction::SceneSetupRemoveSource(vm.layer_id.clone(), node.clone())));
        }
        let item = self.selected_scene_item()?;
        if !item.capabilities.delete {
            return self.hide_selection_action();
        }
        Some(PanelAction::Project(if item.is_light {
            ProjectAction::SceneSetupRemoveLight(item.layer_id, item.scene, item.index)
        } else {
            ProjectAction::SceneSetupRemoveObject(item.layer_id, item.scene, item.index)
        }))
    }

    /// Build the permanent Water-family child Hide action. This shares the
    /// exact visible-row address used by the outliner eye, but always writes
    /// zero so repeated Hide invocations remain valid and idempotent.
    pub fn hide_selection_action(&self) -> Option<PanelAction> {
        let vm = self.state.as_live()?;
        let item = self.selected_scene_item()?;
        if !item.capabilities.hide {
            return None;
        }
        let object_node_id = match self.selection.get(&vm.layer_id)? {
            SceneSelection::Object(id) => *id,
            _ => return None,
        };
        let row = vm.objects.iter().find_map(|object| match object {
            ObjectRowVm::Known(row)
                if row.object_node_id == object_node_id && row.capabilities.hide => Some(row),
            _ => None,
        })?;
        if row.visible.driven {
            return None;
        }
        Some(PanelAction::Project(ProjectAction::SceneSetupParamChanged(
            vm.layer_id.clone(),
            row.visible.addr.scope_path.clone(),
            row.visible.addr.node_doc_id,
            row.visible.addr.param_id.clone(),
            0.0,
        )))
    }

    pub fn frame_selection_action(&self) -> Option<PanelAction> {
        let vm = self.state.as_live()?;
        let item = self.selected_scene_item()?;
        if item.is_light {
            return None;
        }
        let object_node_id = vm.objects.iter().find_map(|object| match object {
            ObjectRowVm::Known(row) if row.index == item.index as usize => Some(row.object_node_id),
            _ => None,
        })?;
        Some(PanelAction::Project(ProjectAction::SceneSetupFrameSelected(
            item.layer_id,
            item.scene,
            object_node_id,
        )))
    }
}
