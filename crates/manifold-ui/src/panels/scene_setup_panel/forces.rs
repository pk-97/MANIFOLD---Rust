//! Shared parameter-card hosting for scene forces.
//!
//! Forces are ordinary scene modifier surfaces with a dedicated outliner and
//! selection. Their rows stay on [`ParamCardPanel`], so modulation, drawers,
//! type-in, mapping, and right-click lifecycle use the same host as every
//! other modifier card.

use super::*;
use crate::param_surface::{ModifierCardInfo, ModifierPickerEntry};

impl ScenePanel {
    pub fn configure_force_cards(&mut self, configs: &[ParamSurface]) {
        let pressed_identity = self.force_pressed_card.clone();
        let mut existing = std::mem::take(&mut self.force_cards);
        let mut cards = Vec::with_capacity(configs.len());
        let layer_id = configs
            .iter()
            .find_map(|config| config.modifier.as_ref().map(|info| info.layer_id.clone()));
        for config in configs {
            let Some(info) = config.modifier.as_ref() else {
                continue;
            };
            let identity = info.instance_id.clone();
            let index = existing.iter().position(|card| {
                card.modifier_info()
                    .is_some_and(|old| old.layer_id == info.layer_id && old.instance_id == identity)
            });
            let mut card = index
                .map(|index| existing.swap_remove(index))
                .unwrap_or_else(ParamCardPanel::new);
            let collapsed = index.is_some() && card.is_collapsed();
            card.configure(config);
            if collapsed {
                card.set_collapsed(true);
            }
            cards.push(card);
        }
        self.force_cards = cards;
        self.force_pressed_card = pressed_identity.and_then(|(old_layer, old_id)| {
            let current_layer = layer_id.as_ref()?;
            (old_layer == *current_layer)
                .then_some((old_layer, old_id))
                .filter(|(_, id)| {
                    self.force_cards.iter().any(|card| {
                        card.modifier_info()
                            .is_some_and(|info| &info.instance_id == id)
                    })
                })
        });
    }

    pub fn clear_force_cards(&mut self) {
        self.force_cards.clear();
        self.force_pressed_card = None;
    }

    pub fn force_picker(&self) -> &[ModifierPickerEntry] {
        match &self.state {
            SceneSetupState::Live(vm) => &vm.force_picker,
            _ => &[],
        }
    }

    pub fn force_card_info(&self, id: &manifold_foundation::NodeId) -> Option<&ModifierCardInfo> {
        let SceneSetupState::Live(vm) = &self.state else {
            return None;
        };
        if !vm.forces.iter().any(|row| &row.instance_id == id) {
            return None;
        }
        self.force_cards
            .iter()
            .filter_map(ParamCardPanel::modifier_info)
            .find(|info| &info.instance_id == id && self.live_layer_id() == Some(&info.layer_id))
    }

    pub fn selected_force(&self) -> Option<(LayerId, manifold_foundation::NodeId)> {
        let SceneSetupState::Live(vm) = &self.state else {
            return None;
        };
        let SceneSelection::Force(id) = self
            .selection
            .get(&vm.layer_id)
            .cloned()
            .filter(|selection| Self::selection_exists(vm, selection.clone()))?
        else {
            return None;
        };
        Some((vm.layer_id.clone(), id))
    }

    pub fn force_cards_mut(&mut self) -> &mut [ParamCardPanel] {
        &mut self.force_cards
    }

    pub(crate) fn build_force_card(
        &mut self,
        tree: &mut UITree,
        inner_x: f32,
        inner_w: f32,
        cy: f32,
        id: &manifold_foundation::NodeId,
    ) -> f32 {
        let Some(card) = self.force_cards.iter_mut().find(|card| {
            card.modifier_info()
                .is_some_and(|info| &info.instance_id == id)
        }) else {
            return cy;
        };
        let height = card.compute_height().max(1.0);
        card.build(tree, Rect::new(inner_x, cy, inner_w, height));
        cy + height + ROW_GAP
    }

    pub(crate) fn force_card_index_for_node(&self, node_id: NodeId) -> Option<usize> {
        let index = node_id.index();
        self.force_cards.iter().position(|card| {
            let first = card.first_node();
            card.node_count() > 0 && index >= first && index < first + card.node_count()
        })
    }

    pub(crate) fn force_card_click(
        &mut self,
        node_id: NodeId,
        tree: &mut UITree,
    ) -> Option<Vec<PanelAction>> {
        let index = self.force_card_index_for_node(node_id)?;
        let info = self.force_cards[index].modifier_info()?.clone();

        self.selection.insert(
            info.layer_id.clone(),
            SceneSelection::Force(info.instance_id.clone()),
        );
        for (card_index, card) in self.force_cards.iter_mut().enumerate() {
            card.update_selection_visual(tree, card_index == index);
        }
        let actions = self.force_cards[index].handle_click(node_id, tree);
        if actions.iter().any(|action| {
            matches!(
                action,
                PanelAction::Params(ParamsAction::ModifierCardClicked(_))
            )
        }) {
            return Some(vec![PanelAction::Root(
                RootAction::SceneSetupSelectionChanged(info.layer_id),
            )]);
        }
        Some(actions)
    }

    pub(crate) fn force_card_pointer_down(
        &mut self,
        node_id: NodeId,
        pos: Vec2,
        tree: &mut UITree,
    ) -> Option<Vec<PanelAction>> {
        let index = self.force_card_index_for_node(node_id)?;
        let info = self.force_cards[index].modifier_info()?.clone();

        self.selection.insert(
            info.layer_id.clone(),
            SceneSelection::Force(info.instance_id.clone()),
        );
        for (card_index, card) in self.force_cards.iter_mut().enumerate() {
            card.update_selection_visual(tree, card_index == index);
        }
        self.force_pressed_card = Some((info.layer_id, info.instance_id));
        Some(self.force_cards[index].handle_pointer_down(node_id, pos, tree))
    }

    pub(crate) fn force_card_double_click(
        &self,
        node_id: NodeId,
        tree: &UITree,
    ) -> Option<PanelAction> {
        let index = self.force_card_index_for_node(node_id)?;
        self.force_cards[index].value_cell_typein(node_id, tree)
    }

    pub(crate) fn force_card_drag(
        &mut self,
        pos: Vec2,
        tree: &mut UITree,
        fine: bool,
    ) -> Vec<PanelAction> {
        let index = self.force_pressed_card.as_ref().and_then(|(layer, id)| {
            self.force_cards.iter().position(|card| {
                card.modifier_info()
                    .is_some_and(|info| &info.layer_id == layer && &info.instance_id == id)
            })
        });
        index
            .and_then(|index| self.force_cards.get_mut(index))
            .map(|card| card.handle_drag(pos, tree, fine))
            .unwrap_or_default()
    }

    pub(crate) fn end_force_card_gesture(&mut self, tree: &mut UITree) -> Vec<PanelAction> {
        let Some((layer, id)) = self.force_pressed_card.take() else {
            return Vec::new();
        };
        let index = self.force_cards.iter().position(|card| {
            card.modifier_info()
                .is_some_and(|info| info.layer_id == layer && info.instance_id == id)
        });
        index
            .and_then(|index| self.force_cards.get_mut(index))
            .map(|card| card.handle_drag_end(tree))
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Modifiers;
    use crate::panels::param_card::ParamCardKind;
    use crate::param_surface::{ParamRow, RowMapping, RowSpec, RowValue};

    fn force_surface(layer: &str, id: &str) -> ParamSurface {
        ParamSurface {
            kind: ParamCardKind::Effect,
            title: format!("Force {id}"),
            collapsed: false,
            enabled: true,
            effect_index: 0,
            effect_id: manifold_foundation::EffectId::new(format!("force-{id}")),
            supports_envelopes: true,
            has_graph_mod: false,
            layer_id: Some(LayerId::new(layer)),
            modifier: Some(ModifierCardInfo {
                instance_id: manifold_foundation::NodeId::new(id),
                layer_id: LayerId::new(layer),
                enabled_label: "Enabled".into(),
                stack_index: 0,
                stack_len: 1,
                targets_all: true,
                objects: Vec::new(),
            }),
            rows: vec![ParamRow {
                id: std::borrow::Cow::Owned(format!("{id}_strength")),
                spec: RowSpec {
                    name: "Strength".into(),
                    min: -1.0,
                    max: 1.0,
                    default: 0.0,
                    whole_numbers: false,
                    is_angle: false,
                    is_toggle: false,
                    is_trigger: false,
                    is_trigger_gate: false,
                    value_labels: None,
                    section: None,
                    disabled: None,
                    material_role: None,
                    inactive_reason: None,
                },
                value: RowValue {
                    base: 0.25,
                    effective: 0.25,
                    exposed: true,
                    driven: false,
                },
                audio: Default::default(),
                modulation: Default::default(),
                mapping: RowMapping {
                    osc_address: None,
                    ableton_display: None,
                    ableton_range: None,
                    mappable: true,
                },
                scene_addr: None,
                rgb_members: None,
                material_attached: false,
            }],
            string_params: Vec::new(),
            audio_sends: Vec::new(),
            relight: Default::default(),
        }
    }

    #[test]
    fn force_cards_reconcile_by_layer_and_instance_and_cancel_stale_gesture() {
        let layer = LayerId::new("layer-a");
        let id = manifold_foundation::NodeId::new("force");
        let mut panel = ScenePanel::new();
        panel.configure_force_cards(&[force_surface("layer-a", "force")]);
        assert_eq!(panel.force_cards.len(), 1);
        assert_eq!(panel.force_cards[0].rows[0].id.as_ref(), "force_strength");
        panel.force_pressed_card = Some((layer.clone(), id.clone()));

        panel.configure_force_cards(&[force_surface("layer-a", "force")]);
        assert_eq!(panel.force_pressed_card, Some((layer.clone(), id.clone())));

        panel.configure_force_cards(&[force_surface("layer-b", "force")]);
        assert!(panel.force_pressed_card.is_none());
    }

    #[test]
    fn force_card_routes_target_and_strength_gestures_to_its_own_layer() {
        let mut panel = ScenePanel::new();
        let surface = force_surface("layer-a", "force");
        panel.configure(SceneSetupState::Live(Box::new(force_vm(surface.clone()))));
        panel.configure_force_cards(&[surface]);
        let mut tree = UITree::new();
        panel.force_cards[0].build(&mut tree, Rect::new(0.0, 0.0, 320.0, 300.0));
        let targets = panel.force_cards[0].modifier_objects_node_id().unwrap();
        assert!(
            matches!(panel.force_card_click(targets, &mut tree).unwrap().as_slice(),
            [PanelAction::Root(RootAction::SceneModifierObjectsClicked(layer, id))]
                if layer.as_str() == "layer-a" && id.as_str() == "force")
        );
        let named_node = |name: &str| {
            tree.nodes()
                .iter()
                .find(|node| tree.name_of(node.id) == Some(name))
                .unwrap()
                .id
        };
        let slider = named_node("param_row.force_strength.slider");
        let value = named_node("param_row.force_strength.value");
        let track = tree.get_bounds(slider);
        let actions = panel
            .force_card_pointer_down(
                slider,
                Vec2::new(track.x + track.width * 0.5, track.y),
                &mut tree,
            )
            .unwrap();
        use crate::panels::{ScrubPhase, ValueRef};
        assert!(matches!(actions.first(), Some(PanelAction::Scrub(
            ValueRef::Param(GraphParamTarget::GeneratorOf(layer), param), ScrubPhase::Begin))
                if layer.as_str() == "layer-a" && param.as_ref() == "force_strength"));
        let end = panel.end_force_card_gesture(&mut tree);
        assert!(end.iter().any(|action| matches!(action,
            PanelAction::Scrub(ValueRef::Param(GraphParamTarget::GeneratorOf(layer), param), ScrubPhase::Commit)
                if layer.as_str() == "layer-a" && param.as_ref() == "force_strength")));
        assert!(panel.force_card_double_click(value, &tree).is_some());
    }

    fn force_vm(surface: ParamSurface) -> SceneSetupVm {
        SceneSetupVm {
            layer_id: LayerId::new("layer-a"),
            scene_name: "Scene".into(),
            multiple_scenes: false,
            object_count: 0,
            light_count: 0,
            shadow_caster_count: 0,
            scene_root_node_id: 0,
            environment: EnvironmentRowVm::None,
            atmosphere: AtmosphereRowVm::None,
            objects: Vec::new(),
            fluid_domains: Vec::new(),
            lights: Vec::new(),
            forces: vec![SceneForceRowVm {
                instance_id: surface.modifier.as_ref().unwrap().instance_id.clone(),
                title: surface.title,
            }],
            force_picker: vec![ModifierPickerEntry {
                preset_id: "gravity".into(),
                label: "Gravity".into(),
                disabled: None,
            }],
            camera: CameraRowVm::None,
            camera_sections: Vec::new(),
            camera_param_doc_ids: None,
            world_sections: Vec::new(),
            scene_bounds: None,
        }
    }

    #[test]
    fn force_outliner_has_stable_selection_and_add_action() {
        let surface = force_surface("layer-a", "force");
        let force_id = manifold_foundation::NodeId::new("force");
        let mut panel = ScenePanel::new();
        panel.open();
        panel.configure(SceneSetupState::Live(Box::new(force_vm(surface.clone()))));
        panel.configure_force_cards(&[surface]);
        let mut tree = UITree::new();
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));

        let add_force = panel.add_force_id.expect("force affordance is built");
        let (_, actions) = panel.handle_event(
            &UIEvent::Click {
                node_id: add_force,
                pos: Vec2::ZERO,
                modifiers: Modifiers::default(),
            },
            &mut tree,
        );
        assert!(matches!(
            actions.as_slice(),
            [PanelAction::Root(RootAction::SceneSetupAddForceClicked(layer))]
                if layer == &LayerId::new("layer-a")
        ));

        let force_row = panel
            .outliner_row_ids
            .iter()
            .find(|(_, selection)| selection == &SceneSelection::Force(force_id.clone()))
            .map(|(node, _)| *node)
            .expect("force row is selectable");
        let (consumed, _) = panel.handle_event(
            &UIEvent::Click {
                node_id: force_row,
                pos: Vec2::ZERO,
                modifiers: Modifiers::default(),
            },
            &mut tree,
        );
        assert!(consumed);
        assert_eq!(
            panel.selected_force(),
            Some((LayerId::new("layer-a"), force_id))
        );

        panel.configure(SceneSetupState::Live(Box::new(force_vm(force_surface(
            "layer-a",
            "replacement",
        )))));
        assert!(panel.selected_force().is_none());
    }
}
