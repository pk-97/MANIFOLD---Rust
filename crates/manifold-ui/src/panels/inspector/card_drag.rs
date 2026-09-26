use super::*;

impl InspectorCompositePanel {
    /// Call on mouse wheel within the inspector viewport.
    /// Positive delta scrolls down.
    pub fn handle_scroll(&mut self, delta: f32) {
        self.handle_scroll_at(delta, self.viewport_rect.x + self.viewport_rect.width * 0.5);
    }

    pub fn handle_scroll_at(&mut self, delta: f32, cursor_x: f32) {
        if cursor_x < self.column_split_x {
            self.master_scroll.apply_scroll_delta(delta);
        } else {
            self.layer_scroll.apply_scroll_delta(delta);
        }
    }

    /// Scroll the inspector in place — the cheap path that mirrors how the
    /// timeline viewport scrolls. Applies the delta to the column under the
    /// cursor, then offsets that column's already-built content nodes instead of
    /// triggering a full `ui_root.build()` + whole-atlas clear. The caller
    /// invalidates only the inspector's cache slot afterwards.
    ///
    /// Returns `false` only when there is nothing built to offset yet (the very
    /// first frame), in which case it has NOT touched the scroll offset and the
    /// caller must fall back to `handle_scroll_at` + a rebuild. Once built it
    /// always handles the scroll in place (returning `true`), so the two paths
    /// never both apply the delta.
    pub fn try_scroll_in_place(&mut self, delta: f32, cursor_x: f32, tree: &mut UITree) -> bool {
        if self.bg_panel_id.is_none() {
            return false;
        }
        let moved = {
            let scroll = if cursor_x < self.column_split_x {
                &mut self.master_scroll
            } else {
                &mut self.layer_scroll
            };
            let old = scroll.scroll_offset();
            if !scroll.apply_scroll_delta(delta) {
                // Already at a scroll limit — consumed, nothing moved.
                return true;
            }
            let delta_y = -(scroll.scroll_offset() - old);
            let moved = scroll.offset_content(tree, delta_y);
            if moved {
                scroll.update_scrollbar(tree);
            }
            moved
        };
        if moved {
            self.scrolled_in_place = true;
        }
        true
    }

    /// Whether an effect card reorder drag is in progress.
    pub fn is_card_drag_active(&self) -> bool {
        self.card_drag_active
    }

    /// First node ID of the drag ghost/indicator overlay (for render pass).
    /// Returns None if no drag is active. Reports the wrapping `Ghost`-tier
    /// region's root (not `card_drag_ghost_id`, the label node itself) —
    /// the render pass's `render_tree_range(start, usize::MAX)` walks
    /// registered regions, and the region root sits one index before the
    /// label, so reporting the label's own index would make that walk miss
    /// the region entirely.
    pub fn card_drag_first_node(&self) -> Option<usize> {
        if self.card_drag_active {
            self.card_drag_region_root.map(|id| id.index())
        } else {
            None
        }
    }

    /// Route drag events to the pressed sub-panel.
    /// Called from UIRoot::process_events (not through Panel::handle_event)
    /// because it needs &mut UITree for slider visual feedback.
    /// `fine` is Shift-held, forwarded to the param cards' value drags (D8).
    pub fn handle_drag(&mut self, pos: Vec2, tree: &mut UITree, fine: bool) -> Vec<PanelAction> {
        if self.dragging_scrollbar {
            // Drag the thumb to an absolute offset, then offset the content nodes
            // by the delta — the same in-place scroll the wheel uses. Previously
            // only the thumb moved (the content stayed frozen until some later
            // rebuild), because the drag carried no rebuild trigger.
            let scroll = if self.dragging_scrollbar_master {
                &mut self.master_scroll
            } else {
                &mut self.layer_scroll
            };
            let old = scroll.scroll_offset();
            scroll.drag_to_scroll(pos.y);
            let delta_y = -(scroll.scroll_offset() - old);
            let moved = scroll.offset_content(tree, delta_y);
            scroll.update_scrollbar(tree);
            if moved {
                self.scrolled_in_place = true;
            }
            return vec![PanelAction::Transport(TransportAction::InspectorScrolled(0.0))];
        }

        if let Some(target) = self.pressed_target {
            match target {
                PressedTarget::Macros => self.macros_panel.handle_drag(pos.x, tree),
                PressedTarget::AudioTriggers => self.audio_trigger_section.handle_drag(pos.x, tree),
                PressedTarget::MasterChrome => self.master_chrome.handle_drag(pos, tree),
                PressedTarget::LayerChrome => self.layer_chrome.handle_drag(pos, tree),
                PressedTarget::ClipChrome => self.clip_chrome.handle_drag(pos, tree),
                PressedTarget::MasterEffect(i) => self.effects[Self::SCOPE_MASTER]
                    .get_mut(i)
                    .map(|c| c.handle_drag(pos, tree, fine))
                    .unwrap_or_default(),
                PressedTarget::LayerEffect(i) => self.effects[Self::SCOPE_LAYER]
                    .get_mut(i)
                    .map(|c| c.handle_drag(pos, tree, fine))
                    .unwrap_or_default(),
                PressedTarget::GenParam => self
                    .gen_params
                    .as_mut()
                    .map(|gp| gp.handle_drag(pos, tree, fine))
                    .unwrap_or_default(),
                PressedTarget::Modifier(i) => self
                    .modifier_cards
                    .get_mut(i)
                    .map(|c| c.handle_drag(pos, tree, fine))
                    .unwrap_or_default(),
                PressedTarget::Scrollbar => Vec::new(),
            }
        } else {
            Vec::new()
        }
    }

    /// Route drag-end events to the pressed sub-panel.
    /// Call directly from the app layer (not through Panel::handle_event).
    pub fn handle_drag_end(&mut self, tree: &mut UITree) -> Vec<PanelAction> {
        if self.dragging_scrollbar {
            self.dragging_scrollbar = false;
            self.pressed_target = None;
            return Vec::new();
        }

        let actions = if let Some(target) = self.pressed_target {
            match target {
                PressedTarget::Macros => self.macros_panel.handle_release(),
                PressedTarget::AudioTriggers => self.audio_trigger_section.handle_release(),
                PressedTarget::MasterChrome => self.master_chrome.handle_drag_end(tree),
                PressedTarget::LayerChrome => self.layer_chrome.handle_drag_end(tree),
                PressedTarget::ClipChrome => self.clip_chrome.handle_drag_end(tree),
                PressedTarget::MasterEffect(i) => self.effects[Self::SCOPE_MASTER]
                    .get_mut(i)
                    .map(|c| c.handle_drag_end(tree))
                    .unwrap_or_default(),
                PressedTarget::LayerEffect(i) => self.effects[Self::SCOPE_LAYER]
                    .get_mut(i)
                    .map(|c| c.handle_drag_end(tree))
                    .unwrap_or_default(),
                PressedTarget::GenParam => self
                    .gen_params
                    .as_mut()
                    .map(|gp| gp.handle_drag_end(tree))
                    .unwrap_or_default(),
                PressedTarget::Modifier(i) => self
                    .modifier_cards
                    .get_mut(i)
                    .map(|c| c.handle_drag_end(tree))
                    .unwrap_or_default(),
                PressedTarget::Scrollbar => Vec::new(),
            }
        } else {
            Vec::new()
        };

        self.pressed_target = None;
        actions
    }

    /// Try to begin a shared effect or modifier card drag on a DragBegin event.
    /// Returns true if drag started.
    /// Called from ui_root.rs on DragBegin (needs &mut UITree). `node_id` is
    /// `Option` (D9, `docs/DRAG_CAPTURE_DESIGN.md`) — a `None` means the
    /// pressed node died before the drag threshold crossed, so no card drag
    /// can be identified; that's a no-op here, same as a `Some` id matching
    /// no drag handle.
    pub fn try_begin_card_drag(&mut self, node_id: Option<NodeId>, tree: &mut UITree) -> bool {
        let Some(node_id) = node_id else {
            return false;
        };
        let group_drag = self.group_nodes.iter().find(|nodes| nodes.header == node_id)
            .and_then(|nodes| self.rack_groups[Self::scope_idx(self.active_tab)].iter().find(|group| group.id == nodes.group_id))
            .cloned();
        let handle = group_drag.as_ref().and_then(|group| {
            self.cards_for_tab(self.active_tab).iter().enumerate()
                .find(|(_, card)| group.member_ids.contains(card.effect_id()))
                .map(|(index, card)| (CardDragStack::Effects(self.active_tab), index, card.effect_index(), group.name.clone()))
        }).or_else(|| self.find_drag_handle(node_id));
        if let Some(group) = group_drag.as_ref() {
            self.select_rack_group(&group.id, false);
        }
        if let Some((stack, card_idx, fx_idx, name)) = handle {
            self.card_drag_active = true;
            self.card_drag_valid = true;
            self.card_drag_pos = None;
            self.card_drag_group = group_drag.map(|group| group.id);
            self.card_drag_destination_group = None;
            self.card_drag_effect_ids.clear();
            if let CardDragStack::Effects(tab) = stack {
                let (selection, cards) = self.selection_for_tab(tab);
                let dragged = cards[card_idx].effect_id();
                self.card_drag_effect_ids = cards.iter()
                    .filter(|card| if selection.contains(dragged) { selection.contains(card.effect_id()) } else { card.effect_id() == dragged })
                    .map(|card| card.effect_id().clone()).collect();
                let groups = &self.rack_groups[Self::scope_idx(tab)];
                let selected_groups: Vec<_> = groups.iter().filter(|group|
                    group.member_ids.iter().any(|id| self.card_drag_effect_ids.contains(id))).collect();
                if selected_groups.iter().all(|group| group.member_ids.iter().all(|id| self.card_drag_effect_ids.contains(id))) {
                    self.card_drag_group = selected_groups.first().map(|group| group.id.clone());
                }
            }
            self.card_drag_stack = stack;
            let tab = match stack {
                CardDragStack::Effects(tab) => tab,
                CardDragStack::Modifiers => InspectorTab::Layer,
            };
            self.card_drag_tab = tab;
            self.card_drag_source_index = card_idx;
            self.card_drag_effect_index = fx_idx;
            self.card_drag_target_index = card_idx;
            self.card_drag_label = name;
            self.last_effect_tab = tab;

            // Dim source card(s) border (Unity: SetDragDimmed(true))
            // If dragged card is part of a multi-selection, dim all selected
            match stack {
                CardDragStack::Effects(tab) => {
                    let dragged_id = self.cards_for_tab(tab).get(card_idx).map(|c| c.effect_id().clone());
                    let sel = self.selection_set_mut(tab);
                    let is_multi = dragged_id.as_ref().is_some_and(|id| sel.len() > 1 && sel.contains(id));
                    let sel_ids = sel.clone();
                    for (i, card) in self.cards_for_tab(tab).iter().enumerate() {
                        if (is_multi && sel_ids.contains(card.effect_id())) || (!is_multi && i == card_idx) {
                            card.set_drag_dimmed(tree, true);
                        }
                    }
                }
                CardDragStack::Modifiers => {
                    let dragged_id = self.modifier_cards.get(card_idx).and_then(ParamCardPanel::modifier_info).map(|m| m.instance_id.clone());
                    let is_multi = dragged_id.as_ref().is_some_and(|id| self.selected_modifier_ids.len() > 1 && self.selected_modifier_ids.contains(id));
                    let selected = self.selected_modifier_ids.clone();
                    for (i, card) in self.modifier_cards.iter().enumerate() {
                        let selected_card = card.modifier_info().is_some_and(|m| selected.contains(&m.instance_id));
                        if (is_multi && selected_card) || (!is_multi && i == card_idx) {
                            card.set_drag_dimmed(tree, true);
                        }
                    }
                }
            }

            self.rebuild_card_drag_overlay(tree);

            return true;
        }
        false
    }

    /// Tree rebuilds replace every widget id, including the active drag overlay.
    pub fn rebuild_card_drag_overlay(&mut self, tree: &mut UITree) {
        if !self.card_drag_active { return; }
        // Create ghost + indicator nodes — scoped to the correct column
        // Single full-width active column — both tabs drag within it.
        //
        // Ghost tier + ALLOW_OVERFLOW (`UI_CLIP_AND_Z_OWNERSHIP_DESIGN.md`
        // D1–D3): the ghost tracks the cursor for the drag's whole
        // lifetime (`update_card_drag` below), so it must be able to
        // paint outside whatever rect it starts at and outside the
        // inspector's own region clip — the region mechanism's one
        // sanctioned overflow case. `try_begin_card_drag` runs from an
        // input event, not the panel's own `build()`, so this region is
        // minted fresh per drag (there is no already-open region to
        // nest under at this point) and torn down in `end_card_drag`.
        let col_x = self.viewport_rect.x + COLUMN_PAD;
        let col_w = (self.viewport_rect.width - COLUMN_PAD * 2.0).max(0.0);
        let ghost_w = (col_w - 24.0).min(160.0);
        let region = tree.begin_region(
            self.viewport_rect,
            ZTier::Ghost,
            "card_drag_ghost",
            UIFlags::ALLOW_OVERFLOW,
        );
        let region_start = tree.count();
        self.card_drag_ghost_id = Some(tree.add_label(
            None,
            0.0,
            -100.0,
            ghost_w,
            DRAG_GHOST_H,
            &self.card_drag_label,
            UIStyle {
                bg_color: DRAG_GHOST_BG,
                text_color: DRAG_GHOST_TEXT,
                font_size: DRAG_GHOST_FONT_SIZE,
                text_align: TextAlign::Center,
                corner_radius: color::CARD_RADIUS,
                ..UIStyle::default()
            },
        ));
        self.card_drag_indicator_id = Some(tree.add_panel(
            None,
            col_x + DRAG_INDICATOR_INSET,
            -100.0,
            col_w - DRAG_INDICATOR_INSET * 2.0,
            DRAG_INDICATOR_H,
            UIStyle {
                bg_color: DRAG_INDICATOR_COLOR,
                corner_radius: color::HAIRLINE_RADIUS,
                ..UIStyle::default()
            },
        ));
        tree.end_region(region, region_start);
        self.card_drag_region_root = Some(region.root);

        if let Some(pos) = self.card_drag_pos { self.update_card_drag(pos, tree); }
    }

    /// Update card drag ghost + indicator during drag.
    pub fn update_card_drag(&mut self, pos: Vec2, tree: &mut UITree) {
        if !self.card_drag_active {
            return;
        }

        let vp = self.viewport_rect;
        self.card_drag_pos = Some(pos);
        self.card_drag_valid = vp.contains(pos);
        if self.card_drag_valid && matches!(self.card_drag_stack, CardDragStack::Modifiers) {
            // Generator controls and the effect rack are separate destinations.
            // Releasing there must cancel, not silently move to a stack end.
            let first = self.modifier_cards.first().and_then(|card| card.live_bounds(tree));
            let last = self.add_modifier_btn.map(|id| tree.get_bounds(id))
                .or_else(|| self.modifier_cards.last().and_then(|card| card.live_bounds(tree)));
            self.card_drag_valid = first.zip(last).is_some_and(|(first, last)| {
                pos.y >= first.y - SECTION_GAP && pos.y <= last.y + last.height
            });
        }
        if !self.card_drag_valid {
            self.hide_card_drag_overlay(tree);
            return;
        }
        // Single full-width active column — both tabs drag within it.
        let col_x = vp.x + COLUMN_PAD;
        let col_w = (vp.width - COLUMN_PAD * 2.0).max(0.0);
        let ghost_w = (col_w - 24.0).min(160.0);

        // Position ghost centered on cursor, clamped to column
        let ghost_x = (pos.x - ghost_w * 0.5).clamp(
            col_x + DRAG_INDICATOR_INSET,
            col_x + col_w - ghost_w - DRAG_INDICATOR_INSET,
        );
        let ghost_y = (pos.y - DRAG_GHOST_H * 0.5).clamp(vp.y, vp.y + vp.height - DRAG_GHOST_H);

        if let Some(ghost_id) = self.card_drag_ghost_id {
            tree.set_bounds(
                ghost_id,
                Rect::new(ghost_x, ghost_y, ghost_w, DRAG_GHOST_H),
            );
        }

        // Compute target card index from Y position. Hit-test against live
        // tree bounds (scroll-current, animation-current), not the
        // build-time `card_y` snapshot / animated `compute_height()` — those
        // go stale by exactly the scroll delta on the in-place scroll path
        // (BUG-265). Cards without a live rect (never built) are skipped.
        let stack = self.card_drag_stack;
        let (mut target, mut indicator_y) = {
            let cards: &[ParamCardPanel] = match stack {
                CardDragStack::Effects(tab) => self.cards_for_tab(tab),
                CardDragStack::Modifiers => &self.modifier_cards,
            };
            let card_count = cards.len();
            let mut t = card_count; // default: after last card
            for (i, card) in cards.iter().enumerate() {
                let Some(b) = card.live_bounds(tree) else {
                    continue;
                };
                let mid = b.y + b.height * 0.5;
                if pos.y < mid {
                    t = i;
                    break;
                }
            }
            let iy = if t < card_count {
                cards[t].live_bounds(tree).map(|b| b.y).unwrap_or(vp.y)
            } else if card_count > 0 {
                cards[card_count - 1]
                    .live_bounds(tree)
                    .map(|b| b.y + b.height)
                    .unwrap_or(vp.y)
            } else {
                vp.y
            };
            (t, iy)
        };
        self.card_drag_destination_group = None;
        let mut indicator_inset = DRAG_INDICATOR_INSET;
        if let CardDragStack::Effects(tab) = stack {
            let cards = self.cards_for_tab(tab);
            for nodes in &self.group_nodes {
                let bounds = tree.get_bounds(nodes.frame);
                if pos.y < bounds.y || pos.y > bounds.y + bounds.height { continue; }
                let Some(group) = self.rack_groups[Self::scope_idx(tab)].iter().find(|group| group.id == nodes.group_id) else { continue; };
                let Some(first) = cards.iter().position(|card| group.member_ids.contains(card.effect_id())) else { continue; };
                let last = cards.iter().rposition(|card| group.member_ids.contains(card.effect_id())).unwrap() + 1;
                if self.card_drag_group.is_some() || pos.x < bounds.x + Self::RACK_INDENT {
                    // A group stays a unit. Its outside edge is also the explicit
                    // route for pulling individual cards out of a group.
                    if pos.y < bounds.y + bounds.height * 0.5 {
                        target = first;
                        indicator_y = bounds.y;
                    } else {
                        target = last;
                        indicator_y = bounds.y + bounds.height;
                    }
                } else {
                    self.card_drag_destination_group = Some(group.id.clone());
                    indicator_inset += Self::RACK_INDENT;
                    if group.collapsed || pos.y < bounds.y + Self::RACK_HEADER_H {
                        target = last;
                        indicator_y = bounds.y + bounds.height;
                    } else {
                        target = target.clamp(first, last);
                    }
                }
                break;
            }
        }
        self.card_drag_target_index = target;

        if let Some(indicator_id) = self.card_drag_indicator_id {
            tree.set_bounds(
                indicator_id,
                Rect::new(
                    col_x + indicator_inset,
                    indicator_y - DRAG_INDICATOR_H * 0.5,
                    col_w - indicator_inset * 2.0,
                    DRAG_INDICATOR_H,
                ),
            );
        }
    }

    /// End card drag — restore dimming, hide ghost/indicator, return reorder action.
    /// Supports multi-select: if dragged card is part of a selection, moves all selected.
    pub fn end_card_drag(&mut self, tree: &mut UITree) -> Vec<PanelAction> {
        if !self.card_drag_active {
            return Vec::new();
        }

        if !self.card_drag_valid {
            self.cancel_card_drag(tree);
            return Vec::new();
        }
        let src = self.card_drag_source_index;
        let stack = self.card_drag_stack;
        let tab = self.card_drag_tab;
        let to_card = self.card_drag_target_index;

        if matches!(stack, CardDragStack::Modifiers) {
            let Some(layer_id) = self.modifier_scope_id.clone() else {
                self.cancel_card_drag(tree);
                return Vec::new();
            };
            let cards = &self.modifier_cards;
            let Some(dragged) = cards.get(src).and_then(ParamCardPanel::modifier_info) else {
                self.cancel_card_drag(tree);
                return Vec::new();
            };
            let dragged_id = dragged.instance_id.clone();
            let is_multi = self.selected_modifier_ids.len() > 1
                && self.selected_modifier_ids.contains(&dragged_id);
            let selected = self.selected_modifier_ids.clone();
            for (i, card) in cards.iter().enumerate() {
                let selected_card = card.modifier_info().is_some_and(|m| selected.contains(&m.instance_id));
                if (is_multi && selected_card) || (!is_multi && i == src) {
                    card.set_drag_dimmed(tree, false);
                }
            }
            let mut order: Vec<manifold_foundation::NodeId> = cards
                .iter()
                .filter_map(ParamCardPanel::modifier_info)
                .map(|m| m.instance_id.clone())
                .collect();
            let original_order = order.clone();
            let moving: Vec<manifold_foundation::NodeId> = if is_multi {
                order.iter().filter(|id| selected.contains(*id)).cloned().collect()
            } else {
                vec![dragged_id.clone()]
            };
            let moving_set: HashSet<manifold_foundation::NodeId> = moving.iter().cloned().collect();
            let removed_before = order[..to_card.min(order.len())]
                .iter()
                .filter(|id| moving_set.contains(*id))
                .count();
            order.retain(|id| !moving_set.contains(id));
            let insert_at = to_card.saturating_sub(removed_before).min(order.len());
            order.splice(insert_at..insert_at, moving);

            self.hide_card_drag_overlay(tree);
            self.card_drag_active = false;
            self.card_drag_ghost_id = None;
            self.card_drag_indicator_id = None;
            self.card_drag_region_root = None;
            if order == original_order {
                return Vec::new();
            }
            return vec![PanelAction::Project(crate::panels::ProjectAction::SceneModifiersReorder(layer_id, order))];
        }

        let before = self.cards_for_tab(tab).iter().skip(to_card)
            .find(|card| !self.card_drag_effect_ids.contains(card.effect_id()))
            .map(|card| card.effect_id().clone());
        let action = PanelAction::Params(ParamsAction::EffectMove {
            tab,
            layer_id: self.inspecting_layer_id.clone(),
            ids: self.card_drag_effect_ids.clone(),
            before,
            destination_group: self.card_drag_destination_group.clone(),
            preserve_groups: self.card_drag_group.is_some(),
        });
        self.cancel_card_drag(tree);
        vec![action]
    }

    /// Cancel without mutating the stack, including release outside its viewport.
    pub fn cancel_card_drag(&mut self, tree: &mut UITree) {
        for cards in &self.effects {
            for card in cards { card.set_drag_dimmed(tree, false); }
        }
        for card in &self.modifier_cards { card.set_drag_dimmed(tree, false); }
        self.hide_card_drag_overlay(tree);
        self.card_drag_active = false;
        self.card_drag_valid = false;
        self.card_drag_pos = None;
        self.card_drag_ghost_id = None;
        self.card_drag_indicator_id = None;
        self.card_drag_region_root = None;
        self.card_drag_effect_ids.clear();
        self.pressed_target = None;
    }

    pub(super) fn tick_card_drag_scroll(&mut self, tree: &mut UITree, dt_ms: f32) {
        let Some(pos) = self.card_drag_pos.filter(|_| self.card_drag_active && self.card_drag_valid) else { return; };
        let viewport = self.viewport_rect;
        const EDGE: f32 = 32.0;
        let direction = if pos.y < viewport.y + EDGE {
            (viewport.y + EDGE - pos.y) / EDGE
        } else if pos.y > viewport.y + viewport.height - EDGE {
            -(pos.y - (viewport.y + viewport.height - EDGE)) / EDGE
        } else { return; };
        let delta = direction * 360.0 * dt_ms.min(50.0) / 1000.0 / crate::scroll_container::SCROLL_SPEED;
        if self.try_scroll_in_place(delta, pos.x, tree) {
            self.update_card_drag(pos, tree);
        }
    }

    /// Find which card's drag handle matches the given node_id.
    /// Returns (ordered stack, card index, effect index, display name).
    fn find_drag_handle(&self, node_id: NodeId) -> Option<(CardDragStack, usize, usize, String)> {
        // No scope gate: `is_drag_handle` is false on a non-live card, so only the
        // active scope's cards can match (the node range is the source of truth).
        for (i, card) in self.effects[Self::SCOPE_MASTER].iter().enumerate() {
            if card.is_drag_handle(node_id) {
                return Some((
                    CardDragStack::Effects(InspectorTab::Master),
                    i,
                    card.effect_index(),
                    card.effect_name().to_string(),
                ));
            }
        }
        for (i, card) in self.effects[Self::SCOPE_LAYER].iter().enumerate() {
            if card.is_drag_handle(node_id) {
                return Some((
                    CardDragStack::Effects(InspectorTab::Layer),
                    i,
                    card.effect_index(),
                    card.effect_name().to_string(),
                ));
            }
        }
        for (i, card) in self.modifier_cards.iter().enumerate() {
            if card.is_drag_handle(node_id) {
                return Some((CardDragStack::Modifiers, i, 0, card.effect_name().to_string()));
            }
        }
        None
    }

    fn hide_card_drag_overlay(&self, tree: &mut UITree) {
        if let Some(ghost_id) = self.card_drag_ghost_id {
            tree.set_bounds(ghost_id, Rect::new(0.0, -100.0, 0.0, 0.0));
        }
        if let Some(indicator_id) = self.card_drag_indicator_id {
            tree.set_bounds(indicator_id, Rect::new(0.0, -100.0, 0.0, 0.0));
        }
    }
}
