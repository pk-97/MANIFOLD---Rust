//! Outliner selection is shared by buttons, menus and keyboard gestures.
use super::*;

#[derive(Clone, Debug)]
pub struct SceneItemAddress {
    pub layer_id: LayerId,
    pub scene: u32,
    pub index: u32,
    pub is_light: bool,
    /// Water-family children own only a look mesh and must not be removed or
    /// duplicated independently of their simulation parent.
    pub is_family_child: bool,
    /// A Water-family parent owns the simulation; duplicating it would clone
    /// the complete simulation. Removing it remains supported.
    pub is_family_parent: bool,
}

impl ScenePanel {
    /// Shared guard for scene-item keyboard and context-menu editing. Water
    /// children are material/look rows, while their parent owns the complete
    /// simulation and may still be removed as one group.
    pub fn scene_item_edit_allowed(
        &self,
        action: crate::panels::actions::CardEditAction,
    ) -> bool {
        let Some(item) = self.selected_scene_item() else {
            return true;
        };
        match action {
            // Cut copies first, and a family cannot be copied.
            crate::panels::actions::CardEditAction::Copy
            | crate::panels::actions::CardEditAction::Cut
            | crate::panels::actions::CardEditAction::Duplicate => {
                !item.is_family_child && !item.is_family_parent
            }
            crate::panels::actions::CardEditAction::Delete => !item.is_family_child,
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
        let (index, is_light, is_family_child, is_family_parent) = match selection {
            SceneSelection::Object(id) => {
                let row = vm.objects.iter().find_map(|o| match o {
                    ObjectRowVm::Known(row) if row.object_node_id == id => Some(row),
                    _ => None,
                })?;
                (
                    row.index,
                    false,
                    row.look_mesh.is_some(),
                    self.is_family_parent(row),
                )
            }
            SceneSelection::Light(id) => (
                vm.lights.iter().find_map(|l| match l {
                    LightRowVm::Known(row) if row.node_doc_id == id => Some(row.index),
                    _ => None,
                })?,
                true,
                false,
                false,
            ),
            _ => return None,
        };
        Some(SceneItemAddress {
            layer_id: vm.layer_id.clone(),
            scene: vm.scene_root_node_id,
            index: index as u32,
            is_light,
            is_family_child,
            is_family_parent,
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
        let item = self.selected_scene_item()?;
        if !self.scene_item_edit_allowed(crate::panels::actions::CardEditAction::Delete) {
            return None;
        }
        Some(PanelAction::Project(if item.is_light {
            ProjectAction::SceneSetupRemoveLight(item.layer_id, item.scene, item.index)
        } else {
            ProjectAction::SceneSetupRemoveObject(item.layer_id, item.scene, item.index)
        }))
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
