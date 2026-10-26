//! Per-frame in-place syncs for the properties card: row values and driver
//! period readouts, written without a structural rebuild.
use super::*;

impl ScenePanel {
    /// Per-frame VALUE sync (sibling of `ui_bridge::sync_card_values`): push
    /// the properties card's row values from the layer's full generator
    /// manifest, without a structural rebuild. `slots` is the id-keyed channel
    /// `ui_translate::with_param_slots(&gp.params, …)` hands out for the SAME
    /// layer `full_params` was built from. Each slot JOINS onto the built row
    /// carrying the same id (BUG-313) via `row_id_index` — never by position.
    /// A manifest param the current selection isn't showing simply misses the
    /// join and is ignored; a built manifest row with no manifest entry is
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
            // An eye follows its item's Visible.
            if id.ends_with(".visible")
                && let Some(&eye) = self.outliner_eye_index.get(id)
                && let Some((eye_id, _, shown)) = self.outliner_eye_ids.get_mut(eye)
            {
                if (slot.base > 0.5) != (*shown > 0.5) {
                    tree.set_text(
                        *eye_id,
                        if slot.base > 0.5 {
                            "\u{1F441}"
                        } else {
                            "\u{2013}"
                        },
                    );
                }
                *shown = slot.base;
            }
            let Some(&i) = card.row_id_index.get(id) else {
                continue;
            };
            // Dirty-check on change only — same shape as
            // `ParamCardPanel::sync_param_value`. The key is
            // `last_pushed_values` (single-writer, this sync only) — NOT
            // `current_values`, which the drag/type-in paths write
            // mid-gesture.
            let value = target
                .as_ref()
                .and_then(|target| card.row_host.active_param_value(target, &card.rows[i].id))
                .unwrap_or(slot.value);
            let prev = card.last_pushed_values[i];
            card.current_values[i] = value;
            if value != prev || prev.is_nan() {
                card.last_pushed_values[i] = value;
                if card.row_host.toggle_ids[i].is_some() {
                    card.row_host
                        .push_button_value(tree, i, value, &card.rows[i].spec);
                } else {
                    card.row_host
                        .push_slider_value(tree, i, value, &card.rows[i].spec, None);
                }
            }
            if let Some(c) = card.row_value_synced.get_mut(i) {
                *c = true;
            }
        }
        for (i, synced) in card.row_value_synced.iter().enumerate() {
            // A field or helper row has no manifest slot to sync.
            if !*synced && card.rows[i].affordance.is_none() {
                debug_assert!(
                    false,
                    "BUG-313/INV-6: built scene row {} (id {:?}) has no live manifest entry",
                    i, card.rows[i].id,
                );
                crate::panels::param_slider_shared::warn_join_gap_once(card.rows[i].id.as_ref());
            }
        }
        if let Some(node) = self.render_mode_id
            && let Some(label) = self.render_mode_label()
        {
            tree.set_text(node, &label);
        }
        self.sync_swatch_values(tree);
    }

    /// Feed each properties swatch its channels' live values (they are rows
    /// here, so a drag previews on the swatch) and repaint the changed ones.
    pub(super) fn sync_swatch_values(&mut self, tree: &mut UITree) {
        for row in 0..self.properties_card.rows.len() {
            let Some(members) = &self.properties_card.rows[row].rgb_members else {
                continue;
            };
            let rgb = members
                .ids
                .each_ref()
                .map(|id| self.material_full_value(id));
            self.properties_card.row_host.set_swatch_rgb(row, rgb);
        }
        self.properties_card.row_host.paint_swatches(tree);
    }

    /// The live-scalar push (UI_INVALIDATION D22) for every row the panel
    /// shows. `shapes_of` answers for a row's param id on the live layer.
    pub fn sync_live_shapes(
        &mut self,
        tree: &mut UITree,
        shapes_of: &mut dyn FnMut(&str) -> crate::panels::param_slider_shared::RowShapes,
    ) {
        if !self.open {
            return;
        }
        let card = &mut self.properties_card;
        card.row_host
            .sync_live_shapes(tree, &mut card.mod_state, &mut card.rows, shapes_of);
    }

    pub fn sync_driver_frame_rates(
        &mut self,
        tree: &mut UITree,
        rate_of: &mut dyn FnMut(&str) -> Option<(u32, f32)>,
    ) {
        let card = &mut self.properties_card;
        card.row_host
            .sync_driver_frame_rates(tree, &mut card.mod_state, &mut card.rows, rate_of);
    }

    /// Fire meter for the properties card's rows, keyed on the generator
    /// instance that owns `full_params`. Does nothing while closed or with no
    /// generator surface.
    pub fn update_fire_meters(
        &self,
        tree: &mut UITree,
        fire_level: &dyn Fn(u64) -> Option<f32>,
        dt: f32,
    ) {
        if !self.open {
            return;
        }
        let Some(full_params) = self.full_params.as_ref() else {
            return;
        };
        let card = &self.properties_card;
        card.row_host.update_fire_meters(
            tree,
            &card.rows,
            full_params.effect_id.as_str(),
            fire_level,
            dt,
        );
    }

    /// The send the open fire-mode drawer in the properties card reads, if any.
    pub fn open_fire_mode_drawer_send(&self) -> Option<manifold_foundation::AudioSendId> {
        if !self.open {
            return None;
        }
        let card = &self.properties_card;
        card.row_host
            .open_fire_mode_drawer_send(&card.rows, &card.mod_state)
    }

    /// The band the open fire-mode drawer in the properties card reads, if any.
    pub fn open_fire_mode_drawer_band(&self) -> Option<crate::types::AudioBand> {
        if !self.open {
            return None;
        }
        let card = &self.properties_card;
        card.row_host
            .open_fire_mode_drawer_band(&card.rows, &card.mod_state)
    }
}
