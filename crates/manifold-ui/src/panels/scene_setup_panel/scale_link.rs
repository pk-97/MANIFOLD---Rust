//! The Scale link toggle of a Transform section (SCENE_DOCUMENT_DESIGN.md
//! section 7.17): linked, an edit of one Scale axis scales all three
//! proportionally as one undo step; unlinked, each axis is free. It is a card
//! editing helper over the existing three params — no param, so modulation,
//! MIDI and OSC still reach each axis — and the link state is view state:
//! never saved and never mapped. Its row is a `RowAffordance::ScaleLink` row
//! of the shared param surface. The app applies the link where every card
//! edit of a param lands (`ui_bridge::scrub::dispatch_scrub`), so a drag, a
//! typed value and a reset all keep the three axes in proportion.

use super::*;
use crate::param_surface::{RowAffordance, RowValue};
use manifold_foundation::ParamId;

pub(super) const SCALE_AXES: [&str; 3] = ["scale_x", "scale_y", "scale_z"];

impl ScenePanel {
    pub(super) fn scale_linked(&self, layer: &LayerId, item: &ItemId) -> bool {
        !self
            .scale_unlinked
            .get(layer)
            .is_some_and(|items| items.contains(item))
    }

    /// The Scale link row of `item`'s Transform section.
    pub(super) fn scale_link_row(&self, item: &ItemId) -> ParamRow {
        let linked = self
            .live_layer_id()
            .is_none_or(|layer| self.scale_linked(layer, item));
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
        row.value = RowValue {
            base: f32::from(u8::from(linked)),
            effective: f32::from(u8::from(linked)),
            exposed: false,
            driven: false,
        };
        row.affordance = Some(RowAffordance::ScaleLink { item: item.clone() });
        row
    }

    /// A click on a Scale link row flips its item's link. `true` when the
    /// click was one.
    pub(super) fn scale_link_click(&mut self, row: usize) -> bool {
        let Some(RowAffordance::ScaleLink { item }) = self
            .properties_card
            .rows
            .get(row)
            .and_then(|row| row.affordance.clone())
        else {
            return false;
        };
        let Some(layer) = self.live_layer_id().cloned() else {
            return false;
        };
        let unlinked = self.scale_unlinked.entry(layer).or_default();
        if !unlinked.remove(&item) {
            unlinked.insert(item);
        }
        self.view_version += 1;
        true
    }

    /// When `param_id` on `layer` is a Scale axis of an item whose link is
    /// on: the item's three Scale ids, and which of them `param_id` is. Only
    /// for the layer this panel shows, whose card holds the link.
    pub fn linked_scale_axes(
        &self,
        layer: &LayerId,
        param_id: &str,
    ) -> Option<([ParamId; 3], usize)> {
        if self.live_layer_id() != Some(layer) {
            return None;
        }
        let (item, property) = param_id.split_once('.')?;
        let axis = SCALE_AXES.iter().position(|name| *name == property)?;
        self.scale_linked(layer, &ItemId::new(item)).then(|| {
            (
                SCALE_AXES.map(|name| ParamId::from(format!("{item}.{name}"))),
                axis,
            )
        })
    }
}
