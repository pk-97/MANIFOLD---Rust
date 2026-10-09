//! Addressed parameter navigation for the Scene Setup panel.
//!
//! Navigation uses the same projected row identities as rendering and
//! dispatch. It only changes UI-local selection, fold, and drawer state.

use super::*;
use crate::panels::param_slider_shared::{active_mod_tabs, row_drawer_height};
use crate::view::UiGraphTarget;

impl ScenePanel {
    fn target_matches_scene(&self, target: &UiGraphTarget) -> bool {
        matches!(target, UiGraphTarget::Generator(layer) if self.live_layer_id() == Some(layer))
    }

    fn property_selection_for_row(vm: &SceneSetupVm, row: &ParamRow) -> Option<SceneSelection> {
        let id = row.id.as_ref();
        if let Some(object) = vm.objects.iter().find_map(|object| {
            let ObjectRowVm::Known(object) = object else { return None; };
            let owned = object.parameter_ids.iter().any(|owned| owned == id)
                || object.material_inspector.as_ref().is_some_and(|material| {
                    material.object_gain.as_ref().is_some_and(|owned| owned.as_ref() == id)
                        || material.params.iter().any(|(_, owned)| owned.as_ref() == id)
                });
            owned.then_some(object.object_node_id)
        }) {
            return Some(SceneSelection::Object(object));
        }
        let section = row.spec.section.as_deref();
        if vm.camera_parameter_ids.as_ref().map_or_else(
            || section.is_some_and(|section| vm.camera_sections.iter().any(|owned| owned == section)),
            |ids| ids.iter().any(|owned| owned == id),
        ) {
            return Some(SceneSelection::Camera);
        }
        if let Some(light) = vm.lights.iter().find_map(|light| match light {
            LightRowVm::Known(light) => section
                .is_some_and(|section| light.sections.iter().any(|owned| owned == section))
                .then_some(light.node_doc_id),
            LightRowVm::Custom { .. } => None,
        }) {
            return Some(SceneSelection::Light(light));
        }
        [
            (&["Environment", "Atmosphere"][..], SceneSelection::World),
            (&["Physics"][..], SceneSelection::Physics),
            (&["Rendering"][..], SceneSelection::Rendering),
        ].into_iter().find_map(|(categories, selection)| {
            section.is_some_and(|section| world_sections_for(&vm.world_sections, categories)
                .iter().any(|owned| owned == section)).then_some(selection)
        })
    }

    /// Select and open the response drawer for a projected scene parameter.
    /// Card hosts own response matching; ordinary properties only unfold the
    /// existing projected section and choose an already-active response tab.
    pub fn reveal_clip_response(
        &mut self,
        target: &UiGraphTarget,
        param: &manifold_foundation::ParamId,
    ) -> bool {
        if !self.target_matches_scene(target) {
            return false;
        }
        let Some(full_row_index) = self.full_params.as_ref().and_then(|surface| {
            surface.rows.iter().position(|row| {
                row.id == *param
                    && row
                        .clip_trigger
                        .as_ref()
                        .is_some_and(|source| source.target == *target)
            })
        }) else {
            return false;
        };
        let Some(layer_id) = self.live_layer_id().cloned() else {
            return false;
        };

        if let Some(index) = self
            .force_cards
            .iter_mut()
            .position(|card| card.reveal_clip_response(target, param))
        {
            let Some(info) = self.force_cards[index].modifier_info().cloned() else {
                return false;
            };
            self.set_selection(
                info.layer_id.clone(),
                SceneSelection::Force(info.instance_id),
            );
            self.pending_parameter_reveal = Some((target.clone(), param.clone()));
            return true;
        }

        if let Some(index) = self
            .object_modifier_cards
            .iter_mut()
            .position(|card| card.reveal_clip_response(target, param))
        {
            let Some(info) = self.object_modifier_cards[index]
                .object_modifier_info()
                .cloned()
            else {
                return false;
            };
            self.set_selection(
                info.layer_id.clone(),
                SceneSelection::Object(info.object_id),
            );
            self.selected_object_modifier = Some(info);
            self.pending_parameter_reveal = Some((target.clone(), param.clone()));
            return true;
        }

        let Some(vm) = self.state.as_live() else {
            return false;
        };
        let Some(full_row) = self
            .full_params
            .as_ref()
            .and_then(|surface| surface.rows.get(full_row_index))
        else {
            return false;
        };
        let Some(selection) = Self::property_selection_for_row(vm, full_row) else {
            return false;
        };
        self.set_selection(layer_id, selection);
        self.revealed_property = Some((target.clone(), param.clone()));
        self.pending_parameter_reveal = Some((target.clone(), param.clone()));
        true
    }

    pub(super) fn property_explicitly_revealed(&self, row: &ParamRow) -> bool {
        self.revealed_property.as_ref().is_some_and(|(target, param)| {
            self.target_matches_scene(target) && row.id == *param
        })
    }

    /// Prepare an ordinary scene row for the pending reveal after the filtered
    /// card has been configured, before its section headers and rows are drawn.
    pub(super) fn prepare_pending_parameter_properties(&mut self) {
        let Some((target, param)) = self.pending_parameter_reveal.as_ref() else {
            return;
        };
        if !self.target_matches_scene(target) {
            return;
        }
        let Some(index) = self
            .properties_card
            .rows
            .iter()
            .position(|row| row.id == *param)
        else {
            return;
        };
        let section = self.material_section_name(&self.properties_card.rows[index]);
        if let Some(section) = section {
            self.section_folded.insert(section, false);
        }
        let active = active_mod_tabs(
            &self.properties_card.mod_state,
            &self.properties_card.rows[index],
            index,
        );
        if let Some(tab) = [ModTab::Envelope, ModTab::Audio]
            .into_iter()
            .find(|tab| active.contains(tab))
        {
            self.properties_card.mod_active_tab[index] = tab;
        }
    }

    /// Reveal whichever addressed host is visible in the freshly built tree.
    /// The pending address stays queued when its row is not built yet.
    pub(super) fn reveal_pending_parameter_response(&mut self, tree: &mut UITree) {
        let Some((target, param)) = self.pending_parameter_reveal.as_ref() else {
            return;
        };
        let bounds = self
            .force_cards
            .iter()
            .find_map(|card| card.clip_response_rect(tree, param.as_ref()))
            .or_else(|| {
                self.object_modifier_cards
                    .iter()
                    .find_map(|card| card.clip_response_rect(tree, param.as_ref()))
            })
            .or_else(|| {
                if !self.target_matches_scene(target) {
                    return None;
                }
                let index = *self.properties_card.row_id_index.get(param.as_ref())?;
                let row = self.properties_card.rows.get(index)?;
                let mut rect = self.properties_card.row_host.param_row_rect(tree, index)?;
                rect.height += row_drawer_height(
                    false,
                    &self.properties_card.mod_state,
                    &self.properties_card.mod_active_tab,
                    row,
                    index,
                );
                Some(rect)
            });
        if let Some(bounds) = bounds {
            self.scroll.reveal_rect(tree, bounds);
            self.pending_parameter_reveal = None;
        }
    }

    /// Drop navigation when its current projection no longer owns the address.
    pub(super) fn clear_stale_parameter_reveal(&mut self) {
        if self.revealed_property.as_ref().is_some_and(|(target, param)| {
            !self.target_matches_scene(target)
                || !self.full_params.as_ref().is_some_and(|surface| {
                    surface.rows.iter().any(|row| row.id == *param)
                })
        }) {
            self.revealed_property = None;
        }
        let Some((target, param)) = self.pending_parameter_reveal.as_ref() else { return; };
        let valid = self.target_matches_scene(target)
            && self.full_params.as_ref().and_then(|surface| surface.rows.iter().find(|row|
                row.id == *param && row.clip_trigger.as_ref().is_some_and(|source| source.target == *target)))
                .is_some_and(|row| {
                    self.force_cards.iter().chain(self.object_modifier_cards.iter()).any(|card|
                        card.rows.iter().any(|row| row.id == *param))
                        || self.state.as_live().is_some_and(|vm|
                            Self::property_selection_for_row(vm, row).is_some())
                });
        if !valid { self.pending_parameter_reveal = None; }
    }

}

#[cfg(test)]
mod tests {
    use super::super::tests::world_transform_vm;
    use super::*;
    use crate::param_surface::ClipTriggerRow;

    fn arm(row: &mut ParamRow, target: &UiGraphTarget) {
        row.clip_trigger = Some(ClipTriggerRow {
            target: target.clone(),
            source_label: "Main lane".into(),
        });
    }

    #[test]
    fn shared_object_section_uses_exact_parameter_owner() {
        assert_object_response_navigation(None);
    }

    #[test]
    fn explicitly_revealed_material_response_remains_reachable() {
        for role in [
            MaterialParamRole::Placement(MaterialMapFamily::Base, crate::param_surface::UvComponent::M00),
            MaterialParamRole::Sampler(MaterialMapFamily::Normal, crate::param_surface::SamplerComponent::WrapU),
        ] {
            assert_object_response_navigation(Some(role));
        }
    }

    fn assert_object_response_navigation(role: Option<MaterialParamRole>) {
        let (mut vm, mut surface) = world_transform_vm();
        let target = UiGraphTarget::Generator(vm.layer_id.clone());
        let first = vm
            .objects
            .iter()
            .find_map(|object| match object {
                ObjectRowVm::Known(row) => Some(row.as_ref().clone()),
                ObjectRowVm::Custom { .. } => None,
            })
            .expect("fixture object");
        let mut object_a = first.clone();
        object_a.object_node_id = 40;
        object_a.parameter_ids = vec!["shared_a".into()];
        object_a.sections = vec!["Shared".into()];
        let mut object_b = object_a.clone();
        object_b.object_node_id = 41;
        object_b.parameter_ids = vec!["shared_b".into()];
        vm.objects = vec![
            ObjectRowVm::Known(Box::new(object_a)),
            ObjectRowVm::Known(Box::new(object_b)),
        ];

        let mut row_a = surface.rows[0].clone();
        row_a.id = "shared_a".into();
        row_a.spec.section = Some("Shared".into());
        arm(&mut row_a, &target);
        let mut row_b = row_a.clone();
        row_b.id = "shared_b".into();
        row_b.spec.material_role = role;
        row_b.value.base = 0.75;
        row_b.value.effective = 0.75;
        row_b.modulation.envelope_active = true;
        arm(&mut row_b, &target);
        surface.rows = vec![row_a, row_b];
        surface.supports_envelopes = true;

        let mut panel = ScenePanel::new();
        panel.open();
        panel.configure(SceneSetupState::Live(Box::new(vm)));
        panel.configure_params(Some(surface));
        let param: manifold_foundation::ParamId = "shared_b".into();
        assert!(panel.reveal_clip_response(&target, &param));
        assert_eq!(
            panel.selection.get(&LayerId::new("layer-1")),
            Some(&SceneSelection::Object(41))
        );
        assert_eq!(panel.full_params.as_ref().unwrap().rows[1].value.base, 0.75);
        let mut tree = UITree::new();
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 1000.0));
        let index = *panel.properties_card.row_id_index.get("shared_b").expect("selected object's row");
        assert!(!panel.properties_card.row_id_index.contains_key("shared_a"));
        assert!(panel.properties_card.row_host.param_row_rect(&tree, index).is_some());
        assert_eq!(panel.properties_card.mod_active_tab[index], ModTab::Envelope);
        assert!(panel.pending_parameter_reveal.is_none());
        tree.clear();
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 1000.0));
        assert!(panel.properties_card.row_id_index.contains_key("shared_b"), "response must remain reachable after the reveal frame");
        panel.set_selection(LayerId::new("layer-1"), SceneSelection::Object(40));
        assert!(panel.revealed_property.is_none());
        assert!(panel.reveal_clip_response(&target, &param));
        panel.configure_params(None);
        assert!(panel.revealed_property.is_none());
    }

    #[test]
    fn camera_light_world_navigation_uses_projected_ownership() {
        let (mut vm, template) = world_transform_vm();
        let target = UiGraphTarget::Generator(vm.layer_id.clone());
        vm.objects = vec![ObjectRowVm::Custom { index: 0 }];
        vm.camera_parameter_ids = Some(vec!["camera_param".into()]);
        vm.camera_sections = vec!["Camera".into()];
        vm.world_sections = vec!["Environment".into(), "Physics".into(), "Rendering".into()];
        if let Some(LightRowVm::Known(light)) = vm.lights.first_mut() {
            light.sections = vec!["Light".into()];
        }
        let rows = [
            ("camera_param", "Camera"),
            ("light_param", "Light"),
            ("world_param", "Environment"),
            ("physics_param", "Physics"),
            ("render_param", "Rendering"),
        ]
        .into_iter()
        .map(|(id, section)| {
            let mut row = template.rows[0].clone();
            row.id = id.into();
            row.spec.section = Some(section.into());
            arm(&mut row, &target);
            row
        })
        .collect();
        let mut surface = template;
        surface.rows = rows;

        let mut panel = ScenePanel::new();
        panel.configure(SceneSetupState::Live(Box::new(vm)));
        panel.configure_params(Some(surface));
        for (id, expected) in [
            ("camera_param", SceneSelection::Camera),
            ("light_param", SceneSelection::Light(60)),
            ("world_param", SceneSelection::World),
            ("physics_param", SceneSelection::Physics),
            ("render_param", SceneSelection::Rendering),
        ] {
            let param: manifold_foundation::ParamId = id.into();
            assert!(panel.reveal_clip_response(&target, &param), "{id}");
            assert_eq!(
                panel.selection.get(&LayerId::new("layer-1")),
                Some(&expected)
            );
        }
    }

    #[test]
    fn missing_navigation_target_keeps_existing_selection() {
        let (vm, surface) = world_transform_vm();
        let mut panel = ScenePanel::new();
        panel.configure(SceneSetupState::Live(Box::new(vm)));
        panel.configure_params(Some(surface));
        panel.set_selection(LayerId::new("layer-1"), SceneSelection::World);
        let wrong_target = UiGraphTarget::Generator(LayerId::new("other"));
        let param: manifold_foundation::ParamId = "translate_x".into();
        assert!(!panel.reveal_clip_response(&wrong_target, &param));
        assert_eq!(
            panel.selection.get(&LayerId::new("layer-1")),
            Some(&SceneSelection::World)
        );
    }
}
