//! The Scene tab's click-to-open menus (fluid roles, add modifier, skin
//! source and target map, the Material section's action menus). Moved verbatim
//! out of `dropdowns.rs`, which stays under the file-size ceiling; the arms are
//! the same, reached through the `_ =>` fallthrough of `try_open_dropdown_inner`.

use manifold_ui::{ProjectAction, RootAction};
use super::*;

impl UIRoot {
    pub(super) fn try_open_scene_setup_menu(&mut self, action: &PanelAction) -> bool {
        match action {
            // SCENE_PANEL_UX_DESIGN.md UX-P2, D6: the "+ Add Modifier"
            // button opens the shared dropdown listing the SAME curated
            // vocabulary the old 7-chip grid did — each item dispatches the
            // SAME `SceneSetupAddModifier` action the chips fired directly.
            // `button_node_id` resolves the anchor directly, same
            // resolve-at-open convention as `SceneSetupEnumClicked` above.
            PanelAction::Root(RootAction::SceneSetupFluidRoleClicked {
                layer_id, object, domains, button_node_id,
            }) => {
                let anchor = self.tree.get_bounds(*button_node_id);
                let mut items = Vec::new();
                if domains.is_empty() {
                    items.push(DropdownItem::disabled("Add a Fluid to this scene first"));
                } else {
                    if domains.len() == 1 {
                        items.push(DropdownItem::disabled(&domains[0].name));
                    }
                    for (role, label) in [(0, "Initial Fill"), (1, "Inflow"), (2, "Drain"), (3, "Collider")] {
                        let action = if domains.len() == 1 {
                            PanelAction::Project(ProjectAction::SceneSetupAssignFluidRole {
                                layer_id: layer_id.clone(), object: object.clone(),
                                 domain: domains[0].node.clone(), role,
                            })
                        } else {
                            PanelAction::Root(RootAction::SceneSetupFluidDomainClicked {
                                layer_id: layer_id.clone(), object: object.clone(),
                                 role, domains: domains.clone(), anchor,
                            })
                        };
                        items.push(DropdownItem::new(label).with_action(action));
                    }
                }
                self.open_dropdown_typed(items, anchor);
                true
            }
            PanelAction::Root(RootAction::SceneSetupFluidRoleTargetClicked {
                layer_id, source, domains, button_node_id,
            }) => {
                let mut items = vec![DropdownItem::disabled("Replace fluid targets")];
                items.extend(domains.iter().map(|domain| DropdownItem::new(&domain.name)
                    .with_action(PanelAction::Project(ProjectAction::SceneSetupRetargetFluidRole {
                        layer_id: layer_id.clone(), source: source.clone(),
                        domain: domain.node.clone(),
                    }))));
                if domains.is_empty() { items.push(DropdownItem::disabled("Add a Fluid to this scene first")); }
                self.open_dropdown_typed(items, self.tree.get_bounds(*button_node_id));
                true
            }
            PanelAction::Root(RootAction::SceneSetupFluidDomainClicked {
                layer_id, object, role, domains, anchor,
            }) => {
                let items = domains.iter().map(|domain| {
                    DropdownItem::new(&domain.name).with_action(PanelAction::Project(
                        ProjectAction::SceneSetupAssignFluidRole {
                            layer_id: layer_id.clone(), object: object.clone(),
                             domain: domain.node.clone(), role: *role,
                        },
                    ))
                }).collect();
                self.open_dropdown_typed(items, *anchor);
                true
            }
            PanelAction::Root(RootAction::SceneSetupAddModifierClicked(layer_id, group_node_id, button_node_id)) => {
                let trigger = self.tree.get_bounds(*button_node_id);
                let items: Vec<DropdownItem> = manifold_ui::panels::scene_setup_panel::MESH_MODIFIER_CHOICES
                    .iter()
                    .map(|(label, type_id)| {
                        // These nodes require a second authored source. A card
                        // picker cannot create a meaningful connection for it.
                        match *type_id {
                            "node.push_mesh" => return DropdownItem::disabled("Displace by Texture — connect a texture in Graph"),
                            "node.morph_mesh" => return DropdownItem::disabled("Morph — connect a target mesh in Graph"),
                            _ => {}
                        }
                        DropdownItem::new(label).with_action(PanelAction::Project(ProjectAction::SceneSetupAddModifier(
                            layer_id.clone(),
                            *group_node_id,
                            (*type_id).to_string(),
                        )))
                    })
                    .collect();
                self.open_dropdown_typed(items, trigger);
                true
            }
            // SCENE_FX P4b: the Skin row's source dropdown — "None" + every
            // project layer, options carried on the action (captured from the
            // row's freshly synced `SkinRowVm`, so no project cache here).
            // Each item dispatches `SceneSetupSkinSourceSet` — the only
            // mutation path for the source binding. Handled at THIS
            // pre-dispatch intercept (not app_render) so the headless script
            // harness drives the identical dropdown.
            PanelAction::Root(RootAction::SceneSetupSkinSourceClicked {
                layer_id,
                scope_path,
                scene_object_id,
                source_node_id,
                target_map,
                source_options,
                button_node_id,
            }) => {
                let trigger = self.tree.get_bounds(*button_node_id);
                let items: Vec<DropdownItem> = std::iter::once((None, "None".to_string()))
                    .chain(source_options.iter().cloned().map(|(id, name)| (Some(id), name)))
                    .map(|(source, label)| {
                        DropdownItem::new(&label).with_action(PanelAction::Project(
                            ProjectAction::SceneSetupSkinSourceSet {
                                layer_id: layer_id.clone(),
                                scope_path: scope_path.clone(),
                                scene_object_id: *scene_object_id,
                                source_node_id: *source_node_id,
                                target_map: *target_map,
                                source,
                            },
                        ))
                    })
                    .collect();
                self.open_dropdown_typed(items, trigger);
                true
            }
            // SCENE_FX P4b: the Skin row's target-map dropdown — Emissive /
            // Base Color, checked at the current map. Items carry the UI enum;
            // `dispatch_project` translates to the editing command's own
            // `SkinTargetMap`.
            PanelAction::Root(RootAction::SceneSetupSkinTargetMapClicked {
                layer_id,
                scope_path,
                scene_object_id,
                source_node_id,
                current_target_map,
                button_node_id,
            }) => {
                let trigger = self.tree.get_bounds(*button_node_id);
                let choices = [
                    manifold_ui::panels::scene_setup_panel::SkinTargetMap::Emissive,
                    manifold_ui::panels::scene_setup_panel::SkinTargetMap::BaseColor,
                ];
                let items: Vec<DropdownItem> = choices
                    .iter()
                    .map(|&map| {
                        DropdownItem::new(map.label())
                            .with_check(map == *current_target_map)
                            .with_action(PanelAction::Project(ProjectAction::SceneSetupSkinTargetMapSet {
                                layer_id: layer_id.clone(),
                                scope_path: scope_path.clone(),
                                scene_object_id: *scene_object_id,
                                source_node_id: *source_node_id,
                                target_map: map,
                            }))
                    })
                    .collect();
                self.open_dropdown_typed(items, trigger);
                true
            }
            PanelAction::Root(RootAction::SceneSetupActionMenuClicked { button_node_id, items }) => {
                let trigger = self.tree.get_bounds(*button_node_id);
                self.open_dropdown_typed(items.clone(), trigger);
                true
            }
            _ => false,
        }
    }
}
