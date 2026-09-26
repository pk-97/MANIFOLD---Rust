//! Shared parameter cards embedded in a Scene Setup object's modifier stack.
//!
//! This module owns only the Scene Setup card host and its reorder gesture.
//! Parameter rows, drawers, mapping, and type-in behaviour remain entirely in
//! [`ParamCardPanel`].

use super::*;
use crate::panels::AudioDrawerClick;

#[derive(Clone, Debug)]
pub(crate) struct ObjectModifierDrag {
    /// Insertion point within the captured object's own modifier stack.
    pub target: usize,
    pub valid: bool,
    pub identity: ObjectModifierCardInfo,
    pub pointer: Option<Vec2>,
}

impl ScenePanel {
    /// Build one shared card surface per object modifier directly from the
    /// layer's authoritative full generator surface. Exposed ids carry the
    /// owner document id as their prefix, so filtering preserves the real
    /// ParamId, descriptor, modulation, audio, and mapping facts verbatim.
    pub(crate) fn rebuild_object_modifier_cards_from_projection(&mut self) {
        let Some(full) = self.full_params.clone() else {
            self.clear_object_modifier_cards();
            return;
        };
        let SceneSetupState::Live(vm) = &self.state else {
            self.clear_object_modifier_cards();
            return;
        };
        let mut configs = Vec::new();
        for object in vm.objects.iter().filter_map(|object| match object {
            ObjectRowVm::Known(row) => Some(row.as_ref()),
            ObjectRowVm::Custom { .. } => None,
        }) {
            for modifier in &object.modifiers {
                let rows: Vec<_> = full
                    .rows
                    .iter()
                    .filter(|row| modifier.parameter_ids.iter().any(|id| id == row.id.as_ref()))
                    .cloned()
                    .map(|mut row| { row.spec.section = None; row })
                    .collect();
                let surface = ParamSurface {
                    kind: crate::panels::param_card::ParamCardKind::Effect,
                    effect_index: 0,
                    effect_id: full.effect_id.clone(),
                    title: modifier.display_name.clone(),
                    enabled: true,
                    collapsed: false,
                    supports_envelopes: full.supports_envelopes,
                    string_params: Vec::new(),
                    layer_id: None,
                    modifier: None,
                    rows,
                    has_graph_mod: false,
                    audio_sends: full.audio_sends.clone(),
                    relight: full.relight,
                };
                let address = ObjectModifierCardInfo {
                    layer_id: vm.layer_id.clone(),
                    object_id: object.object_node_id,
                    group_node_id: object.group_node_id,
                    node_doc_id: modifier.node_doc_id,
                };
                configs.push((surface, address));
            }
        }
        self.configure_object_modifier_cards(&configs);
    }

    /// Reconcile object modifier cards by stable `(layer, object, group,
    /// modifier)` address. Reused cards retain collapse, drawer, and gesture
    /// state while a reordered stack simply changes the vector order.
    pub fn configure_object_modifier_cards(
        &mut self,
        configs: &[(ParamSurface, ObjectModifierCardInfo)],
    ) {
        let mut existing = std::mem::take(&mut self.object_modifier_cards);
        let mut cards = Vec::with_capacity(configs.len());
        for (surface, address) in configs {
            let index = existing
                .iter()
                .position(|card| card.object_modifier_info() == Some(address));
            let mut card = index
                .map(|index| existing.swap_remove(index))
                .unwrap_or_else(ParamCardPanel::new);
            card.configure_object_modifier(surface, address.clone());
            cards.push(card);
        }
        self.object_modifier_cards = cards;
        if self.selected_object_modifier.as_ref().is_some_and(|selected| {
            !self.object_modifier_cards.iter().any(|card| {
                card.object_modifier_info() == Some(selected)
            })
        }) {
            self.selected_object_modifier = None;
        }
        if let Some(drag) = self.object_modifier_drag.as_ref()
            && !self.object_modifier_cards.iter().any(|card| {
                card.object_modifier_info() == Some(&drag.identity)
            })
        {
            self.object_modifier_drag = None;
        }
    }

    /// Drop all object modifier cards when the scene projection has no live
    /// object surface. This keeps stale card nodes from receiving input.
    pub fn clear_object_modifier_cards(&mut self) {
        self.object_modifier_cards.clear();
        self.object_modifier_drag = None;
        self.object_modifier_drag_indicator = None;
        self.selected_object_modifier = None;
    }

    pub fn selected_object_modifier(
        &self,
    ) -> Option<ObjectModifierCardInfo> {
        let SceneSetupState::Live(vm) = &self.state else { return None };
        let SceneSelection::Object(object_id) = self.resolve_selection_readonly(vm) else {
            return None;
        };
        let selected = self.selected_object_modifier.as_ref()?;
        (selected.object_id == object_id
            && self.object_modifier_cards.iter().any(|card| {
                card.object_modifier_info() == Some(selected)
            }))
            .then(|| selected.clone())
    }

    /// Return the graph owner for the currently selected object, including an
    /// object whose modifier stack is empty. Clipboard and keyboard actions
    /// use this owner address rather than inferring it from a visible card.
    pub fn object_modifier_destination(&self) -> Option<(LayerId, u32)> {
        let SceneSetupState::Live(vm) = &self.state else { return None };
        let SceneSelection::Object(object_id) = self.resolve_selection_readonly(vm) else {
            return None;
        };
        let object = vm.objects.iter().find_map(|object| match object {
            ObjectRowVm::Known(row) if row.object_node_id == object_id => Some(row.as_ref()),
            _ => None,
        })?;
        Some((vm.layer_id.clone(), object.group_node_id.unwrap_or(object.object_node_id)))
    }

    /// Select a projected modifier after a context-menu or paste action. The
    /// owner and modifier ids are validated against the current selected
    /// object, so a stale action cannot select a card from another object.
    pub fn select_object_modifier_by_address(
        &mut self,
        layer_id: &LayerId,
        owner_id: u32,
        node_doc_id: u32,
        tree: &mut UITree,
    ) -> bool {
        let SceneSetupState::Live(vm) = &self.state else { return false };
        let SceneSelection::Object(object_id) = self.resolve_selection_readonly(vm) else {
            return false;
        };
        let Some(index) = self.object_modifier_cards.iter().position(|card| {
            let Some(address) = card.object_modifier_info() else { return false };
            address.layer_id == *layer_id
                && address.object_id == object_id
                && address.group_node_id.unwrap_or(address.object_id) == owner_id
                && address.node_doc_id == node_doc_id
        }) else {
            return false;
        };
        self.selected_object_modifier = self.object_modifier_cards[index]
            .object_modifier_info()
            .cloned();
        for (card_index, card) in self.object_modifier_cards.iter_mut().enumerate() {
            card.update_selection_visual(tree, card_index == index);
        }
        if let Some(bounds) = self.object_modifier_cards[index].live_bounds(tree) {
            self.scroll.reveal_rect(tree, bounds);
        }
        true
    }

    /// Select the object modifier owning `node_id`, including a right-click
    /// that the app may route through the intent registry before Scene Setup's
    /// ordinary click handler.
    pub fn select_object_modifier_node(
        &mut self,
        node_id: NodeId,
        tree: &mut UITree,
    ) -> bool {
        let Some(index) = self.card_index_for_node(node_id) else { return false };
        self.selected_object_modifier = self.object_modifier_cards[index]
            .object_modifier_info()
            .cloned();
        for (card_index, card) in self.object_modifier_cards.iter_mut().enumerate() {
            card.update_selection_visual(tree, card_index == index);
        }
        true
    }

    fn resolve_selection_readonly(&self, vm: &SceneSetupVm) -> SceneSelection {
        self.selection
            .get(&vm.layer_id)
            .copied()
            .filter(|selection| Self::selection_exists(vm, *selection))
            .unwrap_or_else(|| Self::default_selection(vm))
    }

    /// Render the selected object's cards in stack order. The caller supplies
    /// the current y position after the object-level properties and receives
    /// the next y position for the structural add control.
    pub(crate) fn build_object_modifier_cards(
        &mut self,
        tree: &mut UITree,
        inner_x: f32,
        inner_w: f32,
        mut cy: f32,
        object_id: u32,
    ) -> f32 {
        for card in &mut self.object_modifier_cards {
            card.clear_nodes();
            let Some(address) = card.object_modifier_info() else { continue };
            if address.object_id != object_id {
                continue;
            }
            let h = card.compute_height().max(1.0);
            card.build(tree, Rect::new(inner_x, cy, inner_w, h));
            let selected = self
                .selected_object_modifier
                .as_ref()
                .is_some_and(|selected| card.object_modifier_info() == Some(selected));
            card.update_selection_visual(tree, selected);
            cy += h + ROW_GAP;
        }
        cy
    }

    fn card_index_for_node(&self, node_id: NodeId) -> Option<usize> {
        let index = node_id.index();
        self.object_modifier_cards.iter().position(|card| {
            let first = card.first_node();
            card.node_count() > 0 && index >= first && index < first + card.node_count()
        })
    }

    pub(crate) fn object_modifier_card_click(
        &mut self,
        node_id: NodeId,
        tree: &mut UITree,
    ) -> Option<Vec<PanelAction>> {
        let index = self.card_index_for_node(node_id)?;
        self.selected_object_modifier = self.object_modifier_cards[index]
            .object_modifier_info()
            .cloned();
        for (card_index, card) in self.object_modifier_cards.iter_mut().enumerate() {
            card.update_selection_visual(tree, card_index == index);
        }
        if self.object_modifier_cards[index].chevron_node_id() == Some(node_id) {
            let collapsed = !self.object_modifier_cards[index].is_collapsed();
            self.object_modifier_cards[index].set_collapsed(collapsed);
            return Some(vec![PanelAction::Params(ParamsAction::SectionFoldToggled)]);
        }
        Some(self.object_modifier_cards[index].handle_click(node_id, tree))
    }

    pub(crate) fn object_modifier_card_pointer_down(
        &mut self,
        node_id: NodeId,
        pos: Vec2,
        tree: &mut UITree,
    ) -> Option<Vec<PanelAction>> {
        let index = self.card_index_for_node(node_id)?;
        self.selected_object_modifier = self.object_modifier_cards[index]
            .object_modifier_info()
            .cloned();
        for (card_index, card) in self.object_modifier_cards.iter_mut().enumerate() {
            card.update_selection_visual(tree, card_index == index);
        }
        self.object_modifier_pressed_card = Some(index);
        Some(self.object_modifier_cards[index].handle_pointer_down(node_id, pos, tree))
    }

    pub(crate) fn object_modifier_card_double_click(
        &self,
        node_id: NodeId,
        tree: &UITree,
    ) -> Option<PanelAction> {
        let index = self.card_index_for_node(node_id)?;
        self.object_modifier_cards[index].value_cell_typein(node_id, tree)
    }

    pub(crate) fn object_modifier_card_drag(
        &mut self,
        pos: Vec2,
        tree: &mut UITree,
        fine: bool,
    ) -> Vec<PanelAction> {
        if let Some(drag) = self.object_modifier_drag.as_ref() {
            let Some(source) = self.card_index_for_address(&drag.identity) else {
                return Vec::new();
            };
            if !drag.valid {
                return Vec::new();
            }
            return self.object_modifier_cards[source].handle_drag(pos, tree, fine);
        }
        self.object_modifier_pressed_card
            .and_then(|index| self.object_modifier_cards.get_mut(index))
            .map(|card| card.handle_drag(pos, tree, fine))
            .unwrap_or_default()
    }

    pub(crate) fn end_object_modifier_card_gesture(
        &mut self,
        tree: &mut UITree,
    ) -> Vec<PanelAction> {
        let Some(index) = self.object_modifier_pressed_card.take() else {
            return Vec::new();
        };
        self.object_modifier_cards
            .get_mut(index)
            .map(|card| card.handle_drag_end(tree))
            .unwrap_or_default()
    }

    pub fn begin_object_modifier_drag(
        &mut self,
        node_id: Option<NodeId>,
        tree: &mut UITree,
    ) -> bool {
        let Some(node_id) = node_id else { return false };
        let Some(source) = self
            .object_modifier_cards
            .iter()
            .position(|card| card.is_drag_handle(node_id)) else { return false };
        let Some(address) = self.object_modifier_cards[source].object_modifier_info().cloned() else {
            return false;
        };
        self.object_modifier_pressed_card = None;
        self.object_modifier_cards[source].set_drag_dimmed(tree, true);
        let source_local = self.local_card_index(address.object_id, source);
        self.object_modifier_drag = Some(ObjectModifierDrag {
            target: source_local,
            valid: true,
            identity: address,
            pointer: None,
        });
        self.rebuild_object_modifier_drag_overlay(tree);
        true
    }

    pub(crate) fn rebuild_object_modifier_drag_overlay(&mut self, tree: &mut UITree) {
        self.object_modifier_drag_indicator = None;
        if self.object_modifier_drag.is_none() { return; }
        let indicator = tree.add_panel(
            Some(self.content_parent),
            self.panel_rect.x,
            -100.0,
            self.panel_rect.width,
            2.0,
            UIStyle {
                bg_color: color::INSPECTOR_ACCENT,
                corner_radius: color::HAIRLINE_RADIUS,
                ..UIStyle::default()
            },
        );
        self.object_modifier_drag_indicator = Some(indicator);
        if let Some(pos) = self.object_modifier_drag.as_ref().and_then(|drag| drag.pointer) {
            self.update_object_modifier_drag(pos, tree, self.object_modifier_bounds);
        }
    }

    pub fn update_object_modifier_drag(
        &mut self,
        pos: Vec2,
        tree: &mut UITree,
        bounds: Rect,
    ) {
        let Some(drag) = self.object_modifier_drag.as_mut() else { return; };
        let object_id = drag.identity.object_id;
        drag.pointer = Some(pos);
        let mut top = f32::INFINITY;
        let mut bottom = f32::NEG_INFINITY;
        let mut target = 0;
        let mut indicator_y = bounds.y;
        let mut found = false;
        for card in &self.object_modifier_cards {
            if card.object_modifier_info().is_none_or(|address| address.object_id != object_id) { continue; }
            let Some(rect) = card.live_bounds(tree) else { continue; };
            top = top.min(rect.y);
            bottom = bottom.max(rect.y + rect.height);
            if !found {
                indicator_y = rect.y;
                if pos.y < rect.y + rect.height * 0.5 { found = true; }
                else { target += 1; indicator_y = rect.y + rect.height; }
            }
        }
        // The outliner and properties above the stack are not drop targets.
        drag.valid = bounds.contains(pos) && pos.y >= top - ROW_GAP && pos.y <= bottom + ROW_GAP;
        drag.target = target;
        if let Some(indicator) = self.object_modifier_drag_indicator {
            let rect = if drag.valid { Rect::new(bounds.x + PAD, indicator_y - 1.0, (bounds.width - PAD * 2.0).max(0.0), 2.0) }
                else { Rect::new(0.0, -100.0, 0.0, 0.0) };
            tree.set_bounds(indicator, rect);
        }
    }

    pub fn end_object_modifier_drag(
        &mut self,
        tree: &mut UITree,
    ) -> Vec<PanelAction> {
        let Some(drag) = self.object_modifier_drag.take() else { return Vec::new() };
        if let Some(indicator) = self.object_modifier_drag_indicator.take() {
            tree.set_bounds(indicator, Rect::new(0.0, -100.0, 0.0, 0.0));
        }
        let Some(source) = self.card_index_for_address(&drag.identity) else {
            return Vec::new();
        };
        self.object_modifier_cards[source].set_drag_dimmed(tree, false);
        if !drag.valid {
            return Vec::new();
        }
        let source_local = self.local_card_index(drag.identity.object_id, source);
        let target = drag.target.min(self.owner_card_indices(drag.identity.object_id).len());
        if target == source_local || target == source_local + 1 {
            let _ = self.object_modifier_cards[source].handle_drag_end(tree);
            return Vec::new();
        }
        let new_position = target.saturating_sub(usize::from(target > source_local));
        let _ = self.object_modifier_cards[source].handle_drag_end(tree);
        vec![PanelAction::Project(ProjectAction::SceneSetupMoveModifier(
            drag.identity.layer_id,
            drag.identity.group_node_id.unwrap_or(drag.identity.object_id),
            drag.identity.node_doc_id,
            new_position as u32,
        ))]
    }

    /// Revalidate the terminal pointer position before committing. PointerUp
    /// can arrive after the last Drag sample and release-outside must cancel
    /// the reorder rather than reusing a stale in-bounds target.
    pub fn finish_object_modifier_drag(
        &mut self,
        pos: Vec2,
        tree: &mut UITree,
        bounds: Rect,
    ) -> Vec<PanelAction> {
        if self.object_modifier_drag.is_some() {
            self.update_object_modifier_drag(pos, tree, bounds);
        }
        self.end_object_modifier_drag(tree)
    }

    pub fn cancel_object_modifier_drag(&mut self, tree: &mut UITree) -> bool {
        let active = self.object_modifier_drag.is_some() || self.object_modifier_pressed_card.is_some();
        if let Some(drag) = self.object_modifier_drag.take()
            && let Some(source) = self.card_index_for_address(&drag.identity)
        {
            let _ = self.object_modifier_cards[source].handle_drag_end(tree);
            self.object_modifier_cards[source].set_drag_dimmed(tree, false);
        }
        if let Some(indicator) = self.object_modifier_drag_indicator.take() {
            tree.set_bounds(indicator, Rect::new(0.0, -100.0, 0.0, 0.0));
        }
        if let Some(index) = self.object_modifier_pressed_card.take()
            && let Some(card) = self.object_modifier_cards.get_mut(index)
        {
            let _ = card.handle_drag_end(tree);
        }
        active
    }

    /// Shared card motion and edge scrolling use the existing scene scroll host.
    pub fn tick_object_cards(&mut self, tree: &mut UITree, dt_ms: f32) {
        if !self.open { self.object_cards_animating = false; return; }
        let mut any = false;
        for card in &mut self.object_modifier_cards {
            if card.node_count() == 0 { continue; }
            any |= card.tick_drawers(dt_ms);
            card.tick_value_flash(tree, dt_ms);
        }
        self.object_cards_animating = any || self.object_cards_were_animating;
        self.object_cards_were_animating = any;
        if let Some(pos) = self.object_modifier_drag.as_ref().and_then(|drag| drag.pointer) {
            let bounds = self.object_modifier_bounds;
            if bounds.contains(pos) {
                let edge = 32.0;
                let speed = if pos.y < bounds.y + edge { 1.0 }
                    else if pos.y > bounds.y + bounds.height - edge { -1.0 } else { 0.0 };
                let before = self.scroll.scroll_offset();
                if speed != 0.0 && self.scroll.apply_scroll_delta(speed * 360.0 * dt_ms.min(50.0) / 1000.0) {
                    let delta = before - self.scroll.scroll_offset();
                    self.scroll.offset_content(tree, delta);
                    self.scroll.update_scrollbar(tree);
                    self.update_object_modifier_drag(pos, tree, bounds);
                }
            }
        }
    }

    pub fn object_cards_animating(&self) -> bool { self.object_cards_animating }

    pub fn update_object_card_fire_meters(&self, tree: &mut UITree, fire_level: &dyn Fn(u64) -> Option<f32>, dt: f32) {
        for card in &self.object_modifier_cards {
            if card.node_count() > 0 { card.update_fire_meters(tree, fire_level, dt); }
        }
    }

    fn card_index_for_address(&self, address: &ObjectModifierCardInfo) -> Option<usize> {
        self.object_modifier_cards.iter().position(|card| {
            card.object_modifier_info() == Some(address)
        })
    }

    pub fn contains_object_modifier_node(&self, node_id: NodeId) -> bool {
        self.card_index_for_node(node_id).is_some()
    }

    pub fn audio_drawer_intent(
        &mut self,
        node_id: NodeId,
        target: GraphParamTarget,
        param_id: &manifold_foundation::ParamId,
        click: AudioDrawerClick,
    ) -> Vec<PanelAction> {
        let Some(index) = self.card_index_for_node(node_id) else { return Vec::new() };
        self.object_modifier_cards[index].audio_drawer_intent(target, param_id, click)
    }

    pub fn refresh_driver_period_intent(
        &self,
        node_id: NodeId,
        tree: &UITree,
        action: PanelAction,
    ) -> PanelAction {
        if !matches!(
            &action,
            PanelAction::Root(RootAction::BeginDriverPeriodTextInput { .. })
        ) {
            return action;
        }
        self.card_index_for_node(node_id)
            .and_then(|index| self.object_modifier_cards[index].driver_period_typein(node_id, tree))
            .unwrap_or(action)
    }

    fn owner_card_indices(&self, object_id: u32) -> Vec<usize> {
        self.object_modifier_cards
            .iter()
            .enumerate()
            .filter_map(|(index, card)| {
                (card.object_modifier_info()?.object_id == object_id).then_some(index)
            })
            .collect()
    }

    fn local_card_index(&self, object_id: u32, card_index: usize) -> usize {
        self.owner_card_indices(object_id)
            .iter()
            .position(|index| *index == card_index)
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::panels::param_card::{ParamCardKind, RelightCardConfig};
    use crate::param_surface::{ParamRow, RowMapping, RowSpec, RowValue};
    use crate::tree::UITree;

    fn surface(name: &str, id: &str) -> ParamSurface {
        ParamSurface {
            kind: ParamCardKind::Effect,
            effect_index: 0,
            effect_id: manifold_foundation::EffectId::new(format!("object-modifier-{id}")),
            title: name.to_string(),
            enabled: true,
            collapsed: false,
            supports_envelopes: true,
            string_params: Vec::new(),
            layer_id: None,
            modifier: None,
            rows: vec![ParamRow {
                id: std::borrow::Cow::Owned(id.to_string()),
                spec: RowSpec {
                    name: "Amount".into(),
                    min: 0.0,
                    max: 1.0,
                    default: 0.5,
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
                value: RowValue { base: 0.5, effective: 0.5, exposed: true, driven: false },
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
            has_graph_mod: false,
            audio_sends: Vec::new(),
            relight: RelightCardConfig::default(),
        }
    }

    fn address(object_id: u32, node_doc_id: u32) -> ObjectModifierCardInfo {
        ObjectModifierCardInfo {
            layer_id: LayerId::new("layer-1"),
            object_id,
            group_node_id: Some(42),
            node_doc_id,
        }
    }

    #[test]
    fn object_modifier_adapter_uses_shared_rows_and_slim_chrome() {
        let mut card = ParamCardPanel::new();
        let addr = address(7, 70);
        card.configure_object_modifier(&surface("Bend", "70_amount"), addr.clone());
        let mut tree = UITree::new();
        card.build(&mut tree, Rect::new(0.0, 0.0, 320.0, card.compute_height()));

        assert_eq!(card.object_modifier_info(), Some(&addr));
        assert_eq!(card.rows[0].id.as_ref(), "70_amount");
        assert!(card.toggle_node_id().is_none(), "object cards hide enable chrome");
        assert!(card.modifier_objects_node_id().is_none(), "object cards hide OBJ chrome");
        assert!(card.relight_node_id().is_none(), "object cards hide relight chrome");
        assert!(card.modifier_remove_node_id().is_some(), "object cards retain remove");
    }

    #[test]
    fn object_modifier_without_rows_keeps_its_header() {
        let mut card = ParamCardPanel::new();
        let mut config = surface("Empty", "unused");
        config.rows.clear();
        card.configure_object_modifier(&config, address(7, 72));
        let mut tree = UITree::new();
        card.build(&mut tree, Rect::new(0.0, 0.0, 320.0, card.compute_height()));

        assert!(card.node_count() > 0);
        assert!(card.chevron_node_id().is_some());
        assert!(card.modifier_remove_node_id().is_some());
    }

    #[test]
    fn object_modifier_collapse_is_local_and_requests_rebuild() {
        let mut panel = ScenePanel::new();
        let addr = address(7, 72);
        panel.configure_object_modifier_cards(&[(surface("Bend", "72_amount"), addr)]);
        let mut tree = UITree::new();
        let height = panel.object_modifier_cards[0].compute_height();
        panel.object_modifier_cards[0].build(
            &mut tree,
            Rect::new(0.0, 0.0, 320.0, height),
        );
        let chevron = panel.object_modifier_cards[0].chevron_node_id().unwrap();
        let actions = panel.object_modifier_card_click(chevron, &mut tree).unwrap();

        assert!(panel.object_modifier_cards[0].is_collapsed());
        assert!(matches!(
            actions.as_slice(),
            [PanelAction::Params(ParamsAction::SectionFoldToggled)]
        ));
    }

    #[test]
    fn object_modifier_remove_preserves_scene_addressing() {
        let mut card = ParamCardPanel::new();
        card.configure_object_modifier(&surface("Twist", "71_amount"), address(8, 71));
        let mut tree = UITree::new();
        card.build(&mut tree, Rect::new(0.0, 0.0, 320.0, card.compute_height()));
        let actions = card.handle_click(card.modifier_remove_node_id().unwrap(), &tree);
        assert!(matches!(
            actions.as_slice(),
            [PanelAction::Project(ProjectAction::SceneSetupRemoveModifier(layer, 42, 71))]
                if *layer == LayerId::new("layer-1")
        ));
    }

    #[test]
    fn object_modifier_drag_commits_existing_move_action_and_cancels_outside() {
        let mut panel = ScenePanel::new();
        let first = address(7, 70);
        let second = address(7, 71);
        panel.configure_object_modifier_cards(&[
            (surface("Bend", "70_amount"), first.clone()),
            (surface("Twist", "71_amount"), second),
        ]);
        panel.object_modifier_drag = Some(ObjectModifierDrag {
            target: 2,
            valid: true,
            identity: first.clone(),
            pointer: None,
        });
        let mut tree = UITree::new();
        let actions = panel.end_object_modifier_drag(&mut tree);
        assert!(matches!(
            actions.as_slice(),
            [PanelAction::Project(ProjectAction::SceneSetupMoveModifier(layer, 42, 70, 1))]
                if *layer == LayerId::new("layer-1")
        ));

        panel.object_modifier_drag = Some(ObjectModifierDrag {
            target: 2,
            valid: false,
            identity: first,
            pointer: None,
        });
        assert!(panel.end_object_modifier_drag(&mut tree).is_empty());
    }

    #[test]
    fn object_modifier_reorder_uses_owner_local_positions() {
        let mut panel = ScenePanel::new();
        let before = address(99, 10);
        let first = address(7, 70);
        let second = address(7, 71);
        panel.configure_object_modifier_cards(&[
            (surface("Other", "10_amount"), before),
            (surface("First", "70_amount"), first.clone()),
            (surface("Second", "71_amount"), second),
        ]);
        panel.object_modifier_drag = Some(ObjectModifierDrag {
            target: 2,
            valid: true,
            identity: first,
            pointer: None,
        });
        let mut tree = UITree::new();
        let actions = panel.end_object_modifier_drag(&mut tree);

        assert!(matches!(
            actions.as_slice(),
            [PanelAction::Project(ProjectAction::SceneSetupMoveModifier(layer, 42, 70, 1))]
                if *layer == LayerId::new("layer-1")
        ));
    }

    #[test]
    fn object_modifier_finish_recomputes_latest_release_position_and_bounds() {
        let mut panel = ScenePanel::new();
        let first = address(7, 70);
        let second = address(7, 71);
        panel.configure_object_modifier_cards(&[
            (surface("First", "70_amount"), first.clone()),
            (surface("Second", "71_amount"), second),
        ]);
        let mut tree = UITree::new();
        let first_height = panel.object_modifier_cards[0].compute_height();
        let second_height = panel.object_modifier_cards[1].compute_height();
        panel.object_modifier_cards[0].build(
            &mut tree,
            Rect::new(0.0, 0.0, 320.0, first_height),
        );
        panel.object_modifier_cards[1].build(
            &mut tree,
            Rect::new(0.0, 80.0, 320.0, second_height),
        );
        panel.object_modifier_drag = Some(ObjectModifierDrag {
            target: 0,
            valid: true,
            identity: first.clone(),
            pointer: None,
        });
        let actions = panel.finish_object_modifier_drag(
            Vec2::new(10.0, 150.0),
            &mut tree,
            Rect::new(0.0, 0.0, 320.0, 200.0),
        );
        assert!(matches!(
            actions.as_slice(),
            [PanelAction::Project(ProjectAction::SceneSetupMoveModifier(layer, 42, 70, 1))]
                if *layer == LayerId::new("layer-1")
        ));

        panel.object_modifier_drag = Some(ObjectModifierDrag {
            target: 1,
            valid: true,
            identity: first,
            pointer: None,
        });
        assert!(panel
            .finish_object_modifier_drag(
                Vec2::new(500.0, 150.0),
                &mut tree,
                Rect::new(0.0, 0.0, 320.0, 200.0),
            )
            .is_empty());
    }
}
