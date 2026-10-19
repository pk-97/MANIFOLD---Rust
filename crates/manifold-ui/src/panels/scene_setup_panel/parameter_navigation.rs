//! Addressed parameter navigation for the Scene tab.
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

    /// The item that owns param `id`: the item whose projected controls list
    /// it (`scene_items::owner_of`, app-side), or the object whose material
    /// drawer holds it.
    fn owner_selection(vm: &SceneSetupVm, id: &str) -> Option<SceneSelection> {
        if let Some(object) = vm.objects.iter().find_map(|object| {
            let ObjectRowVm::Known(object) = object else { return None; };
            object.material_inspector.as_ref().is_some_and(|material| {
                material.object_gain.as_ref().is_some_and(|owned| owned.as_ref() == id)
                    || material.params.iter().any(|(_, owned)| owned.as_ref() == id)
            }).then_some(object.object_node_id)
        }) {
            return Some(SceneSelection::Object(object));
        }
        vm.items
            .iter()
            .find(|item| item.parameter_ids.iter().any(|owned| owned == id))
            .map(|item| item.selection.clone())
    }

    fn has_param(&self, param: &manifold_foundation::ParamId) -> bool {
        self.full_params.as_ref().is_some_and(|surface| surface.rows.iter().any(|row| row.id == *param))
    }

    /// The layer whose scene this tab shows, when it holds `param`.
    pub fn scene_layer_with(&self, param: &manifold_foundation::ParamId) -> Option<&LayerId> {
        self.live_layer_id().filter(|_| self.has_param(param))
    }

    /// Show `param` in the Scene tab: select the item that owns it, unfold
    /// its group, open an active response tab and scroll it into view on the
    /// next build. Ownership comes from the projected item list, never a
    /// section string (SCENE_SURFACES_DESIGN.md D10). Changes no authored
    /// state; `false` leaves the selection as it was.
    pub fn reveal_param(&mut self, target: &UiGraphTarget, param: &manifold_foundation::ParamId) -> bool {
        if !self.target_matches_scene(target) || !self.has_param(param) {
            return false;
        }
        let Some(layer_id) = self.live_layer_id().cloned() else {
            return false;
        };

        if let Some(index) = self.force_cards.iter_mut().position(|card| card.reveal_param(param)) {
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

        if let Some(index) = self.object_modifier_cards.iter_mut().position(|card| card.reveal_param(param)) {
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

        let Some(selection) = self.state.as_live().and_then(|vm| Self::owner_selection(vm, param.as_ref())) else {
            return false;
        };
        self.set_selection(layer_id, selection);
        self.revealed_property = Some((target.clone(), param.clone()));
        self.pending_parameter_reveal = Some((target.clone(), param.clone()));
        true
    }

    /// A card section title's reveal (SCENE_SURFACES_DESIGN.md D10): the
    /// section's first row owned by a light, object or force item, else its
    /// first row. So Lighting opens a light even when a camera row leads it.
    pub fn reveal_section(&mut self, target: &UiGraphTarget, params: &[manifold_foundation::ParamId]) -> bool {
        let item_owned = |panel: &Self, param: &manifold_foundation::ParamId| {
            panel.force_cards.iter().chain(&panel.object_modifier_cards).any(|card| card.row(param.as_ref()).is_some())
                || panel.state.as_live().is_some_and(|vm| {
                    matches!(
                        Self::owner_selection(vm, param.as_ref()),
                        Some(SceneSelection::Light(_) | SceneSelection::Object(_) | SceneSelection::Force(_))
                    )
                })
        };
        let pick = params.iter().find(|param| item_owned(self, param)).or(params.first()).cloned();
        pick.is_some_and(|param| self.reveal_param(target, &param))
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
        self.properties_card.row_host.open_colour_of(&self.properties_card.rows, param.as_ref());
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
            && self.has_param(param)
            && (self.force_cards.iter().chain(self.object_modifier_cards.iter())
                .any(|card| card.rows.iter().any(|row| row.id == *param))
                || self.state.as_live().is_some_and(|vm| Self::owner_selection(vm, param.as_ref()).is_some()));
        if !valid { self.pending_parameter_reveal = None; }
    }

}

#[cfg(test)]
mod tests {
    use super::super::tests::world_transform_vm;
    use super::*;

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
        let mut object_b = object_a.clone();
        object_b.object_node_id = 41;
        vm.objects = vec![
            ObjectRowVm::Known(Box::new(object_a)),
            ObjectRowVm::Known(Box::new(object_b)),
        ];
        super::super::tests::set_item_ids(&mut vm, SceneSelection::Object(40), &["shared_a"]);
        super::super::tests::set_item_ids(&mut vm, SceneSelection::Object(41), &["shared_b"]);

        let mut row_a = surface.rows[0].clone();
        row_a.id = "shared_a".into();
        let mut row_b = row_a.clone();
        row_b.id = "shared_b".into();
        row_b.spec.material_role = role;
        row_b.value.base = 0.75;
        row_b.value.effective = 0.75;
        row_b.modulation.envelope_active = true;
        surface.rows = vec![row_a, row_b];
        surface.supports_envelopes = true;

        let mut panel = ScenePanel::new();
        panel.open();
        panel.configure(super::super::tests::live(vm));
        panel.configure_params(Some(surface));
        let param: manifold_foundation::ParamId = "shared_b".into();
        assert!(panel.reveal_param(&target, &param));
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
        assert!(panel.reveal_param(&target, &param));
        panel.configure_params(None);
        assert!(panel.revealed_property.is_none());
    }

    /// Every scene row reveals its owning item by id alone: no clip trigger,
    /// no section title. A force row reveals through its force card.
    #[test]
    fn reveal_param_selects_owning_item_for_light_object_force_world() {
        let (mut vm, template) = world_transform_vm();
        let layer = vm.layer_id.clone();
        let target = UiGraphTarget::Generator(layer.clone());
        let object = vm
            .objects
            .iter()
            .find_map(|object| match object {
                ObjectRowVm::Known(row) => Some(row.object_node_id),
                ObjectRowVm::Custom { .. } => None,
            })
            .expect("fixture object");
        let force = manifold_foundation::NodeId::new("gravity");
        vm.forces.push(SceneForceRowVm { instance_id: force.clone(), title: "Gravity".into() });
        let physics = SceneSelection::Physics(manifold_foundation::NodeId::new("world"));
        for (selection, ids) in [
            (SceneSelection::Camera, &["camera_param"][..]),
            (SceneSelection::Light(60), &["light_param"]),
            (SceneSelection::Object(object), &["object_param"]),
            (SceneSelection::World, &["world_param", "ray_tracing_param"]),
            (physics.clone(), &["physics_param"]),
        ] {
            super::super::tests::set_item_ids(&mut vm, selection, ids);
        }
        let row = |id: &str| {
            let mut row = template.rows[0].clone();
            row.id = id.to_string().into();
            row
        };
        let ids = ["camera_param", "light_param", "object_param", "world_param", "ray_tracing_param", "physics_param", "force_strength"];
        let mut surface = template.clone();
        surface.rows = ids.iter().map(|id| row(id)).collect();
        let mut force_card = template.clone();
        force_card.kind = crate::panels::param_card::ParamCardKind::Effect;
        force_card.effect_id = manifold_foundation::EffectId::new("force-gravity");
        force_card.modifier = Some(crate::param_surface::ModifierCardInfo {
            instance_id: force.clone(),
            layer_id: layer.clone(),
            params_owner: Default::default(),
            enabled_label: "Enabled".into(),
            stack_index: 0,
            stack_len: 1,
            targets_all: true,
            objects: Vec::new(),
        });
        force_card.rows = vec![row("force_strength")];

        let mut panel = ScenePanel::new();
        panel.open();
        panel.configure(super::super::tests::live(vm));
        panel.configure_params(Some(surface));
        panel.configure_force_cards(&[force_card]);
        panel.force_cards[0].set_collapsed(true);
        for (id, expected) in [
            ("camera_param", SceneSelection::Camera),
            ("light_param", SceneSelection::Light(60)),
            ("object_param", SceneSelection::Object(object)),
            ("world_param", SceneSelection::World),
            ("ray_tracing_param", SceneSelection::World),
            ("physics_param", physics.clone()),
            ("force_strength", SceneSelection::Force(force.clone())),
        ] {
            let param: manifold_foundation::ParamId = id.into();
            assert!(panel.reveal_param(&target, &param), "{id}");
            assert_eq!(panel.selection.get(&layer), Some(&expected), "{id}");
        }
        assert!(!panel.force_cards[0].is_collapsed(), "the force card unfolds to show its row");
        assert!(!panel.reveal_param(&target, &"nobody_owns_this".into()));
        assert_eq!(panel.selection.get(&layer), Some(&SceneSelection::Force(force)), "a miss keeps the selection");
    }

    /// D10: a section title opens the first light, object or force row's
    /// owner; a section with none of those opens its first row's owner.
    #[test]
    fn reveal_section_prefers_light_object_force_owner_over_first_row() {
        let (mut vm, template) = world_transform_vm();
        let layer = vm.layer_id.clone();
        let target = UiGraphTarget::Generator(layer.clone());
        super::super::tests::set_item_ids(&mut vm, SceneSelection::Camera, &["exposure", "orbit"]);
        super::super::tests::set_item_ids(&mut vm, SceneSelection::Light(60), &["sun_intensity"]);
        let mut surface = template.clone();
        surface.rows = ["exposure", "orbit", "sun_intensity"]
            .iter()
            .map(|id| {
                let mut row = template.rows[0].clone();
                row.id = id.to_string().into();
                row
            })
            .collect();
        let mut panel = ScenePanel::new();
        panel.open();
        panel.configure(super::super::tests::live(vm));
        panel.configure_params(Some(surface));

        let lighting: Vec<manifold_foundation::ParamId> = vec!["exposure".into(), "sun_intensity".into()];
        assert!(panel.reveal_section(&target, &lighting));
        assert_eq!(panel.selection.get(&layer), Some(&SceneSelection::Light(60)), "Lighting opens the light");

        let camera: Vec<manifold_foundation::ParamId> = vec!["exposure".into(), "orbit".into()];
        assert!(panel.reveal_section(&target, &camera));
        assert_eq!(panel.selection.get(&layer), Some(&SceneSelection::Camera), "no item-owned row: first row");

        assert!(!panel.reveal_section(&target, &[]));
    }

    #[test]
    fn missing_navigation_target_keeps_existing_selection() {
        let (vm, surface) = world_transform_vm();
        let mut panel = ScenePanel::new();
        panel.configure(super::super::tests::live(vm));
        panel.configure_params(Some(surface));
        panel.set_selection(LayerId::new("layer-1"), SceneSelection::World);
        let wrong_target = UiGraphTarget::Generator(LayerId::new("other"));
        let param: manifold_foundation::ParamId = "translate_x".into();
        assert!(!panel.reveal_param(&wrong_target, &param));
        assert_eq!(
            panel.selection.get(&LayerId::new("layer-1")),
            Some(&SceneSelection::World)
        );
    }
}
