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
}
