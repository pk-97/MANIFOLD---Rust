use std::collections::HashSet;
use crate::LayerId;

/// Region-based selection on the timeline.
#[derive(Debug, Clone, Default)]
pub struct SelectionRegion {
    pub start_beat: f32,
    pub end_beat: f32,
    pub is_active: bool,

    // ── LayerId-based fields (stable identity) ──
    pub start_layer_id: Option<LayerId>,
    pub end_layer_id: Option<LayerId>,
    pub selected_layer_ids: HashSet<LayerId>,
}

impl SelectionRegion {
    /// Create a region with stable LayerIds. Computes `selected_layer_ids` from the layer array.
    pub fn new_with_ids(
        start_beat: f32,
        end_beat: f32,
        start_layer_id: LayerId,
        end_layer_id: LayerId,
        layers: &[crate::layer::Layer],
    ) -> Self {
        let start_idx = layers.iter().position(|l| l.layer_id == start_layer_id)
            .unwrap_or(0);
        let end_idx = layers.iter().position(|l| l.layer_id == end_layer_id)
            .unwrap_or(0);

        let lo = start_idx.min(end_idx);
        let hi = start_idx.max(end_idx);
        let mut selected = HashSet::new();
        for layer in layers.iter().skip(lo).take(hi - lo + 1) {
            selected.insert(layer.layer_id.clone());
        }

        Self {
            start_beat,
            end_beat,
            is_active: true,
            start_layer_id: Some(start_layer_id),
            end_layer_id: Some(end_layer_id),
            selected_layer_ids: selected,
        }
    }

    pub fn contains_beat(&self, beat: f32) -> bool {
        beat >= self.start_beat && beat < self.end_beat
    }

    /// Check if a layer is in this region by LayerId (HashSet lookup).
    pub fn contains_layer_id(&self, id: &LayerId) -> bool {
        self.selected_layer_ids.contains(id)
    }

    pub fn duration_beats(&self) -> f32 {
        self.end_beat - self.start_beat
    }

    /// Set the selection region with stable LayerIds.
    pub fn set_with_ids(
        &mut self,
        start_beat: f32,
        end_beat: f32,
        start_layer_id: LayerId,
        end_layer_id: LayerId,
        layers: &[crate::layer::Layer],
    ) {
        let start_idx = layers.iter().position(|l| l.layer_id == start_layer_id)
            .unwrap_or(0);
        let end_idx = layers.iter().position(|l| l.layer_id == end_layer_id)
            .unwrap_or(0);

        self.start_beat = start_beat;
        self.end_beat = end_beat;
        self.is_active = true;

        let lo = start_idx.min(end_idx);
        let hi = start_idx.max(end_idx);
        self.selected_layer_ids.clear();
        for layer in layers.iter().skip(lo).take(hi - lo + 1) {
            self.selected_layer_ids.insert(layer.layer_id.clone());
        }
        self.start_layer_id = Some(start_layer_id);
        self.end_layer_id = Some(end_layer_id);
    }

    /// Clear the selection region.
    pub fn clear(&mut self) {
        self.start_beat = 0.0;
        self.end_beat = 0.0;
        self.is_active = false;
        self.start_layer_id = None;
        self.end_layer_id = None;
        self.selected_layer_ids.clear();
    }

    /// Resolve LayerIds to a normalized index range (min, max) using the given layer array.
    /// Returns `None` if neither start nor end layer ID is set or found.
    pub fn layer_index_range(&self, layers: &[crate::layer::Layer]) -> Option<(usize, usize)> {
        let start_idx = self.start_layer_id.as_ref()
            .and_then(|id| layers.iter().position(|l| l.layer_id == *id));
        let end_idx = self.end_layer_id.as_ref()
            .and_then(|id| layers.iter().position(|l| l.layer_id == *id));

        match (start_idx, end_idx) {
            (Some(s), Some(e)) => Some((s.min(e), s.max(e))),
            (Some(s), None) => Some((s, s)),
            (None, Some(e)) => Some((e, e)),
            (None, None) => None,
        }
    }
}

/// Narrow interface for setting/clearing the selection region.
/// Port of Unity ISelectionRegionTarget (SelectionRegion.cs lines 22-26).
pub trait SelectionRegionTarget {
    fn set_region(
        &mut self,
        start_beat: f32,
        end_beat: f32,
        start_layer_id: LayerId,
        end_layer_id: LayerId,
        layers: &[crate::layer::Layer],
    );
    fn clear_region(&mut self);
}
