use super::*;

impl InspectorCompositePanel {
    pub fn selection_has_group(&self) -> bool {
        let selected = self.selection_for_tab(self.last_effect_tab).0;
        self.rack_groups[Self::scope_idx(self.last_effect_tab)].iter().any(|group| group.member_ids.iter().any(|id| selected.contains(id)))
    }

    pub fn selected_effect_ids(&self) -> Vec<EffectId> {
        let (selected, cards) = self.selection_for_tab(self.last_effect_tab);
        cards.iter().filter(|card| selected.contains(card.effect_id())).map(|card| card.effect_id().clone()).collect()
    }

    pub fn select_modifier_ids(&mut self, layer: &LayerId, ids: &[manifold_foundation::NodeId]) {
        if self.modifier_scope_id.as_ref() != Some(layer) { return; }
        self.selected_master_ids.clear();
        self.selected_layer_ids.clear();
        self.selected_modifier_ids.clear();
        self.selected_modifier_ids.extend(ids.iter().cloned());
        self.last_clicked_modifier = ids.last().cloned();
        self.pending_reveal = self.modifier_cards.iter().find(|card|
            card.modifier_info().is_some_and(|info| ids.first() == Some(&info.instance_id)))
            .map(|card| card.effect_id().clone());
    }

    pub fn select_modifier_for_context_menu(&mut self, layer: &LayerId, id: &manifold_foundation::NodeId) {
        if self.modifier_scope_id.as_ref() != Some(layer) { return; }
        if !self.selected_modifier_ids.contains(id)
            && let Some(index) = self.modifier_cards.iter().position(|card| card.modifier_info().is_some_and(|info| info.instance_id == *id)) {
            self.select_modifier(index);
        }
    }

    pub fn select_effect_ids(&mut self, tab: InspectorTab, ids: &[EffectId]) {
        self.selected_master_ids.clear();
        self.selected_layer_ids.clear();
        self.selected_modifier_ids.clear();
        self.selection_set_mut(tab).extend(ids.iter().cloned());
        self.last_effect_tab = tab;
        self.pending_reveal = ids.first().cloned();
    }

    pub fn reveal_pending_selection(&mut self, tree: &mut UITree) {
        let Some(id) = self.pending_reveal.as_ref() else { return; };
        let group_bounds = self.rack_groups[Self::scope_idx(self.active_tab)].iter()
            .find(|group| group.collapsed && group.member_ids.contains(id))
            .and_then(|group| self.group_nodes.iter().find(|nodes| nodes.group_id == group.id))
            .map(|nodes| tree.get_bounds(nodes.header));
        let bounds = group_bounds.or_else(|| self.cards_for_tab(self.active_tab).iter()
            .chain(self.modifier_cards.iter()).find(|card| card.effect_id() == id)
            .and_then(|card| card.live_bounds(tree)));
        let Some(bounds) = bounds else { return; };
        let scroll = if self.active_tab == InspectorTab::Master { &mut self.master_scroll } else { &mut self.layer_scroll };
        self.scrolled_in_place |= scroll.reveal_rect(tree, bounds);
        self.pending_reveal = None;
    }

    pub fn inspected_layer_id(&self) -> Option<&LayerId> {
        self.inspecting_layer_id.as_ref()
    }

    pub fn select_effect_for_context_menu(&mut self, index: usize) {
        let tab = self.active_tab;
        let Some(card_index) = self.cards_for_tab(tab).iter().position(|card| card.effect_index() == index) else { return; };
        let id = self.cards_for_tab(tab)[card_index].effect_id();
        if !self.selection_for_tab(tab).0.contains(id) {
            self.select_effect(tab, card_index);
        }
        self.last_effect_tab = tab;
    }


    pub fn select_rack_group(&mut self, group_id: &EffectGroupId, additive: bool) {
        let tab = self.active_tab;
        let Some(members) = self.rack_groups[Self::scope_idx(tab)].iter()
            .find(|group| group.id == *group_id).map(|group| group.member_ids.clone()) else { return; };
        self.selected_modifier_ids.clear();
        if !additive {
            self.selected_master_ids.clear();
            self.selected_layer_ids.clear();
        }
        let selection = self.selection_set_mut(tab);
        if additive && members.iter().all(|id| selection.contains(id)) {
            selection.retain(|id| !members.contains(id));
        } else {
            selection.extend(members);
        }
        self.last_effect_tab = tab;
    }

    /// Get the selection set and cards vec for a given tab.
    pub(super) fn selection_for_tab(&self, tab: InspectorTab) -> (&HashSet<EffectId>, &[ParamCardPanel]) {
        let set = match tab {
            InspectorTab::Master => &self.selected_master_ids,
            InspectorTab::Layer | InspectorTab::Group | InspectorTab::Clip => &self.selected_layer_ids,
        };
        (set, self.cards_for_tab(tab))
    }

    pub(super) fn selection_set_mut(&mut self, tab: InspectorTab) -> &mut HashSet<EffectId> {
        match tab {
            InspectorTab::Master => &mut self.selected_master_ids,
            InspectorTab::Layer | InspectorTab::Group | InspectorTab::Clip => &mut self.selected_layer_ids,
        }
    }

    /// Unity EffectSelectionManager.OnCardClicked (lines 164-177)
    /// Dispatches to select/toggle/range based on modifiers.
    fn on_effect_card_clicked(
        &mut self,
        tab: InspectorTab,
        card_index: usize,
        modifiers: Modifiers,
    ) {
        let cmd = modifiers.ctrl || modifiers.command;
        let shift = modifiers.shift;

        if shift {
            self.range_select_effects(tab, card_index);
        } else if cmd {
            self.toggle_effect_selection(tab, card_index);
        } else {
            self.select_effect(tab, card_index);
        }
    }

    fn on_modifier_card_clicked(&mut self, card_index: usize, modifiers: Modifiers) {
        let Some(id) = self
            .modifier_cards
            .get(card_index)
            .and_then(ParamCardPanel::modifier_info)
            .map(|info| info.instance_id.clone())
        else {
            return;
        };
        let cmd = modifiers.ctrl || modifiers.command;
        let shift = modifiers.shift;
        if shift {
            self.range_select_modifiers(card_index);
        } else if cmd {
            self.toggle_modifier_selection(card_index);
        } else {
            self.select_modifier(card_index);
        }
        self.last_clicked_modifier = Some(id);
    }

    pub(super) fn select_modifier(&mut self, card_index: usize) {
        let Some(id) = self
            .modifier_cards
            .get(card_index)
            .and_then(ParamCardPanel::modifier_info)
            .map(|info| info.instance_id.clone())
        else {
            return;
        };
        self.selected_master_ids.clear();
        self.selected_layer_ids.clear();
        self.selected_modifier_ids.clear();
        self.selected_modifier_ids.insert(id.clone());
        self.last_clicked_modifier = Some(id);
    }

    fn toggle_modifier_selection(&mut self, card_index: usize) {
        let Some(id) = self
            .modifier_cards
            .get(card_index)
            .and_then(ParamCardPanel::modifier_info)
            .map(|info| info.instance_id.clone())
        else {
            return;
        };
        self.selected_master_ids.clear();
        self.selected_layer_ids.clear();
        if !self.selected_modifier_ids.remove(&id) {
            self.selected_modifier_ids.insert(id.clone());
        }
        self.last_clicked_modifier = Some(id);
    }

    fn range_select_modifiers(&mut self, card_index: usize) {
        if card_index >= self.modifier_cards.len() {
            return;
        }
        let anchor = self
            .last_clicked_modifier
            .as_ref()
            .and_then(|id| {
                self.modifier_cards.iter().position(|card| {
                    card.modifier_info().is_some_and(|info| &info.instance_id == id)
                })
            })
            .unwrap_or(0);
        let lo = anchor.min(card_index);
        let hi = anchor.max(card_index);
        self.selected_master_ids.clear();
        self.selected_layer_ids.clear();
        self.selected_modifier_ids.clear();
        for card in &self.modifier_cards[lo..=hi] {
            if let Some(info) = card.modifier_info() {
                self.selected_modifier_ids.insert(info.instance_id.clone());
            }
        }
    }

    /// Unity EffectSelectionManager.SelectCard (lines 89-100)
    /// Select a single card, clearing all others across ALL tabs.
    /// Note: does NOT update card visuals — call apply_selection_visuals() after.
    pub(super) fn select_effect(&mut self, tab: InspectorTab, card_index: usize) {
        let cards = self.cards_for_tab(tab);
        if card_index >= cards.len() {
            return;
        }
        let id = cards[card_index].effect_id().clone();

        // Clear all tabs so only one card is selected globally
        self.selected_master_ids.clear();
        self.selected_layer_ids.clear();
        self.selected_modifier_ids.clear();
        self.last_clicked_modifier = None;

        let set = self.selection_set_mut(tab);
        set.insert(id.clone());
        self.set_last_clicked_for_tab(tab, Some(id));
    }

    /// Unity EffectSelectionManager.ToggleCardSelection (lines 103-118)
    /// Cmd+Click: toggle in/out of multi-selection.
    /// Note: does NOT update card visuals — call apply_selection_visuals() after.
    fn toggle_effect_selection(&mut self, tab: InspectorTab, card_index: usize) {
        let cards = self.cards_for_tab(tab);
        if card_index >= cards.len() {
            return;
        }
        let id = cards[card_index].effect_id().clone();

        self.selected_modifier_ids.clear();
        self.last_clicked_modifier = None;
        let set = self.selection_set_mut(tab);
        if set.contains(&id) {
            set.remove(&id);
        } else {
            set.insert(id.clone());
        }
        self.set_last_clicked_for_tab(tab, Some(id));
    }

    /// Select all effects in the active tab.
    /// Returns true if any effects were selected.
    pub fn select_all_effects(&mut self) -> bool {
        let tab = self.last_effect_tab;
        let cards = self.cards_for_tab(tab);
        if cards.is_empty() {
            return false;
        }
        let ids: Vec<EffectId> = cards.iter().map(|c| c.effect_id().clone()).collect();
        let first_id = ids[0].clone();

        self.selected_modifier_ids.clear();
        self.last_clicked_modifier = None;
        let set = self.selection_set_mut(tab);
        set.clear();
        for id in ids {
            set.insert(id);
        }
        self.set_last_clicked_for_tab(tab, Some(first_id));
        true
    }

    pub fn select_all_modifiers(&mut self) -> bool {
        if self.modifier_cards.is_empty() {
            return false;
        }
        self.selected_master_ids.clear();
        self.selected_layer_ids.clear();
        self.selected_modifier_ids.clear();
        for card in &self.modifier_cards {
            if let Some(info) = card.modifier_info() {
                self.selected_modifier_ids.insert(info.instance_id.clone());
            }
        }
        self.last_clicked_modifier = self
            .modifier_cards
            .first()
            .and_then(ParamCardPanel::modifier_info)
            .map(|info| info.instance_id.clone());
        !self.selected_modifier_ids.is_empty()
    }

    /// How many effects are selected in the active tab.
    pub fn selected_effect_count(&self) -> usize {
        let (set, _) = self.selection_for_tab(self.last_effect_tab);
        set.len()
    }

    pub fn selected_modifier_count(&self) -> usize {
        self.selected_modifier_ids.len()
    }

    pub fn has_modifier_selection(&self) -> bool {
        !self.selected_modifier_ids.is_empty()
    }

    pub fn selected_modifier_ids(&self) -> Vec<manifold_foundation::NodeId> {
        self.modifier_cards
            .iter()
            .filter_map(ParamCardPanel::modifier_info)
            .filter(|info| self.selected_modifier_ids.contains(&info.instance_id))
            .map(|info| info.instance_id.clone())
            .collect()
    }

    /// Return the selected modifier ids in stack order for a captured menu
    /// target. A menu opened on an unselected card acts on that card alone;
    /// a menu opened on a selected card keeps the complete multi-selection.
    pub fn selected_modifier_ids_for(
        &self,
        layer_id: &LayerId,
        clicked: &manifold_foundation::NodeId,
    ) -> Vec<manifold_foundation::NodeId> {
        let clicked_is_selected = self
            .modifier_cards
            .iter()
            .filter_map(ParamCardPanel::modifier_info)
            .any(|info| &info.layer_id == layer_id
                && &info.instance_id == clicked
                && self.selected_modifier_ids.contains(&info.instance_id));
        self.modifier_cards
            .iter()
            .filter_map(ParamCardPanel::modifier_info)
            .filter(|info| &info.layer_id == layer_id)
            .filter(|info| !clicked_is_selected || self.selected_modifier_ids.contains(&info.instance_id))
            .filter(|info| clicked_is_selected || &info.instance_id == clicked)
            .map(|info| info.instance_id.clone())
            .collect()
    }

    pub fn modifier_has_graph_mod(
        &self,
        layer_id: &LayerId,
        id: &manifold_foundation::NodeId,
    ) -> bool {
        self.modifier_cards.iter().any(|card| {
            card.modifier_info().is_some_and(|info| {
                &info.layer_id == layer_id && &info.instance_id == id
            }) && card.has_graph_mod()
        })
    }

    /// Resolve a double-clicked node to a numeric param's value cell across the
    /// visible cards and return its type-in action (empty if it isn't a value
    /// cell). Enum/toggle params are filtered out by the card itself.
    pub(super) fn route_value_typein(&self, node_id: NodeId, tree: &UITree) -> Vec<PanelAction> {
        // No scope gate: a card that didn't build this frame is not live and its
        // `value_cell_typein` returns None, so only the active scope's cards can
        // match. The card's liveness is the single source of truth.
        for card in &self.effects[Self::SCOPE_MASTER] {
            if let Some(a) = card.value_cell_typein(node_id, tree) {
                return vec![a];
            }
        }
        if let Some(gp) = self.gen_params.as_ref()
            && let Some(a) = gp.value_cell_typein(node_id, tree)
        {
            return vec![a];
        }
        for card in &self.effects[Self::SCOPE_LAYER] {
            if let Some(a) = card.value_cell_typein(node_id, tree) {
                return vec![a];
            }
        }
        for card in &self.modifier_cards {
            if let Some(a) = card.value_cell_typein(node_id, tree) {
                return vec![a];
            }
        }
        Vec::new()
    }

    /// Resolve a clicked node to a driver Free-period field across the visible
    /// cards and return its type-in action (empty if it isn't a Free field).
    pub(super) fn route_driver_period_typein(&self, node_id: NodeId, tree: &UITree) -> Vec<PanelAction> {
        // No scope gate — a non-live card's `driver_period_typein` returns None
        // (see `route_value_typein`).
        for card in &self.effects[Self::SCOPE_MASTER] {
            if let Some(a) = card.driver_period_typein(node_id, tree) {
                return vec![a];
            }
        }
        if let Some(gp) = self.gen_params.as_ref()
            && let Some(a) = gp.driver_period_typein(node_id, tree)
        {
            return vec![a];
        }
        for card in &self.effects[Self::SCOPE_LAYER] {
            if let Some(a) = card.driver_period_typein(node_id, tree) {
                return vec![a];
            }
        }
        for card in &self.modifier_cards {
            if let Some(a) = card.driver_period_typein(node_id, tree) {
                return vec![a];
            }
        }
        Vec::new()
    }

    pub(super) fn find_target_for_node(&self, node_id: NodeId) -> Option<PressedTarget> {
        let idx = node_id.index();
        // Macros panel (above both columns)
        if self.macros_panel.owns_node(node_id) {
            return Some(PressedTarget::Macros);
        }
        // AUDIO TRIGGERS section (top of the layer column's content)
        if self.audio_trigger_section.owns_node(node_id) {
            return Some(PressedTarget::AudioTriggers);
        }

        // Scrollbars
        if Some(node_id) == self.master_scroll.track_id()
            || Some(node_id) == self.master_scroll.thumb_id()
            || Some(node_id) == self.layer_scroll.track_id()
            || Some(node_id) == self.layer_scroll.thumb_id()
        {
            return Some(PressedTarget::Scrollbar);
        }

        // Every section below is matched purely by its node range. Only the
        // active scope built nodes this frame; the rest were reset to empty
        // ranges in `build`, and `in_range` is false for an empty range — so no
        // `*_visible()` gate is needed and an inactive scope can't match a live
        // index. The node range is the single source of truth.

        // Master section
        if in_range(
            idx,
            self.master_chrome.first_node(),
            self.master_chrome.node_count(),
        ) {
            return Some(PressedTarget::MasterChrome);
        }
        for (i, card) in self.effects[Self::SCOPE_MASTER].iter().enumerate() {
            if in_range(idx, card.first_node(), card.node_count()) {
                return Some(PressedTarget::MasterEffect(i));
            }
        }

        // Layer section. The generator card lives here (built and range-registered
        // alongside the layer chrome), so it is hit-tested here — not under the
        // clip section, which is a different scope.
        if in_range(
            idx,
            self.layer_chrome.first_node(),
            self.layer_chrome.node_count(),
        ) {
            return Some(PressedTarget::LayerChrome);
        }
        if let Some(ref gp) = self.gen_params
            && in_range(idx, gp.first_node(), gp.node_count())
        {
            return Some(PressedTarget::GenParam);
        }
        for (i, card) in self.modifier_cards.iter().enumerate() {
            if in_range(idx, card.first_node(), card.node_count()) {
                return Some(PressedTarget::Modifier(i));
            }
        }
        for (i, card) in self.effects[Self::SCOPE_LAYER].iter().enumerate() {
            if in_range(idx, card.first_node(), card.node_count()) {
                return Some(PressedTarget::LayerEffect(i));
            }
        }

        // Clip section
        if in_range(
            idx,
            self.clip_chrome.first_node(),
            self.clip_chrome.node_count(),
        ) {
            return Some(PressedTarget::ClipChrome);
        }

        None
    }

    pub(super) fn cards_for_tab(&self, tab: InspectorTab) -> &[ParamCardPanel] {
        &self.effects[Self::scope_idx(tab)]
    }

    pub(super) fn cards_for_tab_mut(&mut self, tab: InspectorTab) -> &mut Vec<ParamCardPanel> {
        &mut self.effects[Self::scope_idx(tab)]
    }

    pub(super) fn route_click(&mut self, node_id: NodeId, modifiers: Modifiers, tree: &UITree) -> Vec<PanelAction> {
        if let Some(nodes) = self.group_nodes.iter().find(|nodes| nodes.collapse == node_id) {
            let group = self.rack_groups[Self::scope_idx(self.active_tab)].iter().find(|group| group.id == nodes.group_id);
            return group.map(|group| vec![PanelAction::Params(ParamsAction::EffectGroupCollapsed {
                tab: self.active_tab, layer_id: self.inspecting_layer_id.clone(),
                group_id: group.id.clone(), collapsed: !group.collapsed,
            })]).unwrap_or_default();
        }
        if self.group_nodes.iter().any(|nodes| nodes.header == node_id) {
            return Vec::new(); // Selection is established on press, before drag capture.
        }
        // Tab strip — selecting a tab mirrors the timeline selection.
        if let Some((_, tab)) = self.tab_node_ids.iter().find(|(id, _)| *id == node_id) {
            let tab = *tab;
            self.set_active_tab(tab);
            return vec![PanelAction::Root(RootAction::SelectInspectorTab(tab))];
        }
        // Collapse-all / expand-all — resolve the target state from the active
        // column's current cards (collapse if any open, else expand).
        if self.collapse_all_btn_id == Some(node_id) {
            let collapsed = self.any_active_card_expanded();
            return vec![PanelAction::Params(ParamsAction::SetAllCardsCollapsed { collapsed })];
        }
        // section 6b — compact toggle: flip global mod-drawer visibility (UI-only). Flip
        // here and return a structural no-op so the inspector rebuilds with the
        // new state propagated to every card.
        if self.compact_toggle_btn_id == Some(node_id) {
            self.mods_compact = !self.mods_compact;
            return vec![PanelAction::Params(ParamsAction::ModsCompactToggled)];
        }
        // Add Effect buttons. The action carries the invocation context
        // the button rendered from — the tab plus the inspected layer's
        // id (foundation type; the app dispatch layer builds the editing
        // crate's EffectTarget from it at pick time, PRESET_BROWSER_AUDITION
        // D2/§3.3). Never re-resolved from the active layer.
        if self.add_master_effect_btn == Some(node_id) {
            return vec![PanelAction::Params(ParamsAction::AddEffectClicked {
                tab: InspectorTab::Master,
                layer_id: None,
            })];
        }
        if self.add_layer_effect_btn == Some(node_id) {
            // No inspecting layer ⇒ the layer scope (and its button) isn't
            // rendered; a click here is unaddressable, so emit nothing —
            // the same outcome as today's pick-time bail.
            return self
                .inspecting_layer_id
                .clone()
                .map(|layer_id| {
                    PanelAction::Params(ParamsAction::AddEffectClicked {
                        tab: InspectorTab::Layer,
                        layer_id: Some(layer_id),
                    })
                })
                .into_iter()
                .collect();
        }
        // SCENE_MODIFIER_FRAMEWORK section 3.7: the modifier picker — the
        // owning layer rides on the action; the app builds the kind list
        // (applied/inapplicable disabled) and opens the typed dropdown.
        if self.add_modifier_btn == Some(node_id) {
            return match self.modifier_scope_id.clone() {
                Some(lid) => vec![PanelAction::Params(ParamsAction::AddModifierClicked(lid))],
                None => Vec::new(),
            };
        }
        if let Some(target) = self.find_target_for_node(node_id) {
            self.update_last_effect_tab(&target);
            match target {
                PressedTarget::Macros => self.macros_panel.handle_click(node_id),
                PressedTarget::AudioTriggers => self.audio_trigger_section.handle_click(node_id),
                PressedTarget::MasterChrome => self.master_chrome.handle_click(node_id),
                PressedTarget::LayerChrome => self.layer_chrome.handle_click(node_id),
                PressedTarget::ClipChrome => self.clip_chrome.handle_click(node_id),
                PressedTarget::MasterEffect(i) => {
                    let mut actions = self.effects[Self::SCOPE_MASTER]
                        .get_mut(i)
                        .map(|c| c.handle_click(node_id, tree))
                        .unwrap_or_default();

                    if actions
                        .iter()
                        .any(|a| matches!(a, PanelAction::Params(ParamsAction::EffectCardClicked(_))))
                    {
                        self.on_effect_card_clicked(InspectorTab::Master, i, modifiers);
                    } else if !self.is_effect_target_selected(&PressedTarget::MasterEffect(i)) {
                        // Only auto-select if not already in multi-selection
                        self.auto_select_effect(&PressedTarget::MasterEffect(i));
                    }
                    let ei = self.effects[Self::SCOPE_MASTER]
                        .get(i)
                        .map(|c| c.effect_index())
                        .unwrap_or(0);
                    if !actions
                        .iter()
                        .any(|a| matches!(a, PanelAction::Params(ParamsAction::EffectCardClicked(_))))
                    {
                        actions.insert(0, PanelAction::Params(ParamsAction::EffectCardClicked(ei)));
                    }
                    actions
                }
                PressedTarget::LayerEffect(i) => {
                    let mut actions = self.effects[Self::SCOPE_LAYER]
                        .get_mut(i)
                        .map(|c| c.handle_click(node_id, tree))
                        .unwrap_or_default();

                    if actions
                        .iter()
                        .any(|a| matches!(a, PanelAction::Params(ParamsAction::EffectCardClicked(_))))
                    {
                        self.on_effect_card_clicked(InspectorTab::Layer, i, modifiers);
                    } else if !self.is_effect_target_selected(&PressedTarget::LayerEffect(i)) {
                        self.auto_select_effect(&PressedTarget::LayerEffect(i));
                    }
                    let ei = self.effects[Self::SCOPE_LAYER]
                        .get(i)
                        .map(|c| c.effect_index())
                        .unwrap_or(0);
                    if !actions
                        .iter()
                        .any(|a| matches!(a, PanelAction::Params(ParamsAction::EffectCardClicked(_))))
                    {
                        actions.insert(0, PanelAction::Params(ParamsAction::EffectCardClicked(ei)));
                    }
                    actions
                }
                PressedTarget::GenParam => self
                    .gen_params
                    .as_mut()
                    .map(|gp| gp.handle_click(node_id, tree))
                    .unwrap_or_default(),
                // Modifier cards share effect selection and card drag routing,
                // while retaining their own stable-id stack.
                PressedTarget::Modifier(i) => {
                    let actions = self
                        .modifier_cards
                        .get_mut(i)
                        .map(|c| c.handle_click(node_id, tree))
                        .unwrap_or_default();
                    if actions.iter().any(|a| {
                        matches!(a, PanelAction::Params(ParamsAction::ModifierCardClicked(_)))
                    }) {
                        self.on_modifier_card_clicked(i, modifiers);
                    } else if !self.is_card_target_selected(&PressedTarget::Modifier(i)) {
                        self.auto_select_card(&PressedTarget::Modifier(i));
                    }
                    actions
                }
                PressedTarget::Scrollbar => Vec::new(),
            }
        } else {
            Vec::new()
        }
    }

    pub(super) fn route_pointer_down(
        &mut self,
        node_id: NodeId,
        pos: Vec2,
        modifiers: Modifiers,
        tree: &UITree,
    ) -> Vec<PanelAction> {
        if let Some(group_id) = self.group_nodes.iter().find(|nodes| nodes.header == node_id).map(|nodes| nodes.group_id.clone()) {
            self.select_rack_group(&group_id, modifiers.ctrl || modifiers.command);
            return vec![PanelAction::Params(ParamsAction::EffectSelectionChanged)];
        }
        let target = self.find_target_for_node(node_id);
        self.pressed_target = target;
        // Record which tab this interaction targets (survives drag_end)
        if let Some(ref t) = target {
            self.update_last_effect_tab(t);
            // Auto-select on pointer-down ONLY when:
            // 1. No selection modifiers are held (shift/ctrl defer to Click handler)
            // 2. The target is not already selected (preserve multi-selection for
            //    functional buttons like chevron/toggle on selected effects)
            if !modifiers.shift
                && !modifiers.ctrl
                && !modifiers.command
                && !self.is_card_target_selected(t)
            {
                self.auto_select_card(t);
            }
        }

        if let Some(target) = target {
            // For effect targets, prepend EffectCardClicked to trigger visual update
            let select_action = match target {
                PressedTarget::MasterEffect(i) => Some(PanelAction::Params(ParamsAction::EffectCardClicked(
                    self.effects[Self::SCOPE_MASTER]
                        .get(i)
                        .map(|c| c.effect_index())
                        .unwrap_or(0),
                ))),
                PressedTarget::LayerEffect(i) => Some(PanelAction::Params(ParamsAction::EffectCardClicked(
                    self.effects[Self::SCOPE_LAYER]
                        .get(i)
                        .map(|c| c.effect_index())
                        .unwrap_or(0),
                ))),
                _ => None,
            };

            let mut actions = match target {
                PressedTarget::Macros => self.macros_panel.handle_press(node_id, pos.x),
                PressedTarget::AudioTriggers => {
                    self.audio_trigger_section.handle_press(node_id, pos.x, tree)
                }
                PressedTarget::MasterChrome => {
                    self.master_chrome.handle_pointer_down(node_id, pos)
                }
                PressedTarget::LayerChrome => {
                    self.layer_chrome.handle_pointer_down(node_id, pos)
                }
                PressedTarget::ClipChrome => self.clip_chrome.handle_pointer_down(node_id, pos),
                PressedTarget::MasterEffect(i) => self.effects[Self::SCOPE_MASTER]
                    .get_mut(i)
                    .map(|c| c.handle_pointer_down(node_id, pos, tree))
                    .unwrap_or_default(),
                PressedTarget::LayerEffect(i) => self.effects[Self::SCOPE_LAYER]
                    .get_mut(i)
                    .map(|c| c.handle_pointer_down(node_id, pos, tree))
                    .unwrap_or_default(),
                PressedTarget::GenParam => self
                    .gen_params
                    .as_mut()
                    .map(|gp| gp.handle_pointer_down(node_id, pos, tree))
                    .unwrap_or_default(),
                PressedTarget::Modifier(i) => self
                    .modifier_cards
                    .get_mut(i)
                    .map(|c| c.handle_pointer_down(node_id, pos, tree))
                    .unwrap_or_default(),
                PressedTarget::Scrollbar => {
                    self.dragging_scrollbar = true;
                    self.dragging_scrollbar_master = Some(node_id) == self.master_scroll.track_id()
                        || Some(node_id) == self.master_scroll.thumb_id();
                    Vec::new()
                }
            };

            // Prepend EffectCardClicked so dispatch applies selection visuals
            if let Some(sa) = select_action {
                actions.insert(0, sa);
            }
            actions
        } else {
            Vec::new()
        }
    }
}
