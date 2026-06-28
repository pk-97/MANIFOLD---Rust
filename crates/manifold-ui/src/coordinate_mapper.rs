// Converts between timeline beats and UI pixel coordinates.
// Core utility for positioning clips and playhead on the timeline.
// Zoom is in pixels per beat (BPM-independent).
//
// Mechanical 1:1 port of Unity CoordinateMapper.cs.
// Shared by viewport, layer headers, and hit tester.

use crate::color;
use crate::snap;
use crate::transform::Axis;
use crate::view::UiLayer;
use manifold_foundation::Beats;

/// Track-row height presets (§24 5d). A content track is sized by one of these
/// named tiers, chosen by its display *state* — never by its layer *type*. The
/// type is shown by a badge in the header, not by giving a generator a taller
/// row. Groups (container rows) and hidden children sit outside these tiers; see
/// [`CoordinateMapper::layer_height`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackHeight {
    /// A slim header strip — name + button row, no detail controls.
    Collapsed,
    /// The default expanded track (clip bodies + content).
    Normal,
    /// A roomier track for larger previews. Reserved for a future per-layer tall
    /// mode; defined so the height vocabulary is complete.
    Tall,
}

impl TrackHeight {
    /// The preset for a content track given its collapse state.
    #[inline]
    pub fn for_collapsed(is_collapsed: bool) -> Self {
        if is_collapsed {
            TrackHeight::Collapsed
        } else {
            TrackHeight::Normal
        }
    }

    /// The pixel height for this preset.
    #[inline]
    pub fn px(self) -> f32 {
        match self {
            TrackHeight::Collapsed => color::COLLAPSED_TRACK_HEIGHT,
            TrackHeight::Normal => color::TRACK_HEIGHT,
            TrackHeight::Tall => color::TALL_TRACK_HEIGHT,
        }
    }
}

pub struct CoordinateMapper {
    pixels_per_beat: f32,
    scroll_offset_x: f32, // in pixels (matches Unity's ScrollOffsetX)
    layer_y_offsets: Vec<f32>,
    layer_heights: Vec<f32>,
    total_content_height: f32,
}

impl Default for CoordinateMapper {
    fn default() -> Self {
        Self::new()
    }
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

    /// The beat↔pixel mapping as a shared affine [`Axis`]: scale is
    /// pixels-per-beat, the offset is the negated scroll (`screen = beat·ppb −
    /// scroll`). Every X conversion below expresses this one axis.
    #[inline]
    fn x_axis(&self) -> Axis {
        Axis::new(self.pixels_per_beat, -self.scroll_offset_x)
    }

    /// Convert beat position to scroll-adjusted pixel X.
    /// Unity line 48-50.
    pub fn beat_to_pixel(&self, beat: Beats) -> f32 {
        self.x_axis().to_screen(beat.as_f32())
    }

    /// Convert pixel X position to beat.
    /// Unity line 56-58.
    pub fn pixel_to_beat(&self, pixel_x: f32) -> Beats {
        Beats::from_f32(self.x_axis().to_logical(pixel_x))
    }

    /// Convert beat to pixel X in content space (not scroll-adjusted).
    /// Use for positioning elements that are children of scrollable content.
    /// Unity line 65-67.
    pub fn beat_to_pixel_absolute(&self, beat: Beats) -> f32 {
        // Same axis with no scroll offset.
        Axis::new(self.pixels_per_beat, 0.0).to_screen(beat.as_f32())
    }

    /// Convert beat duration to pixel width.
    /// Unity line 73-75.
    pub fn beat_duration_to_width(&self, beats: Beats) -> f32 {
        self.x_axis().span_to_screen(beats.as_f32())
    }

    /// Convert pixel width to beat duration.
    /// Unity line 81-83.
    pub fn width_to_beat_duration(&self, width: f32) -> Beats {
        Beats::from_f32(self.x_axis().span_to_logical(width))
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

    /// Index of the [`color::ZOOM_LEVELS`] entry nearest the current zoom. Handles
    /// an off-grid `pixels_per_beat` (continuous scroll-zoom leaves the zoom
    /// between discrete levels), so the +/- buttons resume from where the view
    /// actually is, not a stale index.
    pub fn nearest_zoom_index(&self) -> usize {
        let ppb = self.pixels_per_beat;
        color::ZOOM_LEVELS
            .iter()
            .enumerate()
            .min_by(|(_, a), (_, b)| {
                (**a - ppb)
                    .abs()
                    .partial_cmp(&(**b - ppb).abs())
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|(i, _)| i)
            .unwrap_or(color::DEFAULT_ZOOM_INDEX)
    }

    /// New ppb after stepping `delta` discrete zoom levels from the nearest current
    /// level (the +/- buttons and keyboard step in fixed notches). Clamped to the
    /// level range.
    pub fn zoom_level_stepped(&self, delta: i32) -> f32 {
        let n = color::ZOOM_LEVELS.len() as i32;
        let idx = (self.nearest_zoom_index() as i32 + delta).clamp(0, n - 1) as usize;
        color::ZOOM_LEVELS[idx]
    }

    /// New ppb after a continuous multiplicative zoom by `factor`, clamped to the
    /// [`color::ZOOM_LEVELS`] range. Used by cursor-anchored scroll-wheel zoom so
    /// zoom is smooth, not a jump between ten fixed steps (§24 5e).
    pub fn zoom_continuous(&self, factor: f32) -> f32 {
        let min = color::ZOOM_LEVELS[0];
        let max = color::ZOOM_LEVELS[color::ZOOM_LEVELS.len() - 1];
        (self.pixels_per_beat * factor).clamp(min, max)
    }

    /// Calculate zoom level to fit timeline duration in viewport width.
    /// Unity line 109-115.
    pub fn calculate_fit_zoom(&self, timeline_beats: Beats, viewport_width: f32) -> f32 {
        if timeline_beats.as_f32() <= 0.0 || viewport_width <= 0.0 {
            return self.pixels_per_beat;
        }
        viewport_width / timeline_beats.as_f32()
    }

    /// Get content width needed for timeline duration at current zoom.
    /// Unity line 120-123.
    pub fn get_content_width(&self, timeline_beats: Beats) -> f32 {
        self.beat_duration_to_width(timeline_beats)
    }

    // ── Y-axis layout (variable track heights) ──────────────────────

    /// Rebuild Y-axis layout arrays. Call once per RebuildTimeline, before BuildTrack loop.
    /// From Unity CoordinateMapper.RebuildYLayout (lines 141-181).
    ///
    /// Height rules — see [`CoordinateMapper::layer_height`].
    pub fn rebuild_y_layout(&mut self, layers: &[UiLayer]) {
        let count = layers.len();
        self.layer_y_offsets.resize(count, 0.0);
        self.layer_heights.resize(count, 0.0);

        let mut y = 0.0f32;
        for i in 0..count {
            let height = Self::layer_height(layers, i);
            self.layer_y_offsets[i] = y;
            self.layer_heights[i] = height;
            y += height;
        }
        self.total_content_height = y;
    }

    /// The single source of truth for one layer's track height.
    ///
    /// This is THE height rule — the viewport's bitmap sizing, the layer
    /// headers' row heights, and this mapper's Y-layout all flow from here, so
    /// they cannot disagree. (Previously copied verbatim in three places; see
    /// `docs/TIMELINE_API_DESIGN.md` §3.4.)
    ///
    /// Two structural cases, then one preset (§24 5d) — the height is chosen by
    /// the layer's display *state*, never by its *type* (type is shown by a badge
    /// in the header, not by giving generators a taller row):
    /// - Child of a collapsed parent → 0 (hidden)
    /// - Group (any state) → a fixed container-header height (it shows no clips)
    /// - Otherwise → [`TrackHeight::Collapsed`] when collapsed, else
    ///   [`TrackHeight::Normal`] — the same for video / generator / audio / text.
    pub fn layer_height(layers: &[UiLayer], index: usize) -> f32 {
        let layer = match layers.get(index) {
            Some(l) => l,
            None => return TrackHeight::Normal.px(),
        };

        // Hidden: a child of a collapsed parent.
        if layer.parent_layer_id.is_some() {
            let parent = find_parent_in_list(layers, layer.parent_layer_id.as_deref());
            if parent.is_some_and(|p| p.is_collapsed) {
                return 0.0;
            }
        }

        // A group is a container row, not a content track — one fixed height.
        if layer.is_group() {
            return color::GROUP_TRACK_HEIGHT;
        }

        // Content track: one preset, selected by collapse state alone.
        TrackHeight::for_collapsed(layer.is_collapsed).px()
    }

    /// Get the cumulative Y offset for a layer (top of that layer's track row).
    /// Unity line 186-191.
    pub fn get_layer_y_offset(&self, layer_index: usize) -> f32 {
        self.layer_y_offsets
            .get(layer_index)
            .copied()
            .unwrap_or(layer_index as f32 * color::TRACK_HEIGHT)
    }

    /// Get the height of a layer's track row (0 for hidden children of collapsed groups).
    /// Unity line 196-201.
    pub fn get_layer_height(&self, layer_index: usize) -> f32 {
        self.layer_heights
            .get(layer_index)
            .copied()
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
        (0..self.layer_y_offsets.len())
            .rev()
            .find(|&i| y_in_tracks >= self.layer_y_offsets[i] && self.layer_heights[i] > 0.0)
    }

    /// Number of layers in the current Y layout.
    pub fn layer_count(&self) -> usize {
        self.layer_y_offsets.len()
    }

    /// Set Y layout from raw height values. Test utility — bypasses Layer struct.
    #[cfg(test)]
    pub fn set_layout(&mut self, heights: &[f32]) {
        let count = heights.len();
        self.layer_y_offsets.resize(count, 0.0);
        self.layer_heights.resize(count, 0.0);
        let mut y = 0.0f32;
        for (i, &h) in heights.iter().enumerate() {
            self.layer_y_offsets[i] = y;
            self.layer_heights[i] = h;
            y += h;
        }
        self.total_content_height = y;
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
    pub fn snap_beat_to_grid(&self, beat: Beats, beats_per_bar: u32) -> Beats {
        let interval = self.get_grid_interval_beats(beats_per_bar);
        snap::snap_beat_to_grid(beat, Beats::from_f32(interval)).max(Beats::ZERO)
    }

    /// Floor a beat value to the LEFT EDGE of the grid cell.
    /// Used for placement operations (double-click clip creation) where the click
    /// should land in the grid cell the cursor is inside, not snap to nearest line.
    /// Unity line 262-266.
    pub fn floor_beat_to_grid(&self, beat: Beats, beats_per_bar: u32) -> Beats {
        let interval = self.get_grid_interval_beats(beats_per_bar);
        if interval <= 0.0 {
            return beat;
        }
        Beats(((beat.0 / interval as f64).floor() * interval as f64).max(0.0))
    }
}

/// Linear search for parent layer by LayerId.
/// Unity CoordinateMapper.FindParentInList (lines 218-225).
fn find_parent_in_list<'a>(layers: &'a [UiLayer], parent_id: Option<&str>) -> Option<&'a UiLayer> {
    let parent_id = parent_id?;
    layers.iter().find(|l| l.layer_id == parent_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::LayerType;
    use manifold_foundation::LayerId;

    fn make_layer(name: &str, layer_type: LayerType, _index: i32) -> UiLayer {
        UiLayer {
            layer_id: LayerId::new(name),
            parent_layer_id: None,
            layer_type,
            is_collapsed: false,
        }
    }

    // ── Beat ↔ Pixel conversions (Unity CoordinateMapperTests.cs) ────

    #[test]
    fn default_zoom_is_120() {
        let mapper = CoordinateMapper::new();
        assert_eq!(mapper.pixels_per_beat(), 120.0);
    }

    #[test]
    fn beat_to_pixel_default_zoom() {
        let mapper = CoordinateMapper::new();
        // Default zoom is ZoomLevels[7] = 120 ppb, scroll = 0
        let pixel = mapper.beat_to_pixel(Beats::from_f32(4.0));
        assert!((pixel - 4.0 * 120.0).abs() < 0.001);
    }

    #[test]
    fn beat_to_pixel_with_scroll() {
        let mut mapper = CoordinateMapper::new();
        mapper.set_scroll_offset_x(100.0);
        let pixel = mapper.beat_to_pixel(Beats::from_f32(4.0));
        assert!((pixel - (4.0 * 120.0 - 100.0)).abs() < 0.001);
    }

    #[test]
    fn pixel_to_beat_roundtrip() {
        let mut mapper = CoordinateMapper::new();
        mapper.set_scroll_offset_x(50.0);
        let pixel = mapper.beat_to_pixel(Beats::from_f32(7.5));
        let beat = mapper.pixel_to_beat(pixel);
        assert!((beat.as_f32() - 7.5).abs() < 0.001);
    }

    #[test]
    fn beat_to_pixel_absolute_ignores_scroll() {
        let mut mapper = CoordinateMapper::new();
        mapper.set_scroll_offset_x(200.0);
        let absolute = mapper.beat_to_pixel_absolute(Beats::from_f32(4.0));
        let scrolled = mapper.beat_to_pixel(Beats::from_f32(4.0));

        assert!((absolute - 4.0 * 120.0).abs() < 0.001);
        assert!((scrolled - (absolute - 200.0)).abs() < 0.001);
    }

    #[test]
    fn duration_width_roundtrip() {
        let mapper = CoordinateMapper::new();
        let beats = Beats::from_f32(3.5);
        let width = mapper.beat_duration_to_width(beats);
        let result = mapper.width_to_beat_duration(width);
        assert!((result.as_f32() - beats.as_f32()).abs() < 0.001);
    }

    // ── Zoom management ──────────────────────────────────────────────

    #[test]
    fn set_zoom_clamps_minimum() {
        let mut mapper = CoordinateMapper::new();
        mapper.set_zoom(0.5);
        assert!((mapper.pixels_per_beat() - 1.0).abs() < 0.001);

        mapper.set_zoom(-10.0);
        assert!((mapper.pixels_per_beat() - 1.0).abs() < 0.001);
    }

    #[test]
    fn set_zoom_by_index_clamps() {
        let mut mapper = CoordinateMapper::new();
        // ZoomLevels: [1, 2, 5, 10, 20, 40, 80, 120, 200, 400]
        mapper.set_zoom_by_index(0);
        assert!((mapper.pixels_per_beat() - 1.0).abs() < 0.001);

        mapper.set_zoom_by_index(100);
        assert!((mapper.pixels_per_beat() - 400.0).abs() < 0.001);

        mapper.set_zoom_by_index(2);
        assert!((mapper.pixels_per_beat() - 5.0).abs() < 0.001);
    }

    // ── Fit zoom ─────────────────────────────────────────────────────

    #[test]
    fn calculate_fit_zoom() {
        let mapper = CoordinateMapper::new();
        let fit = mapper.calculate_fit_zoom(Beats::from_f32(16.0), 800.0);
        assert!((fit - 800.0 / 16.0).abs() < 0.001);
    }

    #[test]
    fn calculate_fit_zoom_zero_input() {
        let mapper = CoordinateMapper::new();
        let current = mapper.pixels_per_beat();
        assert!((mapper.calculate_fit_zoom(Beats::from_f32(0.0), 800.0) - current).abs() < 0.001);
        assert!((mapper.calculate_fit_zoom(Beats::from_f32(16.0), 0.0) - current).abs() < 0.001);
    }

    // ── Y-axis layout ────────────────────────────────────────────────

    #[test]
    fn rebuild_y_layout_basic() {
        let mut mapper = CoordinateMapper::new();
        let layers = vec![
            make_layer("A", LayerType::Video, 0),
            make_layer("B", LayerType::Video, 1),
            make_layer("C", LayerType::Generator, 2),
        ];
        mapper.rebuild_y_layout(&layers);

        assert!((mapper.get_layer_y_offset(0) - 0.0).abs() < 0.001);
        assert!((mapper.get_layer_y_offset(1) - 200.0).abs() < 0.001);
        assert!((mapper.get_layer_y_offset(2) - 400.0).abs() < 0.001);
        assert!((mapper.get_layer_height(0) - 200.0).abs() < 0.001);
        assert!((mapper.get_layer_height(1) - 200.0).abs() < 0.001);
        assert!((mapper.get_layer_height(2) - 200.0).abs() < 0.001);
        assert!((mapper.total_content_height() - 600.0).abs() < 0.001);
    }

    #[test]
    fn rebuild_y_layout_collapsed() {
        let mut mapper = CoordinateMapper::new();

        let mut video = make_layer("V", LayerType::Video, 0);
        video.is_collapsed = true;
        let mut gen_layer = make_layer("G", LayerType::Generator, 1);
        gen_layer.is_collapsed = true;
        let mut group = make_layer("Grp", LayerType::Group, 2);
        group.is_collapsed = true;

        let layers = vec![video, gen_layer, group];
        mapper.rebuild_y_layout(&layers);

        // §24 5d: collapse is sized by state, not type — collapsed video AND
        // collapsed generator are both Collapsed (58); the type is shown by a
        // badge. A group is a container row → its fixed 70.
        assert!((mapper.get_layer_height(0) - 58.0).abs() < 0.001);
        assert!((mapper.get_layer_height(1) - 58.0).abs() < 0.001);
        assert!((mapper.get_layer_height(2) - 70.0).abs() < 0.001);
        assert!((mapper.total_content_height() - 186.0).abs() < 0.001);
    }

    #[test]
    fn rebuild_y_layout_hidden_child() {
        let mut mapper = CoordinateMapper::new();

        let mut group = make_layer("Grp", LayerType::Group, 0);
        group.is_collapsed = true;
        let group_id = group.layer_id.clone();

        let mut child = make_layer("Child", LayerType::Video, 1);
        child.parent_layer_id = Some(group_id);

        let layers = vec![group, child];
        mapper.rebuild_y_layout(&layers);

        // Collapsed group → 70, child of collapsed parent → 0 (hidden)
        assert!((mapper.get_layer_height(0) - 70.0).abs() < 0.001);
        assert!((mapper.get_layer_height(1) - 0.0).abs() < 0.001);
        assert!((mapper.total_content_height() - 70.0).abs() < 0.001);
    }

    #[test]
    fn layer_height_is_the_single_rule() {
        // The extracted rule must produce exactly what rebuild_y_layout stores,
        // for every layer — proving there is one height computation, not two.
        let mut video = make_layer("V", LayerType::Video, 0);
        video.is_collapsed = true;
        let gen_expanded = make_layer("G", LayerType::Generator, 1);
        let mut group = make_layer("Grp", LayerType::Group, 2);
        group.is_collapsed = true;
        let group_id = group.layer_id.clone();
        let mut hidden_child = make_layer("Child", LayerType::Video, 3);
        hidden_child.parent_layer_id = Some(group_id);

        let layers = vec![video, gen_expanded, group, hidden_child];
        let mut mapper = CoordinateMapper::new();
        mapper.rebuild_y_layout(&layers);

        for i in 0..layers.len() {
            assert_eq!(
                CoordinateMapper::layer_height(&layers, i),
                mapper.get_layer_height(i),
                "layer_height({i}) must equal the stored Y-layout height",
            );
        }
        // Spot-check the actual values the rule yields.
        assert_eq!(CoordinateMapper::layer_height(&layers, 0), 58.0); // collapsed video
        assert_eq!(CoordinateMapper::layer_height(&layers, 1), 200.0); // expanded generator
        assert_eq!(CoordinateMapper::layer_height(&layers, 2), 70.0); // collapsed group
        assert_eq!(CoordinateMapper::layer_height(&layers, 3), 0.0); // hidden child
    }

    #[test]
    fn collapsed_height_is_type_independent() {
        // §24 5d: collapse height is identical for every content type — the badge
        // carries type, so the header no longer restructures (and re-heights) by
        // it. This is the regression guard for the old generator-only 62.
        for t in [LayerType::Video, LayerType::Generator, LayerType::Audio] {
            let mut l = make_layer("L", t, 0);
            l.is_collapsed = true;
            let layers = vec![l];
            assert_eq!(
                CoordinateMapper::layer_height(&layers, 0),
                TrackHeight::Collapsed.px(),
                "collapsed {t:?} must use the Collapsed preset, not a per-type height",
            );
        }
    }

    #[test]
    fn track_height_presets_ordered() {
        assert_eq!(TrackHeight::for_collapsed(true), TrackHeight::Collapsed);
        assert_eq!(TrackHeight::for_collapsed(false), TrackHeight::Normal);
        assert!(TrackHeight::Collapsed.px() < TrackHeight::Normal.px());
        // Two-tier system: Normal IS the expanded tier (200). Tall is reserved
        // and currently equal to Normal — no longer strictly greater.
        assert!(TrackHeight::Normal.px() <= TrackHeight::Tall.px());
    }

    #[test]
    fn get_layer_at_y_hit_test() {
        let mut mapper = CoordinateMapper::new();
        mapper.set_layout(&[140.0, 140.0, 140.0]);

        assert_eq!(mapper.get_layer_at_y(0.0), Some(0));
        assert_eq!(mapper.get_layer_at_y(70.0), Some(0));
        assert_eq!(mapper.get_layer_at_y(140.0), Some(1));
        assert_eq!(mapper.get_layer_at_y(280.0), Some(2));
        assert_eq!(mapper.get_layer_at_y(419.0), Some(2));
    }

    #[test]
    fn get_layer_at_y_skips_zero_height() {
        let mut mapper = CoordinateMapper::new();
        // Layer 1 has height 0 (hidden child of collapsed group)
        mapper.set_layout(&[140.0, 0.0, 140.0]);

        // Y=140 should land on layer 2 (index 2), not the hidden layer 1
        assert_eq!(mapper.get_layer_at_y(140.0), Some(2));
        // Y in range of hidden layer should still find previous visible layer
        assert_eq!(mapper.get_layer_at_y(139.0), Some(0));
    }
}
