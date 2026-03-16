/// Converts between timeline beats and UI pixel coordinates.
/// Core utility for positioning clips and playhead on the timeline.
/// Zoom is in pixels per beat (BPM-independent).
///
/// Mechanical 1:1 port of Unity CoordinateMapper.cs.
/// Shared by viewport, layer headers, and hit tester.

use crate::color;
use crate::snap;
use manifold_core::layer::Layer;
use manifold_core::types::LayerType;

pub struct CoordinateMapper {
    pixels_per_beat: f32,
    scroll_offset_x: f32, // in pixels (matches Unity's ScrollOffsetX)
    layer_y_offsets: Vec<f32>,
    layer_heights: Vec<f32>,
    total_content_height: f32,
}

impl CoordinateMapper {
    /// Create mapper with default zoom level.
    /// From Unity CoordinateMapper() constructor (line 20-24).
    pub fn new() -> Self {
        Self {
            pixels_per_beat: color::ZOOM_LEVELS[color::DEFAULT_ZOOM_INDEX],
            scroll_offset_x: 0.0,
            layer_y_offsets: Vec::new(),
            layer_heights: Vec::new(),
            total_content_height: 0.0,
        }
    }

    // ── Properties ──────────────────────────────────────────────────

    pub fn pixels_per_beat(&self) -> f32 {
        self.pixels_per_beat
    }

    pub fn scroll_offset_x(&self) -> f32 {
        self.scroll_offset_x
    }

    pub fn set_scroll_offset_x(&mut self, x: f32) {
        self.scroll_offset_x = x;
    }

    pub fn total_content_height(&self) -> f32 {
        self.total_content_height
    }

    // ── Beat-based conversions (primary) ────────────────────────────

    /// Convert beat position to scroll-adjusted pixel X.
    /// Unity line 48-50.
    pub fn beat_to_pixel(&self, beat: f32) -> f32 {
        beat * self.pixels_per_beat - self.scroll_offset_x
    }

    /// Convert pixel X position to beat.
    /// Unity line 56-58.
    pub fn pixel_to_beat(&self, pixel_x: f32) -> f32 {
        (pixel_x + self.scroll_offset_x) / self.pixels_per_beat
    }

    /// Convert beat to pixel X in content space (not scroll-adjusted).
    /// Use for positioning elements that are children of scrollable content.
    /// Unity line 65-67.
    pub fn beat_to_pixel_absolute(&self, beat: f32) -> f32 {
        beat * self.pixels_per_beat
    }

    /// Convert beat duration to pixel width.
    /// Unity line 73-75.
    pub fn beat_duration_to_width(&self, beats: f32) -> f32 {
        beats * self.pixels_per_beat
    }

    /// Convert pixel width to beat duration.
    /// Unity line 81-83.
    pub fn width_to_beat_duration(&self, width: f32) -> f32 {
        width / self.pixels_per_beat
    }

    // ── Zoom management ─────────────────────────────────────────────

    /// Set zoom level by index into ZOOM_LEVELS array.
    /// Unity line 93-96.
    pub fn set_zoom_by_index(&mut self, zoom_index: usize) {
        let idx = zoom_index.min(color::ZOOM_LEVELS.len() - 1);
        self.pixels_per_beat = color::ZOOM_LEVELS[idx];
    }

    /// Set zoom level directly (pixels per beat, minimum 1.0).
    /// Unity line 101-104.
    pub fn set_zoom(&mut self, new_ppb: f32) {
        self.pixels_per_beat = new_ppb.max(1.0);
    }

    /// Calculate zoom level to fit timeline duration in viewport width.
    /// Unity line 109-115.
    pub fn calculate_fit_zoom(&self, timeline_beats: f32, viewport_width: f32) -> f32 {
        if timeline_beats <= 0.0 || viewport_width <= 0.0 {
            return self.pixels_per_beat;
        }
        viewport_width / timeline_beats
    }

    /// Get content width needed for timeline duration at current zoom.
    /// Unity line 120-123.
    pub fn get_content_width(&self, timeline_beats: f32) -> f32 {
        self.beat_duration_to_width(timeline_beats)
    }

    // ── Y-axis layout (variable track heights) ──────────────────────

    /// Rebuild Y-axis layout arrays. Call once per RebuildTimeline, before BuildTrack loop.
    /// From Unity CoordinateMapper.RebuildYLayout (lines 141-181).
    ///
    /// Height rules:
    /// - Child of collapsed parent → 0 (hidden)
    /// - Collapsed group → CollapsedGroupTrackHeight (70)
    /// - Collapsed generator → CollapsedGeneratorTrackHeight (62)
    /// - Collapsed regular → CollapsedTrackHeight (48)
    /// - Expanded (all types) → TrackHeight (140)
    pub fn rebuild_y_layout(&mut self, layers: &[Layer]) {
        let count = layers.len();
        self.layer_y_offsets.resize(count, 0.0);
        self.layer_heights.resize(count, 0.0);

        let mut y = 0.0f32;
        for i in 0..count {
            let layer = &layers[i];
            let height;

            if layer.parent_layer_id.is_some() {
                // Child layer — check parent collapsed state
                let parent = find_parent_in_list(layers, layer.parent_layer_id.as_deref());
                height = if parent.map_or(false, |p| p.is_collapsed) {
                    0.0 // Hidden: parent is collapsed
                } else {
                    color::TRACK_HEIGHT
                };
            } else if layer.is_group() && layer.is_collapsed {
                height = color::COLLAPSED_GROUP_TRACK_HEIGHT;
            } else if !layer.is_group() && layer.is_collapsed {
                height = if layer.layer_type == LayerType::Generator {
                    color::COLLAPSED_GEN_TRACK_HEIGHT
                } else {
                    color::COLLAPSED_TRACK_HEIGHT
                };
            } else {
                height = color::TRACK_HEIGHT;
            }

            self.layer_y_offsets[i] = y;
            self.layer_heights[i] = height;
            y += height;
        }
        self.total_content_height = y;
    }

    /// Get the cumulative Y offset for a layer (top of that layer's track row).
    /// Unity line 186-191.
    pub fn get_layer_y_offset(&self, layer_index: usize) -> f32 {
        self.layer_y_offsets.get(layer_index).copied()
            .unwrap_or(layer_index as f32 * color::TRACK_HEIGHT)
    }

    /// Get the height of a layer's track row (0 for hidden children of collapsed groups).
    /// Unity line 196-201.
    pub fn get_layer_height(&self, layer_index: usize) -> f32 {
        self.layer_heights.get(layer_index).copied()
            .unwrap_or(color::TRACK_HEIGHT)
    }

    /// Hit-test: given Y offset in track space (positive downward from top),
    /// return the layer index at that position. Returns None if out of range.
    /// REVERSE ITERATION — finds topmost visible layer.
    /// Unity line 207-216.
    pub fn get_layer_at_y(&self, y_in_tracks: f32) -> Option<usize> {
        if self.layer_y_offsets.is_empty() {
            return None;
        }
        for i in (0..self.layer_y_offsets.len()).rev() {
            if y_in_tracks >= self.layer_y_offsets[i] && self.layer_heights[i] > 0.0 {
                return Some(i);
            }
        }
        None
    }

    /// Number of layers in the current Y layout.
    pub fn layer_count(&self) -> usize {
        self.layer_y_offsets.len()
    }

    // ── Grid snapping ───────────────────────────────────────────────

    /// Returns the finest musically meaningful grid interval (in beats) for the current zoom.
    /// Delegates to snap.rs which matches Unity thresholds.
    /// Unity line 239-245.
    pub fn get_grid_interval_beats(&self, beats_per_bar: u32) -> f32 {
        snap::grid_interval_for_zoom(self.pixels_per_beat, beats_per_bar as f32)
    }

    /// Snap a beat value to the NEAREST grid line. Result clamped >= 0.
    /// Unity line 251-255.
    pub fn snap_beat_to_grid(&self, beat: f32, beats_per_bar: u32) -> f32 {
        let interval = self.get_grid_interval_beats(beats_per_bar);
        snap::snap_beat_to_grid(beat, interval).max(0.0)
    }

    /// Floor a beat value to the LEFT EDGE of the grid cell.
    /// Used for placement operations (double-click clip creation) where the click
    /// should land in the grid cell the cursor is inside, not snap to nearest line.
    /// Unity line 262-266.
    pub fn floor_beat_to_grid(&self, beat: f32, beats_per_bar: u32) -> f32 {
        let interval = self.get_grid_interval_beats(beats_per_bar);
        if interval <= 0.0 {
            return beat;
        }
        ((beat / interval).floor() * interval).max(0.0)
    }
}

/// Linear search for parent layer by LayerId.
/// Unity CoordinateMapper.FindParentInList (lines 218-225).
fn find_parent_in_list<'a>(layers: &'a [Layer], parent_id: Option<&str>) -> Option<&'a Layer> {
    let parent_id = parent_id?;
    layers.iter().find(|l| l.layer_id == parent_id)
}
