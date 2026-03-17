/// Region-based selection on the timeline.
#[derive(Debug, Clone, Copy, Default)]
pub struct SelectionRegion {
    pub start_beat: f32,
    pub end_beat: f32,
    pub start_layer_index: i32,
    pub end_layer_index: i32,
    pub is_active: bool,
}

impl SelectionRegion {
    pub fn new(start_beat: f32, end_beat: f32, start_layer: i32, end_layer: i32) -> Self {
        Self {
            start_beat,
            end_beat,
            start_layer_index: start_layer,
            end_layer_index: end_layer,
            is_active: true,
        }
    }

    pub fn contains_beat(&self, beat: f32) -> bool {
        beat >= self.start_beat && beat < self.end_beat
    }

    pub fn contains_layer(&self, layer_index: i32) -> bool {
        let min = self.start_layer_index.min(self.end_layer_index);
        let max = self.start_layer_index.max(self.end_layer_index);
        layer_index >= min && layer_index <= max
    }

    pub fn duration_beats(&self) -> f32 {
        self.end_beat - self.start_beat
    }

    /// Set the selection region.
    pub fn set(&mut self, start_beat: f32, end_beat: f32, start_layer: i32, end_layer: i32) {
        self.start_beat = start_beat;
        self.end_beat = end_beat;
        self.start_layer_index = start_layer;
        self.end_layer_index = end_layer;
        self.is_active = true;
    }

    /// Clear the selection region.
    pub fn clear(&mut self) {
        self.start_beat = 0.0;
        self.end_beat = 0.0;
        self.start_layer_index = 0;
        self.end_layer_index = 0;
        self.is_active = false;
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
    fn set_region(&mut self, start_beat: f32, end_beat: f32, start_layer: i32, end_layer: i32);
    fn clear_region(&mut self);
}
