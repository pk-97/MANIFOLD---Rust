//! The Material section's two menus: the preset pick list at the top and the
//! "+ Add feature" menu under the factor rows. A preset is an action, not a
//! stored param, so the row shows the look the material's values currently
//! match (decided app-side in the projection) or "Custom". Each menu item
//! carries the same `ProjectAction` the old buttons sent.

use super::*;
use crate::panels::dropdown::DropdownItem;
use crate::panels::param_slider_shared::TOGGLE_BTN_H;

const PRESET_BUTTON_KEY: u64 = MATERIAL_LOOK_KEY_BASE;
const ADD_BUTTON_KEY: u64 = MATERIAL_LOOK_KEY_BASE + 32;

const LOOKS: [(MaterialLook, &str); 5] = [
    (MaterialLook::Default, "Default"),
    (MaterialLook::Matte, "Matte"),
    (MaterialLook::Coated, "Coated"),
    (MaterialLook::BrushedMetal, "Brushed Metal"),
    (MaterialLook::Glass, "Glass"),
];

/// What a Material-section menu button opens.
pub(super) enum MaterialMenu {
    Presets(Option<MaterialLook>),
    /// Each addable feature with the writes that switch it on, captured from
    /// the rows drawn this pass.
    AddFeature(Vec<(crate::param_surface::MaterialFeature, Vec<MaterialParamWrite>)>),
}

impl ScenePanel {
    pub(super) fn build_material_preset_row(
        &mut self,
        tree: &mut UITree,
        inner_x: f32,
        inner_w: f32,
        cy: f32,
        info: &MaterialInspectorInfo,
    ) {
        // The same field the param-row pick lists use, so the two line up.
        let RowGeometry { label_width, slider_w } = crate::panels::param_card::row_geometry(inner_w, false);
        tree.add_label(Some(self.content_parent), inner_x, cy, label_width, ROW_H, "Preset", label_style());
        let start = label_width.min(slider_w);
        let current = LOOKS.iter().find(|(look, _)| Some(*look) == info.look).map_or("Custom", |(_, name)| name);
        let id = tree.add_button_keyed(
            Some(self.content_parent),
            inner_x + start,
            cy + (ROW_H - TOGGLE_BTN_H) * 0.5,
            slider_w - start,
            TOGGLE_BTN_H,
            crate::chrome::components::dropdown_trigger_style(btn_style().font_size),
            current,
            PRESET_BUTTON_KEY,
        );
        tree.set_name(id, "material.preset");
        self.material_menus.push((id, MaterialMenu::Presets(info.look)));
    }

    pub(super) fn build_material_add_menu(
        &mut self,
        tree: &mut UITree,
        inner_x: f32,
        inner_w: f32,
        cy: f32,
    ) -> f32 {
        let feature_rows = self.selected_material_rows();
        let mut addable = Vec::new();
        for &feature in crate::param_surface::MATERIAL_FEATURES {
            if self.material_feature_visible(&feature_rows, feature) {
                continue;
            }
            let Some(mode_row) = feature_rows
                .iter()
                .find(|row| row.spec.material_role == Some(MaterialParamRole::FeatureMode(feature)))
            else {
                continue;
            };
            // A feature that lands opens unfolded.
            self.section_folded.insert(Self::material_feature_label(feature).into(), false);
            addable.push((feature, self.material_feature_writes(&feature_rows, feature, &mode_row.id)));
        }
        if addable.is_empty() {
            return cy;
        }
        let id = tree.add_button_keyed(
            Some(self.content_parent), inner_x, cy, inner_w, ROW_H, btn_style(), "+ Add feature", ADD_BUTTON_KEY,
        );
        tree.set_name(id, "material.add_feature");
        self.material_menus.push((id, MaterialMenu::AddFeature(addable)));
        cy + ROW_H + ROW_GAP
    }

    /// The app dispatched an add-feature menu item: that feature moves to the
    /// end of the UI-local section order. An undone remove is not an add, so it
    /// keeps its old slot. UI state only.
    pub fn note_material_feature_added(&mut self, writes: &[MaterialParamWrite]) {
        let added = self.material_menus.iter().find_map(|(_, menu)| match menu {
            MaterialMenu::AddFeature(features) => {
                features.iter().find(|(_, offered)| offered == writes).map(|(feature, _)| *feature)
            }
            MaterialMenu::Presets(_) => None,
        });
        if let Some(feature) = added {
            self.material_feature_order.retain(|existing| *existing != feature);
            self.material_feature_order.push(feature);
        }
    }

    /// A click on a Material menu button: ask the app to open the dropdown.
    /// `None` when the node is not one of them.
    pub(super) fn material_menu_click(
        &self,
        node: NodeId,
        target: GraphParamTarget,
    ) -> Option<Vec<PanelAction>> {
        let at = self.material_menus.iter().position(|(id, _)| *id == node)?;
        let info = self.active_material_info.clone()?;
        if !self.material_action_context(&info.object, &info.material, false) {
            return Some(Vec::new());
        }
        let items = match &self.material_menus[at].1 {
            MaterialMenu::Presets(current) => {
                if info.shared_object_count.is_none() {
                    return Some(Vec::new());
                }
                LOOKS
                    .iter()
                    .map(|&(look, name)| {
                        DropdownItem::new(name).with_check(Some(look) == *current).with_action(PanelAction::Project(
                            ProjectAction::MaterialLookApply {
                                target: target.clone(),
                                object: info.object.clone(),
                                material: info.material.clone(),
                                look,
                            },
                        ))
                    })
                    .collect()
            }
            MaterialMenu::AddFeature(features) => {
                let mut items = Vec::with_capacity(features.len());
                for (feature, writes) in features.iter().cloned() {
                    let label = Self::material_feature_label(feature);
                    items.push(DropdownItem::new(label).with_action(PanelAction::Project(
                        ProjectAction::MaterialParamsSet {
                            target: target.clone(),
                            object: info.object.clone(),
                            material: info.material.clone(),
                            kind: MaterialEditKind::Feature,
                            writes,
                            description: format!("Add {label} feature"),
                        },
                    )));
                }
                items
            }
        };
        Some(vec![PanelAction::Root(RootAction::SceneSetupActionMenuClicked { button_node_id: node, items })])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn panel_with_menus(look: Option<MaterialLook>) -> (ScenePanel, NodeId, NodeId) {
        let reference = |name: &str| ModifierObjectRef { scope: Vec::new(), node: manifold_foundation::NodeId::new(name) };
        let mut panel = ScenePanel::new();
        panel.active_material_info = Some(MaterialInspectorInfo {
            object: reference("object"),
            object_gain: None,
            material: reference("material"),
            shared_object_count: Some(1),
            look,
            textures: Vec::new(),
            params: Vec::new(),
        });
        let mut tree = UITree::new();
        let presets = tree.add_label(None, 0.0, 0.0, 10.0, 10.0, "p", label_style());
        let add = tree.add_label(None, 0.0, 0.0, 10.0, 10.0, "a", label_style());
        panel.material_menus.push((presets, MaterialMenu::Presets(look)));
        panel.material_menus.push((
            add,
            MaterialMenu::AddFeature(vec![(
                crate::param_surface::MaterialFeature::Glass,
                vec![MaterialParamWrite { param_id: "51_transmission".into(), value: 1.0 }],
            )]),
        ));
        (panel, presets, add)
    }

    fn menu_items(panel: &ScenePanel, node: NodeId) -> Vec<DropdownItem> {
        let target = GraphParamTarget::GeneratorOf(LayerId::new("layer"));
        match panel.material_menu_click(node, target).expect("a menu button").as_slice() {
            [PanelAction::Root(RootAction::SceneSetupActionMenuClicked { button_node_id, items })] => {
                assert_eq!(*button_node_id, node);
                items.clone()
            }
            other => panic!("expected one menu action, got {other:?}"),
        }
    }

    #[test]
    fn preset_menu_lists_every_look_and_checks_the_current_one() {
        let (panel, presets, _) = panel_with_menus(Some(MaterialLook::Coated));
        let items = menu_items(&panel, presets);
        let labels: Vec<_> = items.iter().map(|item| item.label.as_str()).collect();
        assert_eq!(labels, ["Default", "Matte", "Coated", "Brushed Metal", "Glass"]);
        let checked: Vec<_> = items.iter().filter(|item| item.checked).map(|item| item.label.as_str()).collect();
        assert_eq!(checked, ["Coated"]);
        assert!(matches!(
            items[2].action,
            Some(PanelAction::Project(ProjectAction::MaterialLookApply { look: MaterialLook::Coated, .. }))
        ));
    }

    #[test]
    fn custom_material_checks_no_preset() {
        let (panel, presets, _) = panel_with_menus(None);
        assert!(menu_items(&panel, presets).iter().all(|item| !item.checked));
    }

    #[test]
    fn add_menu_item_sends_the_feature_batch() {
        let (panel, _, add) = panel_with_menus(None);
        let items = menu_items(&panel, add);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].label, "Glass");
        match &items[0].action {
            Some(PanelAction::Project(ProjectAction::MaterialParamsSet { kind, writes, description, .. })) => {
                assert_eq!(*kind, MaterialEditKind::Feature);
                assert_eq!(writes.len(), 1);
                assert_eq!(description, "Add Glass feature");
            }
            other => panic!("expected the feature batch, got {other:?}"),
        }
    }

    #[test]
    fn a_stale_material_context_opens_nothing() {
        let (mut panel, presets, _) = panel_with_menus(None);
        panel.active_material_info = None;
        assert!(panel.material_menu_click(presets, GraphParamTarget::GeneratorOf(LayerId::new("layer"))).is_none());
    }
}
