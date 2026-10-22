//! The Scale link toggle of a Transform section (SCENE_DOCUMENT_DESIGN.md
//! section 7.17): linked, a drag on one Scale axis scales all three
//! proportionally as one undo step; unlinked, each axis is free. It is a card
//! editing helper over the existing three params — no param, so modulation,
//! MIDI and OSC still reach each axis — and the link state is view state:
//! never saved and never mapped. Its row is a `RowAffordance::ScaleLink` row
//! of the shared param surface.

use super::*;
use crate::param_surface::{RowAffordance, RowValue};
use manifold_foundation::ParamId;

pub(super) const SCALE_AXES: [&str; 3] = ["scale_x", "scale_y", "scale_z"];

impl ScenePanel {
    pub(super) fn scale_linked(&self, item: &ItemId) -> bool {
        !self.scale_unlinked.contains(item)
    }

    /// The Scale link row of `item`'s Transform section.
    pub(super) fn scale_link_row(&self, item: &ItemId) -> ParamRow {
        let linked = self.scale_linked(item);
        let mut row = placeholder_param_info();
        row.id = ParamId::from(format!("{item}#scale_link"));
        row.spec.name = "Link Scale".to_owned();
        row.spec.is_toggle = true;
        row.spec.default = f32::from(u8::from(linked));
        row.spec.tooltip = Some("Scale all three axes together".to_owned());
        row.spec.scene = Some(manifold_foundation::scene_surface::SceneParamDesc {
            role: SurfaceRole::Setup,
            group: ControlGroup::Transform,
            band: None,
            card_order: 0,
            card_label: manifold_foundation::scene_surface::CardLabel::Param,
            gate: None,
            switch: None,
            colour: None,
        });
        row.value = RowValue { base: f32::from(u8::from(linked)), effective: f32::from(u8::from(linked)), exposed: false, driven: false };
        row.affordance = Some(RowAffordance::ScaleLink { item: item.clone() });
        row
    }

    /// A click on a Scale link row flips its item's link. `true` when the
    /// click was one.
    pub(super) fn scale_link_click(&mut self, row: usize) -> bool {
        let Some(RowAffordance::ScaleLink { item }) = self.properties_card.rows.get(row).and_then(|row| row.affordance.clone()) else {
            return false;
        };
        if !self.scale_unlinked.remove(&item) {
            self.scale_unlinked.insert(item);
        }
        self.view_version += 1;
        true
    }

    /// The three Scale ids a drag of `param_id` moves together, with the axis
    /// it drags — when it is a Scale axis of a linked item with all three
    /// rows on the card.
    fn scale_triple(&self, param_id: &ParamId) -> Option<([ParamId; 3], usize)> {
        let (item, property) = param_id.as_ref().split_once('.')?;
        let axis = SCALE_AXES.iter().position(|name| *name == property)?;
        if !self.scale_linked(&ItemId::new(item)) {
            return None;
        }
        let ids = SCALE_AXES.map(|name| ParamId::from(format!("{item}.{name}")));
        ids.iter().all(|id| self.properties_card.row_id_index.contains_key(id.as_ref())).then_some((ids, axis))
    }

    /// A drag on one Scale axis of a linked item, rewritten as the three
    /// axes' one gesture.
    pub(super) fn rewrite_scale_link_actions(&mut self, actions: Vec<PanelAction>) -> Vec<PanelAction> {
        actions
            .into_iter()
            .map(|action| {
                let PanelAction::Scrub(crate::ValueRef::Param(target, param_id), phase) = &action else {
                    return action;
                };
                let Some((ids, axis)) = self.scale_triple(param_id) else {
                    return action;
                };
                let row_of = |id: &ParamId| self.properties_card.row_id_index.get(id.as_ref()).copied();
                match phase {
                    crate::ScrubPhase::Begin => {
                        let start = ids.each_ref().map(|id| row_of(id).and_then(|row| self.properties_card.current_values.get(row)).copied().unwrap_or(1.0));
                        self.scale_drag = Some((ids.clone(), start));
                        PanelAction::Scrub(crate::ValueRef::ParamLinked(target.clone(), ids), crate::ScrubPhase::Begin)
                    }
                    crate::ScrubPhase::Move(crate::ScrubValue::Scalar(value)) => {
                        let Some((drag_ids, start)) = self.scale_drag.clone().filter(|(drag_ids, _)| *drag_ids == ids) else {
                            return action;
                        };
                        let factor = (start[axis].abs() > 1e-6).then(|| value / start[axis]);
                        let mut values = start.map(|s| factor.map_or(*value, |k| s * k));
                        for (index, id) in drag_ids.iter().enumerate() {
                            if let Some(row) = row_of(id) {
                                let spec = &self.properties_card.rows[row].spec;
                                values[index] = values[index].clamp(spec.min.min(spec.max), spec.max.max(spec.min));
                            }
                        }
                        for (id, value) in drag_ids.iter().zip(values) {
                            if let Some(row) = row_of(id) {
                                self.properties_card.current_values[row] = value;
                            }
                        }
                        PanelAction::Scrub(crate::ValueRef::ParamLinked(target.clone(), ids), crate::ScrubPhase::Move(crate::ScrubValue::Rgb(values)))
                    }
                    crate::ScrubPhase::Commit => {
                        self.scale_drag = None;
                        PanelAction::Scrub(crate::ValueRef::ParamLinked(target.clone(), ids), crate::ScrubPhase::Commit)
                    }
                    _ => action,
                }
            })
            .collect()
    }
}
