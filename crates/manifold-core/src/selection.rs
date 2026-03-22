use std::collections::HashSet;
use crate::LayerId;

/// Region-based selection on the timeline.
#[derive(Debug, Clone, Default)]
pub struct SelectionRegion {
    pub start_beat: f32,
    pub end_beat: f32,
    pub start_layer_index: i32,
    pub end_layer_index: i32,
    pub is_active: bool,

    // ── LayerId-based fields (stable identity) ──
    pub start_layer_id: Option<LayerId>,
    pub end_layer_id: Option<LayerId>,
    pub selected_layer_ids: HashSet<LayerId>,
}

impl SelectionRegion {
    #[allow(dead_code)]
    pub fn new(start_beat: f32, end_beat: f32, start_layer: i32, end_layer: i32) -> Self {
        Self {
            start_beat,
            end_beat,
            start_layer_index: start_layer,
            end_layer_index: end_layer,
            is_active: true,
            start_layer_id: None,
            end_layer_id: None,
            selected_layer_ids: HashSet::new(),
        }
    }

    /// Create a region with stable LayerIds. Computes index caches from the layer array.
    pub fn new_with_ids(
        start_beat: f32,
        end_beat: f32,
        start_layer_id: LayerId,
        end_layer_id: LayerId,
        layers: &[crate::layer::Layer],
    ) -> Self {
        let start_idx = layers.iter().position(|l| l.layer_id == start_layer_id)
            .map(|i| i as i32).unwrap_or(0);
        let end_idx = layers.iter().position(|l| l.layer_id == end_layer_id)
            .map(|i| i as i32).unwrap_or(0);

        let lo = start_idx.min(end_idx) as usize;
        let hi = start_idx.max(end_idx) as usize;
        let mut selected = HashSet::new();
        for layer in layers.iter().skip(lo).take(hi - lo + 1) {
            selected.insert(layer.layer_id.clone());
        }

        Self {
            start_beat,
            end_beat,
            start_layer_index: start_idx,
            end_layer_index: end_idx,
            is_active: true,
            start_layer_id: Some(start_layer_id),
            end_layer_id: Some(end_layer_id),
            selected_layer_ids: selected,
        }
    }

    pub fn contains_beat(&self, beat: f32) -> bool {
        beat >= self.start_beat && beat < self.end_beat
    }

    #[allow(dead_code)]
    pub fn contains_layer(&self, layer_index: i32) -> bool {
        let min = self.start_layer_index.min(self.end_layer_index);
        let max = self.start_layer_index.max(self.end_layer_index);
        layer_index >= min && layer_index <= max
    }

    /// Check if a layer is in this region by LayerId (HashSet lookup).
    pub fn contains_layer_id(&self, id: &LayerId) -> bool {
        self.selected_layer_ids.contains(id)
    }

    pub fn duration_beats(&self) -> f32 {
        self.end_beat - self.start_beat
    }

    /// Set the selection region (index-based, backward compat).
    #[allow(dead_code)]
    pub fn set(&mut self, start_beat: f32, end_beat: f32, start_layer: i32, end_layer: i32) {
        self.start_beat = start_beat;
        self.end_beat = end_beat;
        self.start_layer_index = start_layer;
        self.end_layer_index = end_layer;
        self.is_active = true;
        // Clear LayerId fields — caller should use set_with_ids instead
        self.start_layer_id = None;
        self.end_layer_id = None;
        self.selected_layer_ids.clear();
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
            .map(|i| i as i32).unwrap_or(0);
        let end_idx = layers.iter().position(|l| l.layer_id == end_layer_id)
            .map(|i| i as i32).unwrap_or(0);

        self.start_beat = start_beat;
        self.end_beat = end_beat;
        self.start_layer_index = start_idx;
        self.end_layer_index = end_idx;
        self.is_active = true;

        let lo = start_idx.min(end_idx) as usize;
        let hi = start_idx.max(end_idx) as usize;
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
        self.start_layer_index = 0;
        self.end_layer_index = 0;
        self.is_active = false;
        self.start_layer_id = None;
        self.end_layer_id = None;
        self.selected_layer_ids.clear();
    }

    /// Get normalized layer range (min, max).
    pub fn layer_range(&self) -> (i32, i32) {
        let min = self.start_layer_index.min(self.end_layer_index);
        let max = self.start_layer_index.max(self.end_layer_index);
        (min, max)
    }
}

/// Narrow interface for setting/clearing the selection region.
/// Port of Unity ISelectionRegionTarget (SelectionRegion.cs lines 22-26).
pub trait SelectionRegionTarget {
    fn set_region(&mut self, start_beat: f32, end_beat: f32, start_layer: i32, end_layer: i32, layers: &[crate::layer::Layer]);
    fn clear_region(&mut self);

    /// Set region with stable LayerIds. Default impl falls back to index-based set_region.
    fn set_region_with_ids(
        &mut self,
        start_beat: f32,
        end_beat: f32,
        start_layer: i32,
        end_layer: i32,
        _start_layer_id: LayerId,
        _end_layer_id: LayerId,
        layers: &[crate::layer::Layer],
    ) {
        self.set_region(start_beat, end_beat, start_layer, end_layer, layers);
    }
}
