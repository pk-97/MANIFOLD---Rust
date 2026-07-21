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
    pub fn handle_drag(&mut self, pos: Vec2, tree: &mut UITree) -> Vec<PanelAction> {
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
                    .map(|c| c.handle_drag(pos, tree))
                    .unwrap_or_default(),
                PressedTarget::LayerEffect(i) => self.effects[Self::SCOPE_LAYER]
                    .get_mut(i)
                    .map(|c| c.handle_drag(pos, tree))
                    .unwrap_or_default(),
                PressedTarget::GenParam => self
                    .gen_params
                    .as_mut()
                    .map(|gp| gp.handle_drag(pos, tree))
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
                PressedTarget::Scrollbar => Vec::new(),
            }
        } else {
            Vec::new()
        };

        self.pressed_target = None;
        actions
    }

    /// Try to begin a card drag on a DragBegin event. Returns true if drag started.
    /// Called from ui_root.rs on DragBegin (needs &mut UITree). `node_id` is
    /// `Option` (D9, `docs/DRAG_CAPTURE_DESIGN.md`) — a `None` means the
    /// pressed node died before the drag threshold crossed, so no card drag
    /// can be identified; that's a no-op here, same as a `Some` id matching
    /// no drag handle.
    pub fn try_begin_card_drag(&mut self, node_id: Option<NodeId>, tree: &mut UITree) -> bool {
        let Some(node_id) = node_id else {
            return false;
        };
        // Check each tab's effect cards for a drag handle match
        if let Some((tab, card_idx, fx_idx, name)) = self.find_drag_handle(node_id) {
            self.card_drag_active = true;
            self.card_drag_tab = tab;
            self.card_drag_source_index = card_idx;
            self.card_drag_effect_index = fx_idx;
            self.card_drag_target_index = card_idx;
            self.card_drag_label = name;
            self.last_effect_tab = tab;

            // Dim source card(s) border (Unity: SetDragDimmed(true))
            // If dragged card is part of a multi-selection, dim all selected
            let dragged_id = self
                .cards_for_tab(tab)
                .get(card_idx)
                .map(|c| c.effect_id().clone());
            let sel = self.selection_set_mut(tab);
            let is_multi = dragged_id
                .as_ref()
                .is_some_and(|id| sel.len() > 1 && sel.contains(id));
            if is_multi {
                let sel_ids: HashSet<EffectId> = sel.clone();
                let cards = self.cards_for_tab(tab);
                for card in cards {
                    if sel_ids.contains(card.effect_id()) {
                        card.set_drag_dimmed(tree, true);
                    }
                }
            } else {
                let cards = self.cards_for_tab(tab);
                if let Some(card) = cards.get(card_idx) {
                    card.set_drag_dimmed(tree, true);
                }
            }

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

            return true;
        }
        false
    }

    /// Update card drag ghost + indicator during drag.
    pub fn update_card_drag(&mut self, pos: Vec2, tree: &mut UITree) {
        if !self.card_drag_active {
            return;
        }

        let vp = self.viewport_rect;
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
        let tab = self.card_drag_tab;
        let (target, indicator_y) = {
            let cards = self.cards_for_tab(tab);
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
        self.card_drag_target_index = target;

        if let Some(indicator_id) = self.card_drag_indicator_id {
            tree.set_bounds(
                indicator_id,
                Rect::new(
                    col_x + DRAG_INDICATOR_INSET,
                    indicator_y - DRAG_INDICATOR_H * 0.5,
                    col_w - DRAG_INDICATOR_INSET * 2.0,
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

        let src = self.card_drag_source_index;
        let tab = self.card_drag_tab;
        let from = self.card_drag_effect_index;
        let to_card = self.card_drag_target_index;

        // Check if dragged card is part of a multi-selection
        let dragged_id = self
            .cards_for_tab(tab)
            .get(src)
            .map(|c| c.effect_id().clone());
        let sel = self.selection_set_mut(tab);
        let is_multi = dragged_id
            .as_ref()
            .is_some_and(|id| sel.len() > 1 && sel.contains(id));

        // Restore source card border + compute target effect index
        let to_fx = {
            // Restore dimming on all selected cards (or just source)
            if is_multi {
                let sel_ids: HashSet<EffectId> = self.selection_set_mut(tab).clone();
                let cards = self.cards_for_tab(tab);
                for card in cards {
                    if sel_ids.contains(card.effect_id()) {
                        card.set_drag_dimmed(tree, false);
                    }
                }
            } else if let Some(card) = self.cards_for_tab(tab).get(src) {
                card.set_drag_dimmed(tree, false);
            }
            let cards = self.cards_for_tab(tab);
            if to_card < cards.len() {
                cards[to_card].effect_index()
            } else if !cards.is_empty() {
                // After-last drop: one past the HIGHEST effect index in the
                // tab's cards, not `cards.last()`'s index — the tab's cards
                // are a contiguous run of the flat effects list today, but
                // list order isn't guaranteed to track index order, and
                // `.last()` silently breaks the moment it doesn't (BUG-265
                // root cause 3).
                cards.iter().map(|c| c.effect_index()).max().unwrap() + 1
            } else {
                0
            }
        };

        // Hide ghost + indicator (move offscreen)
        if let Some(ghost_id) = self.card_drag_ghost_id {
            tree.set_bounds(ghost_id, Rect::new(0.0, -100.0, 0.0, 0.0));
        }
        if let Some(indicator_id) = self.card_drag_indicator_id {
            tree.set_bounds(indicator_id, Rect::new(0.0, -100.0, 0.0, 0.0));
        }

        self.card_drag_active = false;
        self.card_drag_ghost_id = None;
        self.card_drag_indicator_id = None;
        self.card_drag_region_root = None;

        if is_multi {
            // Multi-select: move all selected effects as a group
            let sel_ids = self.selection_set_mut(tab).clone();
            let cards = self.cards_for_tab(tab);
            // Convert selected IDs to sorted effect indices
            let mut effect_indices: Vec<usize> = cards
                .iter()
                .filter(|c| sel_ids.contains(c.effect_id()))
                .map(|c| c.effect_index())
                .collect();
            effect_indices.sort_unstable();
            if !effect_indices.is_empty() {
                vec![PanelAction::Params(ParamsAction::EffectReorderGroup(effect_indices, to_fx))]
            } else {
                Vec::new()
            }
        } else if to_fx != from && to_fx != from + 1 {
            vec![PanelAction::Params(ParamsAction::EffectReorder(from, to_fx))]
        } else {
            Vec::new()
        }
    }

    /// Find which card's drag handle matches the given node_id.
    /// Returns (tab, card_index_in_vec, effect_index, effect_name).
    fn find_drag_handle(&self, node_id: NodeId) -> Option<(InspectorTab, usize, usize, String)> {
        // No scope gate: `is_drag_handle` is false on a non-live card, so only the
        // active scope's cards can match (the node range is the source of truth).
        for (i, card) in self.effects[Self::SCOPE_MASTER].iter().enumerate() {
            if card.is_drag_handle(node_id) {
                return Some((
                    InspectorTab::Master,
                    i,
                    card.effect_index(),
                    card.effect_name().to_string(),
                ));
            }
        }
        for (i, card) in self.effects[Self::SCOPE_LAYER].iter().enumerate() {
            if card.is_drag_handle(node_id) {
                return Some((
                    InspectorTab::Layer,
                    i,
                    card.effect_index(),
                    card.effect_name().to_string(),
                ));
            }
        }
        None
    }
}
