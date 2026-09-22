//! Shared camera controls stay editable when a scene modifier owns movement.

use super::*;

impl ScenePanel {
    /// P2 slice 2a: replaced the per-family Orbit/Free/LookAt row lists
    /// (plus the separate Lens sub-section) with one `build_filtered_
    /// properties` pass over `vm.camera_sections` (the camera atom's REAL
    /// P1 section, plus the lens's if wired — see
    /// `SceneSetupVm::camera_sections`'s doc comment). Custom/None fallback
    /// messaging (no camera vocabulary matched, or the port is unwired)
    /// stays panel-shaped, unchanged.
    pub(super) fn build_camera_section(&mut self, tree: &mut UITree, inner_x: f32, inner_w: f32, mut cy: f32, vm: &SceneSetupVm) -> f32 {
        match &vm.camera {
            CameraRowVm::Orbit(_) | CameraRowVm::Free(_) | CameraRowVm::LookAt(_) => {
                self.build_filtered_properties(tree, inner_x, inner_w, cy, &vm.camera_sections)
            }
            CameraRowVm::Custom => {
                if !vm.camera_sections.is_empty() {
                    cy = self.build_filtered_properties_owned(
                        tree, inner_x, inner_w, cy,
                        (&vm.camera_sections, vm.camera_param_doc_ids.as_deref()),
                    );
                }
                let message = if vm.camera_sections.is_empty() {
                    "Custom (edit in graph)"
                } else {
                    "Movement: controlled by camera graph"
                };
                tree.add_label(Some(self.content_parent), inner_x, cy, inner_w, ROW_H, message, label_style());
                cy + ROW_H
            }
            CameraRowVm::None => {
                // `render_scene`'s `camera` port is REQUIRED (unlike
                // envmap/atmosphere) — every shipped path (importer,
                // Scene Starter) always wires one, so there is no "Add
                // camera" action in v1 (D3).
                tree.add_label(Some(self.content_parent), inner_x, cy, inner_w, ROW_H, "No camera wired", label_style());
                cy + ROW_H
            }
        }
    }

}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::tests::world_transform_vm;
    use crate::input::Modifiers;
    use crate::node::{Rect, Vec2};

    #[test]
    fn camera_boolean_controls_toggle_and_keep_synced_values_after_rebuild() {
        let (mut vm, mut surface) = world_transform_vm();
        vm.camera = CameraRowVm::Custom;
        vm.camera_sections = vec!["Camera".into()];
        vm.camera_param_doc_ids = Some(vec![72, 73]);
        let template = surface.rows[0].clone();
        surface.rows = [("72_enabled", "Motion Blur"), ("73_enabled", "Depth of Field")]
            .into_iter().map(|(id, name)| {
                let mut row = template.clone();
                row.id = id.into();
                row.spec.name = name.into();
                row.spec.section = Some("Camera".into());
                row.spec.is_toggle = true;
                row.spec.min = 0.0;
                row.spec.max = 1.0;
                row.spec.default = 0.0;
                row.value.base = 1.0;
                row.value.effective = 1.0;
                row
            }).collect();
        let mut panel = ScenePanel::new();
        panel.open();
        panel.configure(SceneSetupState::Live(Box::new(vm)));
        panel.configure_params(Some(surface));
        panel.selection.insert(LayerId::new("layer-1"), SceneSelection::Camera);
        let mut tree = UITree::new();
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));
        for (slot, id) in ["72_enabled", "73_enabled"].into_iter().enumerate() {
            assert!(panel.properties_card.row_host.slider_ids[slot].is_none());
            let button = panel.properties_card.row_host.toggle_ids[slot].as_ref().unwrap().button_id;
            assert_eq!(tree.get_node(button).unwrap().text.as_deref(), Some("ON"));
            let (consumed, actions) = panel.handle_event(&UIEvent::Click {
                node_id: button, pos: Vec2::ZERO, modifiers: Modifiers::default(),
            }, &mut tree);
            assert!(consumed);
            assert!(matches!(actions.as_slice(),
                [PanelAction::Params(ParamsAction::ParamToggle(GraphParamTarget::GeneratorOf(layer), param))]
                    if layer.as_str() == "layer-1" && param.as_ref() == id
            ));
        }
        for value in [0.0, 1.0] {
            let values = ["72_enabled", "73_enabled"].map(|id| (id, crate::view::UiParamSlot {
                value, base: value, ..Default::default()
            }));
            panel.sync_properties_values(&mut tree, &mut values.into_iter());
            for rebuilt in [false, true] {
                if rebuilt {
                    tree = UITree::new();
                    panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));
                    panel.sync_properties_values(&mut tree, &mut values.into_iter());
                }
                for ids in panel.properties_card.row_host.toggle_ids.iter().flatten() {
                    assert_eq!(tree.get_node(ids.button_id).unwrap().text.as_deref(),
                        Some(if value > 0.5 { "ON" } else { "OFF" }));
                }
            }
        }
    }

    #[test]
    fn camera_custom_shared_controls_exclude_inactive_movement_and_keep_dispatch() {
        let (mut vm, mut surface) = world_transform_vm();
        vm.camera = CameraRowVm::Custom;
        vm.camera_sections = vec!["Camera".into()];
        vm.camera_param_doc_ids = Some(vec![71, 72, 73]);
        let template = surface.rows[0].clone();
        surface.rows = ["70_distance", "71_focus_distance", "72_enabled", "73_enabled"]
            .into_iter().map(|id| {
                let mut row = template.clone();
                row.id = id.into();
                row.spec.section = Some("Camera".into());
                row.spec.name = id.into();
                row
            }).collect();
        let mut panel = ScenePanel::new();
        panel.open();
        panel.configure(SceneSetupState::Live(Box::new(vm)));
        panel.configure_params(Some(surface));
        panel.selection.insert(LayerId::new("layer-1"), SceneSelection::Camera);
        let mut tree = UITree::new();
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 1000.0));
        let ids: Vec<&str> = panel.properties_card.rows.iter().map(|r| r.id.as_ref()).collect();
        assert_eq!(ids, ["71_focus_distance", "72_enabled", "73_enabled"]);
        let value_cell = panel.properties_card.row_host.slider_ids[0].as_ref().unwrap().value_text;
        let (_, actions) = panel.handle_event(
            &UIEvent::DoubleClick { node_id: value_cell, pos: Vec2::ZERO, modifiers: Modifiers::default() },
            &mut tree,
        );
        assert!(matches!(actions.as_slice(),
            [PanelAction::Root(RootAction::BeginParamTextInput { target, param_id, .. })]
                if *target == GraphParamTarget::GeneratorOf(LayerId::new("layer-1"))
                    && param_id.as_ref() == "71_focus_distance"
        ));
    }

}
