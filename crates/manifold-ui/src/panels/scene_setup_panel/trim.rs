//! Scene properties trim gestures.
//!
//! Trim state lives separately from the panel's large layout file because a
//! trim drag has a small but important lifetime contract: its wire address,
//! dragged edge, and current range survive a structural rebuild, while the
//! track node is resolved again from the stable parameter id each move.

use super::*;
use crate::panels::param_slider_shared::clamp_trim_range;
use crate::panels::{PanelAction, ScrubPhase, ScrubValue, TrimKind, ValueRef};
use crate::slider::{BitmapSlider, TrackSpan};
use manifold_foundation::{LayerId, ParamId};

/// The complete scene trim gesture identity captured at PointerDown. The row
/// index and node ids are deliberately absent: selection/rebuilds can change
/// both during a gesture, while the bound layer and real parameter id remain
/// the same wire address.
#[derive(Clone, Debug)]
pub(crate) struct SceneTrimDrag {
    pub(crate) kind: TrimKind,
    pub(crate) layer_id: LayerId,
    pub(crate) param_id: ParamId,
    pub(crate) is_min: bool,
    pub(crate) range: (f32, f32),
}

impl ScenePanel {
    fn scene_trim_ranges(&self, row: usize) -> [Option<(f32, f32)>; 3] {
        let card = &self.properties_card;
        [
            card.mod_state
                .driver_expanded
                .get(row)
                .copied()
                .unwrap_or(false)
                .then(|| {
                    (
                        card.mod_state.trim_min.get(row).copied().unwrap_or(0.0),
                        card.mod_state.trim_max.get(row).copied().unwrap_or(1.0),
                    )
                }),
            card.rows.get(row).and_then(|r| r.mapping.ableton_range),
            card.mod_state.audio_rows.get(row).and_then(|audio| {
                audio.active.then_some((audio.range_min, audio.range_max))
            }),
        ]
    }

    fn scene_trim_range_for(ranges: [Option<(f32, f32)>; 3], kind: TrimKind) -> Option<(f32, f32)> {
        match kind {
            TrimKind::Driver => ranges[0],
            TrimKind::Ableton => ranges[1],
            TrimKind::Audio => ranges[2],
        }
    }

    /// Start a scene trim drag if `node_id` is a trim bar or a driver's
    /// proximity zone. The emitted Begin snapshots the undo baseline.
    pub(super) fn handle_trim_pointer_down(
        &mut self,
        node_id: NodeId,
        pos: Vec2,
        tree: &UITree,
    ) -> Option<Vec<PanelAction>> {
        let layer_id = self.live_layer_id()?.clone();
        let widget = tree.widget_of(node_id);
        let (row, _role) = self.properties_card.row_host.row_index.get(widget)?;
        let ranges = self.scene_trim_ranges(row);
        let (kind, is_min) = self
            .properties_card
            .row_host
            .trim_hit(row, node_id, pos, tree, ranges[0])?;
        let range = Self::scene_trim_range_for(ranges, kind)?;
        let param_id = self.properties_card.rows.get(row)?.id.clone();
        self.trim_drag.start(
            SceneTrimDrag {
                kind,
                layer_id: layer_id.clone(),
                param_id: param_id.clone(),
                is_min,
                range,
            },
            pos,
        );
        Some(vec![PanelAction::Scrub(
            ValueRef::Trim(kind, GraphParamTarget::GeneratorOf(layer_id), param_id),
            ScrubPhase::Begin,
        )])
    }

    fn set_scene_trim_range(&mut self, kind: TrimKind, row: usize, range: (f32, f32)) {
        match kind {
            TrimKind::Driver => {
                if let Some(value) = self.properties_card.mod_state.trim_min.get_mut(row) {
                    *value = range.0;
                }
                if let Some(value) = self.properties_card.mod_state.trim_max.get_mut(row) {
                    *value = range.1;
                }
            }
            TrimKind::Ableton => {
                if let Some(row) = self.properties_card.rows.get_mut(row) {
                    row.mapping.ableton_range = Some(range);
                }
            }
            TrimKind::Audio => {
                if let Some(audio) = self.properties_card.mod_state.audio_rows.get_mut(row) {
                    audio.range_min = range.0;
                    audio.range_max = range.1;
                }
            }
        }
    }

    /// Draw the active range across rebuilds until the content snapshot catches up.
    pub(super) fn sync_trim_preview(&mut self, row: usize) {
        if let Some(drag) = self.trim_drag.payload()
            && self.live_layer_id() == Some(&drag.layer_id)
            && self.properties_card.rows[row].id == drag.param_id
        {
            self.set_scene_trim_range(drag.kind, row, drag.range);
        }
    }

    /// Continue the captured scene trim drag. A changed bound layer consumes
    /// the event but emits no move, so a gesture cannot redirect to a same-id
    /// row in a newly selected layer. A same-layer rebuild resolves the live
    /// track by the captured stable parameter id.
    pub(super) fn handle_trim_drag(&mut self, pos: Vec2, tree: &UITree) -> Vec<PanelAction> {
        let Some(payload) = self.trim_drag.payload().cloned() else {
            return Vec::new();
        };
        if self.live_layer_id() != Some(&payload.layer_id) {
            return Vec::new();
        }
        let Some(&row) = self
            .properties_card
            .row_id_index
            .get(payload.param_id.as_ref())
        else {
            return Vec::new();
        };
        let Some(track) = self
            .properties_card
            .row_host
            .slider_ids
            .get(row)
            .and_then(Option::as_ref)
            .map(|ids| ids.track)
        else {
            return Vec::new();
        };
        let track_rect = tree.get_bounds(track);
        let norm = BitmapSlider::x_to_normalized(TrackSpan::of(track_rect), pos.x);
        let range = clamp_trim_range(payload.range, norm, payload.is_min);
        self.set_scene_trim_range(payload.kind, row, range);
        if let Some(active) = self.trim_drag.payload_mut() {
            active.range = range;
        }
        vec![PanelAction::Scrub(
            ValueRef::Trim(
                payload.kind,
                GraphParamTarget::GeneratorOf(payload.layer_id),
                payload.param_id,
            ),
            ScrubPhase::Move(ScrubValue::Range(range.0, range.1)),
        )]
    }

    /// Release the captured scene trim gesture. The payload is taken exactly
    /// once, so a PointerUp followed by DragEnd cannot create two commits.
    pub(super) fn handle_trim_end(&mut self) -> Vec<PanelAction> {
        let Some(payload) = self.trim_drag.release() else {
            return Vec::new();
        };
        vec![PanelAction::Scrub(
            ValueRef::Trim(
                payload.kind,
                GraphParamTarget::GeneratorOf(payload.layer_id),
                payload.param_id,
            ),
            ScrubPhase::Commit,
        )]
    }
}
