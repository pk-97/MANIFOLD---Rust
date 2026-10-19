use crate::LayerId;
use crate::units::Beats;
use std::collections::HashSet;

/// Region-based selection on the timeline.
#[derive(Debug, Clone, Default)]
pub struct SelectionRegion {
    pub start_beat: Beats,
    pub end_beat: Beats,
    pub is_active: bool,

    // ── LayerId-based fields (stable identity) ──
    pub start_layer_id: Option<LayerId>,
    pub end_layer_id: Option<LayerId>,
    pub selected_layer_ids: HashSet<LayerId>,
}

impl SelectionRegion {
    pub fn contains_beat(&self, beat: Beats) -> bool {
        beat >= self.start_beat && beat < self.end_beat
    }

    pub fn contains_layer_id(&self, id: &LayerId) -> bool {
        self.selected_layer_ids.contains(id)
    }

    pub fn duration_beats(&self) -> Beats {
        self.end_beat - self.start_beat
    }

    pub fn clear(&mut self) {
        self.start_beat = Beats::ZERO;
        self.end_beat = Beats::ZERO;
        self.is_active = false;
        self.start_layer_id = None;
        self.end_layer_id = None;
        self.selected_layer_ids.clear();
    }

    /// Resolve LayerIds to a normalized index range (min, max) using the given layer array.
    /// Returns `None` if neither start nor end layer ID is set or found.
    pub fn layer_index_range(&self, layers: &[crate::layer::Layer]) -> Option<(usize, usize)> {
        let start_idx = self
            .start_layer_id
            .as_ref()
            .and_then(|id| layers.iter().position(|l| l.layer_id == *id));
        let end_idx = self
            .end_layer_id
            .as_ref()
            .and_then(|id| layers.iter().position(|l| l.layer_id == *id));

        match (start_idx, end_idx) {
            (Some(s), Some(e)) => Some((s.min(e), s.max(e))),
            (Some(s), None) => Some((s, s)),
            (None, Some(e)) => Some((e, e)),
            (None, None) => None,
        }
    }
}
