//! Per-frame in-place syncs for the properties card: row values and driver
//! period readouts, written without a structural rebuild.
use super::*;

impl ScenePanel {
    /// Per-frame VALUE sync (sibling of `ui_bridge::sync_card_values`): push
    /// the unified properties card's row values from the layer's full generator
    /// manifest, without a structural rebuild. `slots` is the id-keyed channel
    /// `ui_translate::with_param_slots(&gp.params, …)` hands out for the SAME
    /// layer `full_params` was built from. Each slot JOINS onto the built row
    /// carrying the same id (BUG-313) via `row_id_index` — never by position,
    /// so no retained-index list can drift. A manifest param the current
    /// outliner selection isn't showing (not in this section's rows) simply
    /// misses the join and is ignored; a built row with no manifest entry is
    /// caught by the INV-6 coverage check.
    pub fn sync_properties_values(
        &mut self,
        tree: &mut UITree,
        slots: &mut dyn Iterator<Item = (&str, crate::view::UiParamSlot)>,
    ) {
        if !self.open {
            return;
        }
        let target = self
            .live_layer_id()
            .cloned()
            .map(GraphParamTarget::GeneratorOf);
        let full_params = &mut self.full_params;
        let full_param_id_index = &self.full_param_id_index;
        let card = &mut self.properties_card;
        card.row_value_synced.clear();
        card.row_value_synced.resize(card.rows.len(), false);
        for (id, slot) in slots {
            if let Some(&full_index) = full_param_id_index.get(id)
                && let Some(surface) = full_params.as_mut()
                && let Some(row) = surface.rows.get_mut(full_index)
            {
                row.value.base = slot.base;
                row.value.effective = slot.value;
            }
            let Some(&i) = card.row_id_index.get(id) else {
                continue;
            };
            // Dirty-check on change only — same shape as
            // `ParamCardPanel::sync_param_value` (param_card/render.rs): an
            // unconditional push formats the value String and touches the
            // tree for every row every frame, even when nothing moved. The
            // key is `last_pushed_values` (single-writer, this sync only) —
            // NOT `current_values`, which the drag/type-in paths write
            // mid-gesture; gating on that skipped the post-commit push.
            let value = target
                .as_ref()
                .and_then(|target| card.row_host.active_param_value(target, &card.rows[i].id))
                .unwrap_or(slot.value);
            let prev = card.last_pushed_values[i];
            card.current_values[i] = value;
            if value != prev || prev.is_nan() {
                card.last_pushed_values[i] = value;
                if card.row_host.toggle_ids[i].is_some() {
                    card.row_host.push_button_value(tree, i, value, &card.rows[i].spec);
                } else {
                    card.row_host
                        .push_slider_value(tree, i, value, &card.rows[i].spec, None);
                }
            }
            if let Some(c) = card.row_value_synced.get_mut(i) {
                *c = true;
            }
        }
        // Object modifier cards share the same layer manifest channel. Their
        // retained rows join by the unchanged ParamId; rebuilding a second
        // value store here would let object-card order drift from the graph.
        if let Some(surface) = full_params.as_ref() {
            for card in &mut self.object_modifier_cards {
                if card.node_count() == 0 { continue; }
                let mut modifier_slots = surface.rows.iter().map(|row| {
                    (
                        row.id.as_ref(),
                        crate::view::UiParamSlot {
                            value: row.value.effective,
                            base: row.value.base,
                            exposed: row.value.exposed,
                            min: row.spec.min,
                            max: row.spec.max,
                        },
                    )
                });
                card.sync_values(tree, &mut modifier_slots);
            }
            for card in &mut self.force_cards {
                if card.node_count() == 0 { continue; }
                let mut force_slots = surface.rows.iter().map(|row| {
                    (
                        row.id.as_ref(),
                        crate::view::UiParamSlot {
                            value: row.value.effective,
                            base: row.value.base,
                            exposed: row.value.exposed,
                            min: row.spec.min,
                            max: row.spec.max,
                        },
                    )
                });
                card.sync_values(tree, &mut force_slots);
            }
        }
        for &(node, slot) in &self.material_mode_ids {
            let label = match card.current_values[slot].round() as i32 { 1 => "Off", 2 => "On", _ => "Auto" };
            tree.set_text(node, label);
        }
        for (i, synced) in card.row_value_synced.iter().enumerate() {
            if !*synced {
                debug_assert!(
                    false,
                    "BUG-313/INV-6: built scene row {} (id {:?}) has no live manifest entry",
                    i, card.rows[i].id,
                );
                crate::panels::param_slider_shared::warn_join_gap_once(card.rows[i].id.as_ref());
            }
        }
        self.sync_swatch_values(tree);
    }

    /// Feed each properties swatch its channels' live values (they are rows
    /// here, so a drag previews on the swatch) and repaint the changed ones.
    pub(super) fn sync_swatch_values(&mut self, tree: &mut UITree) {
        for row in 0..self.properties_card.rows.len() {
            let Some(members) = &self.properties_card.rows[row].rgb_members else { continue };
            let rgb = members.ids.each_ref().map(|id| self.material_full_value(id));
            self.properties_card.row_host.set_swatch_rgb(row, rgb);
        }
        self.properties_card.row_host.paint_swatches(tree);
    }

    /// The live-scalar push (UI_INVALIDATION D22) for every row card the panel
    /// shows: the properties card, the object modifier cards and the force
    /// cards. `shapes_of` answers for a row's param id on the live layer.
    pub fn sync_live_shapes(
        &mut self,
        tree: &mut UITree,
        shapes_of: &mut dyn FnMut(&str) -> crate::panels::param_slider_shared::RowShapes,
    ) {
        if !self.open {
            return;
        }
        let card = &mut self.properties_card;
        for (row, param) in card.rows.iter_mut().enumerate() {
            let shapes = shapes_of(param.id.as_ref());
            card.row_host.sync_row_shapes(tree, &mut card.mod_state, param, row, &shapes);
        }
        for card in self.object_modifier_cards.iter_mut().chain(self.force_cards.iter_mut()) {
            card.sync_live_shapes(tree, shapes_of);
        }
    }

    pub fn sync_driver_frame_rates(
        &mut self,
        tree: &mut UITree,
        rate_of: &mut dyn FnMut(&str) -> Option<(u32, f32)>,
    ) {
        let card = &mut self.properties_card;
        for (row, param) in card.rows.iter_mut().enumerate() {
            if !card.mod_state.driver_frame_aligned.get(row).copied().unwrap_or(false) {
                continue;
            }
            let rate = rate_of(param.id.as_ref());
            // The row model too, so the next build from these rows agrees.
            param.modulation.driver_frame_rate = rate;
            card.row_host.sync_driver_frame_rate(tree, &mut card.mod_state, row, rate);
        }
    }
}
