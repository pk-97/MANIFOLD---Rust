use crate::color;
use crate::coordinate_mapper::CoordinateMapper;
use crate::input::{Modifiers, UIEvent};
use crate::layout::ScreenLayout;
use crate::node::*;
use crate::tree::UITree;
use super::{Panel, PanelAction};

// ── Layout constants ────────────────────────────────────────────

const RULER_HEIGHT: f32 = color::RULER_HEIGHT;
const PLAYHEAD_WIDTH: f32 = color::PLAYHEAD_WIDTH;
const INSERT_CURSOR_WIDTH: f32 = 2.0;
const CLIP_VERTICAL_PAD: f32 = 12.0;
const CLIP_LABEL_PAD: f32 = 4.0;
const CLIP_CORNER_RADIUS: f32 = 2.0;
const CLIP_BORDER_WIDTH: f32 = 1.0;
const CLIP_MIN_WIDTH_PX: f32 = color::CLIP_MIN_WIDTH;

/// Center a vertical line of given width at pixel position `px`.
#[inline]
fn centered_line_x(px: f32, width: f32) -> f32 { px - width * 0.5 }

const FONT_SIZE: u16 = 9;
const RULER_FONT_SIZE: u16 = 9;
const RULER_TICK_W: f32 = 1.0;
const RULER_BEAT_TICK_H: f32 = 8.0;
const RULER_BAR_TICK_H: f32 = 14.0;
const RULER_LABEL_H: f32 = 14.0;
const RULER_LABEL_W: f32 = 40.0;
const GRID_LINE_W: f32 = 1.0;

// Maximum nodes to allocate for grid/ruler/clips (avoid unbounded allocation)
const MAX_GRID_LINES: usize = 200;
const MAX_RULER_TICKS: usize = 200;
const MAX_VISIBLE_CLIPS: usize = 500;

// ── Data types ──────────────────────────────────────────────────

/// A clip to be rendered in the timeline viewport.
#[derive(Debug, Clone)]
pub struct ViewportClip {
    pub clip_id: String,
    pub layer_index: usize,
    pub start_beat: f32,
    pub duration_beats: f32,
    pub name: String,
    pub color: Color32,
    pub is_muted: bool,
    pub is_locked: bool,
    pub is_generator: bool,
}

/// Which part of a clip was hit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HitRegion {
    Body,
    TrimLeft,
    TrimRight,
}

/// Result of a clip hit-test in the viewport.
#[derive(Debug, Clone)]
pub struct ClipHitResult {
    pub clip_id: String,
    pub layer_index: usize,
    pub region: HitRegion,
}

/// Region-based selection in the timeline.
#[derive(Debug, Clone, Copy)]
pub struct SelectionRegion {
    pub start_beat: f32,
    pub end_beat: f32,
    pub start_layer: usize,
    pub end_layer: usize,
}

/// Per-layer track info for the viewport.
#[derive(Debug, Clone)]
pub struct TrackInfo {
    pub height: f32,
    pub is_muted: bool,
    pub is_group: bool,
    pub is_collapsed: bool,
    pub accent_color: Option<Color32>,
    /// For group layers: indices of child layers (used for collapsed group preview).
    /// From Unity ViewportManager.GenerateCollapsedGroupTexture.
    pub child_layer_indices: Vec<usize>,
}

impl Default for TrackInfo {
    fn default() -> Self {
        Self {
            height: color::TRACK_HEIGHT,
            is_muted: false,
            is_group: false,
            is_collapsed: false,
            accent_color: None,
            child_layer_indices: Vec::new(),
        }
    }
}

// ── TimelineViewportPanel ───────────────────────────────────────

pub struct TimelineViewportPanel {
    // Shared coordinate mapper (owns zoom, Y-layout, grid snapping).
    // The viewport adds screen-space offset (tracks_rect.x) on top.
    mapper: CoordinateMapper,

    // Viewport-specific scroll state (in beats, not pixels)
    scroll_x_beats: f32,
    scroll_y_px: f32,
    beats_per_bar: u32,

    // Track layout (kept in sync with mapper via set_tracks)
    tracks: Vec<TrackInfo>,
    track_y_offsets: Vec<f32>,    // cumulative Y offsets from tracks
    total_tracks_height: f32,

    // Clip data
    clips: Vec<ViewportClip>,

    // Playback state
    playhead_beat: f32,
    insert_cursor_beat: f32,
    is_playing: bool,
    selection_region: Option<SelectionRegion>,
    selected_clip_ids: Vec<String>,
    hovered_clip_id: Option<String>,

    // Viewport rects
    viewport_rect: Rect,
    ruler_rect: Rect,
    tracks_rect: Rect,

    // Node IDs — fixed elements
    bg_panel_id: i32,
    ruler_bg_id: i32,
    playhead_ruler_id: i32,
    playhead_track_id: i32,
    insert_cursor_ruler_id: i32,
    insert_cursor_track_id: i32,
    selection_region_id: i32,

    // Export range
    export_in_beat: f32,
    export_out_beat: f32,

    // Node IDs — fixed export elements
    export_range_id: i32,
    export_in_marker_id: i32,
    export_out_marker_id: i32,

    // Node IDs — dynamic elements (rebuilt on scroll/zoom)
    ruler_tick_ids: Vec<i32>,
    ruler_label_ids: Vec<i32>,
    grid_line_ids: Vec<i32>,
    track_bg_ids: Vec<i32>,
    clip_bg_ids: Vec<i32>,
    clip_label_ids: Vec<i32>,
    clip_border_ids: Vec<i32>,
    clip_trim_handle_ids: Vec<i32>,

    // Node range
    first_node: usize,
    node_count: usize,

    // Drag interaction state
    drag_mode: ViewportDragMode,

    // Dirty-checking fingerprint to skip unnecessary rebuilds.
    // From Unity LayerBitmapRenderer dirty-checking (lines 99-186).
    cached_fingerprint: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ViewportDragMode {
    None,
    Move,
    TrimLeft,
    TrimRight,
    RegionSelect,
    RulerScrub,
}

impl TimelineViewportPanel {
    pub fn new() -> Self {
        Self {
            mapper: CoordinateMapper::new(),
            scroll_x_beats: 0.0,
            scroll_y_px: 0.0,
            beats_per_bar: 4,
            tracks: Vec::new(),
            track_y_offsets: Vec::new(),
            total_tracks_height: 0.0,
            clips: Vec::new(),
            playhead_beat: 0.0,
            insert_cursor_beat: 0.0,
            is_playing: false,
            selection_region: None,
            selected_clip_ids: Vec::new(),
            hovered_clip_id: None,
            viewport_rect: Rect::ZERO,
            ruler_rect: Rect::ZERO,
            tracks_rect: Rect::ZERO,
            bg_panel_id: -1,
            ruler_bg_id: -1,
            playhead_ruler_id: -1,
            playhead_track_id: -1,
            insert_cursor_ruler_id: -1,
            insert_cursor_track_id: -1,
            selection_region_id: -1,
            export_in_beat: 0.0,
            export_out_beat: 0.0,
            export_range_id: -1,
            export_in_marker_id: -1,
            export_out_marker_id: -1,
            ruler_tick_ids: Vec::new(),
            ruler_label_ids: Vec::new(),
            grid_line_ids: Vec::new(),
            track_bg_ids: Vec::new(),
            clip_bg_ids: Vec::new(),
            clip_label_ids: Vec::new(),
            clip_border_ids: Vec::new(),
            clip_trim_handle_ids: Vec::new(),
            first_node: 0,
            node_count: 0,
            drag_mode: ViewportDragMode::None,
            cached_fingerprint: 0,
        }
    }

    /// Compute a fingerprint of the current viewport state.
    /// If unchanged from cached, the tree rebuild can be skipped.
    /// From Unity LayerBitmapRenderer.ComputeClipFingerprint (lines 332-344).
    pub fn compute_fingerprint(&self) -> u64 {
        let (min_beat, max_beat) = self.visible_beat_range();
        let mut hash = self.clips.len() as u64;
        hash = hash.wrapping_mul(31).wrapping_add(self.mapper.pixels_per_beat().to_bits() as u64);
        hash = hash.wrapping_mul(31).wrapping_add(self.scroll_x_beats.to_bits() as u64);
        hash = hash.wrapping_mul(31).wrapping_add(self.scroll_y_px.to_bits() as u64);
        hash = hash.wrapping_mul(31).wrapping_add(self.tracks.len() as u64);
        hash = hash.wrapping_mul(31).wrapping_add(self.selected_clip_ids.len() as u64);
        hash = hash.wrapping_mul(31).wrapping_add(
            self.hovered_clip_id.as_ref().map(|s| {
                let mut h = 0u64;
                for b in s.bytes() { h = h.wrapping_mul(31).wrapping_add(b as u64); }
                h
            }).unwrap_or(0)
        );
        // Per-visible-clip fingerprint
        for clip in &self.clips {
            let clip_end = clip.start_beat + clip.duration_beats;
            if clip_end <= min_beat || clip.start_beat >= max_beat {
                continue;
            }
            hash = hash.wrapping_mul(31).wrapping_add(clip.start_beat.to_bits() as u64);
            hash = hash.wrapping_mul(31).wrapping_add(clip_end.to_bits() as u64);
            hash = hash.wrapping_mul(31).wrapping_add(clip.is_muted as u64);
            hash = hash.wrapping_mul(31).wrapping_add(clip.is_locked as u64);
        }
        hash
    }

    /// Check if the viewport needs a full tree rebuild based on fingerprint.
    /// Returns true if changed (needs rebuild), false if unchanged (skip).
    pub fn needs_rebuild(&mut self) -> bool {
        let fp = self.compute_fingerprint();
        if fp == self.cached_fingerprint {
            return false;
        }
        self.cached_fingerprint = fp;
        true
    }

    // ── Configuration ─────────────────────────────────────────────

    pub fn set_tracks(&mut self, tracks: Vec<TrackInfo>) {
        self.tracks = tracks;
        // Recompute cumulative Y offsets from track heights
        self.track_y_offsets.clear();
        let mut y = 0.0;
        for track in &self.tracks {
            self.track_y_offsets.push(y);
            y += track.height;
        }
        self.total_tracks_height = y;
    }

    /// Rebuild the CoordinateMapper's Y-layout from layer data.
    /// Call this from app.rs when layers change (before build).
    pub fn rebuild_mapper_layout(&mut self, layers: &[manifold_core::layer::Layer]) {
        self.mapper.rebuild_y_layout(layers);
    }

    /// Get a reference to the shared CoordinateMapper.
    /// Used by layer headers and other panels that need shared coordinate space.
    pub fn mapper(&self) -> &CoordinateMapper {
        &self.mapper
    }

    pub fn set_clips(&mut self, clips: Vec<ViewportClip>) {
        self.clips = clips;
    }

    pub fn set_zoom(&mut self, pixels_per_beat: f32) {
        self.mapper.set_zoom(pixels_per_beat);
    }

    pub fn set_zoom_index(&mut self, index: usize) {
        self.mapper.set_zoom_by_index(index);
    }

    /// Set scroll position (clamped). Returns true if the value actually changed.
    pub fn set_scroll(&mut self, scroll_x_beats: f32, scroll_y_px: f32) -> bool {
        let new_x = scroll_x_beats.max(0.0);
        // Clamp vertical scroll: never scroll past the last track
        let viewport_h = self.tracks_rect.height;
        let max_scroll_y = (self.total_tracks_height - viewport_h).max(0.0);
        let new_y = scroll_y_px.clamp(0.0, max_scroll_y);

        let changed = (new_x - self.scroll_x_beats).abs() > 0.001
            || (new_y - self.scroll_y_px).abs() > 0.001;
        self.scroll_x_beats = new_x;
        self.scroll_y_px = new_y;
        changed
    }

    pub fn set_beats_per_bar(&mut self, bpb: u32) {
        self.beats_per_bar = bpb.max(1);
    }

    pub fn set_playhead(&mut self, beat: f32) {
        self.playhead_beat = beat;
    }

    pub fn set_insert_cursor(&mut self, beat: f32) {
        self.insert_cursor_beat = beat;
    }

    pub fn set_export_range(&mut self, in_beat: f32, out_beat: f32) {
        self.export_in_beat = in_beat;
        self.export_out_beat = out_beat;
    }

    pub fn set_playing(&mut self, playing: bool) {
        self.is_playing = playing;
    }

    pub fn set_selection_region(&mut self, region: Option<SelectionRegion>) {
        self.selection_region = region;
    }

    pub fn set_selected_clip_ids(&mut self, ids: Vec<String>) {
        self.selected_clip_ids = ids;
    }

    pub fn set_hovered_clip_id(&mut self, id: Option<String>) {
        self.hovered_clip_id = id;
    }

    // ── Accessors ─────────────────────────────────────────────────

    pub fn pixels_per_beat(&self) -> f32 { self.mapper.pixels_per_beat() }
    pub fn scroll_x_beats(&self) -> f32 { self.scroll_x_beats }
    pub fn scroll_y_px(&self) -> f32 { self.scroll_y_px }
    pub fn viewport_rect(&self) -> Rect { self.viewport_rect }
    pub fn ruler_rect(&self) -> Rect { self.ruler_rect }
    pub fn tracks_rect(&self) -> Rect { self.tracks_rect }
    pub fn first_node(&self) -> usize { self.first_node }
    pub fn node_count(&self) -> usize { self.node_count }

    // ── Coordinate mapping ────────────────────────────────────────

    /// Convert beat position to pixel X in the tracks area (screen-space).
    pub fn beat_to_pixel(&self, beat: f32) -> f32 {
        (beat - self.scroll_x_beats) * self.mapper.pixels_per_beat() + self.tracks_rect.x
    }

    /// Convert pixel X in the tracks area to beat position.
    pub fn pixel_to_beat(&self, px: f32) -> f32 {
        (px - self.tracks_rect.x) / self.mapper.pixels_per_beat() + self.scroll_x_beats
    }

    /// Convert beat duration to pixel width.
    pub fn beat_duration_to_width(&self, beats: f32) -> f32 {
        self.mapper.beat_duration_to_width(beats)
    }

    /// Get Y position of a track (relative to tracks_rect top, before scroll).
    pub fn track_y(&self, layer_index: usize) -> f32 {
        self.track_y_offsets.get(layer_index).copied().unwrap_or(0.0)
            + self.tracks_rect.y - self.scroll_y_px
    }

    /// Get height of a track.
    pub fn track_height(&self, layer_index: usize) -> f32 {
        self.tracks.get(layer_index)
            .map(|t| t.height)
            .unwrap_or(color::TRACK_HEIGHT)
    }

    /// Visible beat range (with buffer).
    fn visible_beat_range(&self) -> (f32, f32) {
        let min_beat = self.scroll_x_beats;
        let max_beat = min_beat + self.tracks_rect.width / self.mapper.pixels_per_beat();
        (min_beat, max_beat)
    }

    // ── Hit-testing ───────────────────────────────────────────────

    /// Hit-test a screen position against all clips.
    /// Returns the topmost clip hit and which region was hit (body, trim left, trim right).
    pub fn hit_test_clip(&self, pos: Vec2) -> Option<ClipHitResult> {
        if !self.tracks_rect.contains(pos) {
            return None;
        }

        let layer_index = self.layer_at_y(pos.y)?;
        let beat = self.pixel_to_beat(pos.x);

        // Reject clicks in vertical padding — only the padded clip rect is interactive.
        let track_y = self.track_y(layer_index);
        let track_h = self.track_height(layer_index);
        let clip_top = track_y + CLIP_VERTICAL_PAD;
        let clip_bottom = track_y + track_h - CLIP_VERTICAL_PAD;
        if pos.y < clip_top || pos.y > clip_bottom {
            return None;
        }

        // Iterate clips on this layer in reverse order (topmost/last wins)
        for clip in self.clips.iter().rev() {
            if clip.layer_index != layer_index {
                continue;
            }

            let clip_end = clip.start_beat + clip.duration_beats;
            if beat < clip.start_beat || beat >= clip_end {
                continue;
            }

            let clip_width_px = clip.duration_beats * self.mapper.pixels_per_beat();
            let local_px = (beat - clip.start_beat) * self.mapper.pixels_per_beat();

            let region = if clip_width_px > color::TRIM_HANDLE_MIN_CLIP_WIDTH_PX && local_px < color::TRIM_HANDLE_THRESHOLD_PX {
                HitRegion::TrimLeft
            } else if clip_width_px > color::TRIM_HANDLE_MIN_CLIP_WIDTH_PX && local_px > clip_width_px - color::TRIM_HANDLE_THRESHOLD_PX {
                HitRegion::TrimRight
            } else {
                HitRegion::Body
            };

            return Some(ClipHitResult {
                clip_id: clip.clip_id.clone(),
                layer_index,
                region,
            });
        }

        None
    }

    /// Called every frame (or on CursorMoved) with the current cursor position
    /// to update clip hover state. Matches Unity's OnPointerMove continuous hit-testing.
    pub fn update_hover_at(&mut self, pos: Vec2) -> Vec<PanelAction> {
        if !self.tracks_rect.contains(pos) {
            if self.hovered_clip_id.is_some() {
                self.hovered_clip_id = None;
                return vec![PanelAction::ViewportHoverChanged(None)];
            }
            return Vec::new();
        }

        let new_hover = self.hit_test_clip(pos).map(|h| h.clip_id);
        if new_hover != self.hovered_clip_id {
            self.hovered_clip_id = new_hover.clone();
            return vec![PanelAction::ViewportHoverChanged(new_hover)];
        }
        Vec::new()
    }

    /// Determine which layer a Y coordinate falls in.
    pub fn layer_at_y(&self, y: f32) -> Option<usize> {
        if y < self.tracks_rect.y || y > self.tracks_rect.y_max() {
            return None;
        }

        for (i, &offset) in self.track_y_offsets.iter().enumerate() {
            let track_y = offset + self.tracks_rect.y - self.scroll_y_px;
            let track_h = self.tracks.get(i).map(|t| t.height).unwrap_or(0.0);
            if y >= track_y && y < track_y + track_h {
                return Some(i);
            }
        }

        None
    }

    /// Snap a beat position to the current grid subdivision.
    pub fn snap_to_grid(&self, beat: f32) -> f32 {
        let step = match self.grid_subdivision() {
            GridSubdivision::Bar => self.beats_per_bar as f32,
            GridSubdivision::Beat => 1.0,
            GridSubdivision::Eighth => 0.5,
            GridSubdivision::Sixteenth => 0.25,
        };
        (beat / step).round() * step
    }

    /// Magnetic snap: snap to grid lines AND neighboring clip edges within threshold.
    /// Returns the nearest snap point within `SNAP_THRESHOLD_PX` pixels (12px),
    /// or `beat` unchanged if nothing is within threshold.
    /// `ignore_ids` are clip IDs being dragged (don't snap to self).
    pub fn magnetic_snap(&self, beat: f32, layer_index: usize, ignore_ids: &[String]) -> f32 {
        use crate::snap::SNAP_THRESHOLD_PX;

        // Clamp threshold to avoid snapping across bars at low zoom
        let max_snap_beats = 0.5_f32;
        let threshold_beats = (SNAP_THRESHOLD_PX / self.mapper.pixels_per_beat()).min(max_snap_beats);

        // Start with raw beat — only snap if a candidate is within threshold.
        let mut best_beat = beat;
        let mut best_dist = f32::MAX;

        // Grid candidate
        let grid_snapped = self.snap_to_grid(beat);
        let grid_dist = (grid_snapped - beat).abs();
        if grid_dist <= threshold_beats && grid_dist < best_dist {
            best_dist = grid_dist;
            best_beat = grid_snapped;
        }

        // Check neighboring clip edges on the same layer
        for clip in &self.clips {
            if clip.layer_index != layer_index { continue; }
            if ignore_ids.contains(&clip.clip_id) { continue; }

            // Check start edge
            let dist_start = (clip.start_beat - beat).abs();
            if dist_start < threshold_beats && dist_start < best_dist {
                best_dist = dist_start;
                best_beat = clip.start_beat;
            }

            // Check end edge
            let end_beat = clip.start_beat + clip.duration_beats;
            let dist_end = (end_beat - beat).abs();
            if dist_end < threshold_beats && dist_end < best_dist {
                best_dist = dist_end;
                best_beat = end_beat;
            }
        }

        best_beat
    }

    /// Check if a clip is locked by clip_id.
    fn clip_is_locked(&self, clip_id: &str) -> bool {
        for clip in &self.clips {
            if clip.clip_id == clip_id {
                return clip.is_locked;
            }
        }
        false
    }

    /// Current grid step size in beats.
    pub fn grid_step(&self) -> f32 {
        match self.grid_subdivision() {
            GridSubdivision::Bar => self.beats_per_bar as f32,
            GridSubdivision::Beat => 1.0,
            GridSubdivision::Eighth => 0.5,
            GridSubdivision::Sixteenth => 0.25,
        }
    }

    // ── Grid subdivision ──────────────────────────────────────────

    /// Determine grid subdivision level based on zoom.
    /// Uses per-note pixel widths (matching Unity's GridOverlay thresholds):
    ///   - Show 16ths when a 16th-note ≥ 4px wide
    ///   - Show 8ths  when an 8th-note ≥ 6px wide
    ///   - Show beats when a beat ≥ 6px wide
    fn grid_subdivision(&self) -> GridSubdivision {
        let sixteenth_px = self.mapper.pixels_per_beat() * 0.25;
        let eighth_px = self.mapper.pixels_per_beat() * 0.5;
        if sixteenth_px >= 4.0 {
            GridSubdivision::Sixteenth
        } else if eighth_px >= 6.0 {
            GridSubdivision::Eighth
        } else if self.mapper.pixels_per_beat() >= 6.0 {
            GridSubdivision::Beat
        } else {
            GridSubdivision::Bar
        }
    }

    // ── Sync methods ──────────────────────────────────────────────

    /// Update playhead position in the tree without rebuilding.
    pub fn sync_playhead(&self, tree: &mut UITree) {
        let px = self.beat_to_pixel(self.playhead_beat);
        let in_view = px >= self.tracks_rect.x && px <= self.tracks_rect.x_max();

        if self.playhead_track_id >= 0 {
            tree.set_visible(self.playhead_track_id as u32, in_view);
            if in_view {
                tree.set_bounds(
                    self.playhead_track_id as u32,
                    Rect::new(centered_line_x(px, PLAYHEAD_WIDTH), self.tracks_rect.y,
                              PLAYHEAD_WIDTH, self.tracks_rect.height),
                );
            }
        }

        if self.playhead_ruler_id >= 0 {
            tree.set_visible(self.playhead_ruler_id as u32, in_view);
            if in_view {
                tree.set_bounds(
                    self.playhead_ruler_id as u32,
                    Rect::new(centered_line_x(px, PLAYHEAD_WIDTH), self.ruler_rect.y,
                              PLAYHEAD_WIDTH, self.ruler_rect.height),
                );
            }
        }
    }

    /// Update insert cursor position without rebuilding.
    pub fn sync_insert_cursor(&self, tree: &mut UITree) {
        let px = self.beat_to_pixel(self.insert_cursor_beat);
        let in_view = px >= self.tracks_rect.x && px <= self.tracks_rect.x_max();

        if self.insert_cursor_track_id >= 0 {
            tree.set_visible(self.insert_cursor_track_id as u32, in_view);
            if in_view {
                tree.set_bounds(
                    self.insert_cursor_track_id as u32,
                    Rect::new(centered_line_x(px, INSERT_CURSOR_WIDTH), self.tracks_rect.y,
                              INSERT_CURSOR_WIDTH, self.tracks_rect.height),
                );
            }
        }

        if self.insert_cursor_ruler_id >= 0 {
            tree.set_visible(self.insert_cursor_ruler_id as u32, in_view);
            if in_view {
                let marker_s = color::INSERT_CURSOR_RULER_MARKER_SIZE;
                tree.set_bounds(
                    self.insert_cursor_ruler_id as u32,
                    Rect::new(px - marker_s * 0.5, self.ruler_rect.y + self.ruler_rect.height - marker_s,
                              marker_s, marker_s),
                );
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GridSubdivision {
    Bar,
    Beat,
    Eighth,
    Sixteenth,
}

impl Panel for TimelineViewportPanel {
    fn build(&mut self, tree: &mut UITree, layout: &ScreenLayout) {
        self.first_node = tree.count();

        let body = layout.timeline_body();
        if body.width <= 0.0 || body.height <= 0.0 {
            self.node_count = 0;
            return;
        }

        // Viewport areas
        let layer_ctrl_w = color::LAYER_CONTROLS_WIDTH;
        let tracks_w = body.width - layer_ctrl_w;
        if tracks_w <= 0.0 {
            self.node_count = 0;
            return;
        }

        // Header stack: overview strip (16) + ruler (40) = 56px
        let header_h = color::OVERVIEW_STRIP_HEIGHT + RULER_HEIGHT;
        self.viewport_rect = Rect::new(body.x, body.y, tracks_w, body.height);
        self.ruler_rect = Rect::new(body.x, body.y + color::OVERVIEW_STRIP_HEIGHT, tracks_w, RULER_HEIGHT);
        self.tracks_rect = Rect::new(
            body.x,
            body.y + header_h,
            tracks_w,
            (body.height - header_h).max(0.0),
        );

        // Background
        self.bg_panel_id = tree.add_panel(
            -1, self.viewport_rect.x, self.viewport_rect.y,
            self.viewport_rect.width, self.viewport_rect.height,
            UIStyle { bg_color: color::DARK_BG, ..UIStyle::default() },
        ) as i32;

        // Overview strip at top of viewport.
        // From Unity OverviewStripPanel.cs — mini-timeline with clip miniatures,
        // viewport indicator, playhead, and export range markers.
        let overview_rect = Rect::new(body.x, body.y, tracks_w, color::OVERVIEW_STRIP_HEIGHT);
        tree.add_panel(
            -1, overview_rect.x, overview_rect.y, overview_rect.width, overview_rect.height,
            UIStyle { bg_color: color::OVERVIEW_BG, ..UIStyle::default() },
        );

        // Overview clip miniatures (from Unity OverviewStripPanel.BuildPanel lines 218-238)
        self.build_overview_clips(tree, overview_rect);

        // Ruler background — INTERACTIVE so clicks register for playhead scrubbing
        self.ruler_bg_id = tree.add_button(
            -1, self.ruler_rect.x, self.ruler_rect.y,
            self.ruler_rect.width, self.ruler_rect.height,
            UIStyle { bg_color: color::HEADER_BG, ..UIStyle::default() },
            "",
        ) as i32;

        // Interactive overlay covering entire tracks area — catches all clicks/drags
        // (matches Unity's InteractionOverlay which is a transparent MonoBehaviour
        // covering the tracks viewport). Without this, clicks on non-interactive
        // panel nodes (track backgrounds, grid lines) won't generate events.
        tree.add_button(
            -1, self.tracks_rect.x, self.tracks_rect.y,
            self.tracks_rect.width, self.tracks_rect.height,
            UIStyle { bg_color: Color32::TRANSPARENT, ..UIStyle::default() },
            "",
        );

        // Build track backgrounds
        self.build_track_backgrounds(tree);

        // Build grid lines
        self.build_grid_lines(tree);

        // Build ruler ticks and labels
        self.build_ruler(tree);

        // Build clips
        self.build_clips(tree);

        // Selection region overlay
        self.build_selection_region(tree);

        // Export range markers
        self.build_export_markers(tree);

        // Insert cursor (below playhead in draw order)
        self.build_insert_cursor(tree);

        // Playhead (on top of everything)
        self.build_playhead(tree);

        self.node_count = tree.count() - self.first_node;
    }

    fn update(&mut self, tree: &mut UITree) {
        self.sync_playhead(tree);
        self.sync_insert_cursor(tree);
    }

    fn handle_event(&mut self, event: &UIEvent, _tree: &UITree) -> Vec<PanelAction> {
        match event {
            // ── Click ────────────────────────────────────────────
            // Mirrors Unity InteractionOverlay.OnPointerClick exactly.
            // UIInputSystem never emits Click after DragEnd, but guard
            // against edge cases by checking drag_mode.
            UIEvent::Click { pos, modifiers, .. } => {
                if self.drag_mode != ViewportDragMode::None {
                    return Vec::new();
                }

                // Ruler click → seek
                if self.ruler_rect.contains(*pos) {
                    let beat = self.pixel_to_beat(pos.x).max(0.0);
                    return vec![PanelAction::Seek(beat)];
                }

                if !self.tracks_rect.contains(*pos) {
                    return Vec::new();
                }

                let hit = self.hit_test_clip(*pos);

                if let Some(ref hit) = hit {
                    // ── HIT: clip was clicked ──
                    // Locked clips: ignore
                    if self.clip_is_locked(&hit.clip_id) {
                        return Vec::new();
                    }

                    // Right-click handled via RightClick event, not here.

                    let mut actions = Vec::new();

                    if modifiers.shift {
                        // Shift+click: extend region to clip
                        actions.push(PanelAction::ClipClicked(
                            hit.clip_id.clone(),
                            Modifiers { shift: true, ..*modifiers },
                        ));
                    } else if modifiers.ctrl || modifiers.command {
                        // Cmd/Ctrl+click: toggle multi-select
                        actions.push(PanelAction::ClipClicked(
                            hit.clip_id.clone(),
                            Modifiers { ctrl: true, command: true, ..*modifiers },
                        ));
                    } else {
                        // Plain click: select single
                        actions.push(PanelAction::ClipClicked(
                            hit.clip_id.clone(),
                            Modifiers::default(),
                        ));
                    }

                    actions
                } else {
                    // ── NO HIT: empty area clicked ──
                    let beat = self.pixel_to_beat(pos.x);

                    if let Some(layer) = self.layer_at_y(pos.y) {
                        let snapped = self.snap_to_grid(beat);

                        if modifiers.shift {
                            // Shift+click on empty area: extend region
                            vec![PanelAction::TrackClicked(snapped, layer, *modifiers)]
                        } else {
                            // Plain click: set insert cursor + inspect layer
                            vec![PanelAction::SetInsertCursor(snapped)]
                        }
                    } else {
                        Vec::new()
                    }
                }
            }

            // ── DoubleClick ──────────────────────────────────────
            // Mirrors Unity InteractionOverlay: double-click on clip
            // or double-click on empty area to create clip.
            UIEvent::DoubleClick { pos, .. } => {
                if !self.tracks_rect.contains(*pos) {
                    return Vec::new();
                }

                if let Some(hit) = self.hit_test_clip(*pos) {
                    vec![PanelAction::ClipDoubleClicked(hit.clip_id)]
                } else {
                    let beat = self.pixel_to_beat(pos.x);
                    if let Some(layer) = self.layer_at_y(pos.y) {
                        vec![PanelAction::TrackDoubleClicked(beat, layer)]
                    } else {
                        Vec::new()
                    }
                }
            }

            // ── RightClick ───────────────────────────────────────
            // Mirrors Unity InteractionOverlay.OnPointerClick right-click path:
            // if clip hit and not selected, select it first (app layer handles
            // that via ClipClicked before showing menu).
            UIEvent::RightClick { pos, .. } => {
                if !self.tracks_rect.contains(*pos) {
                    return Vec::new();
                }

                let beat = self.pixel_to_beat(pos.x);
                if let Some(hit) = self.hit_test_clip(*pos) {
                    if self.clip_is_locked(&hit.clip_id) {
                        return Vec::new();
                    }

                    let mut actions = Vec::new();
                    // If not already selected, select first (app layer checks)
                    if !self.selected_clip_ids.contains(&hit.clip_id) {
                        actions.push(PanelAction::ClipClicked(
                            hit.clip_id.clone(),
                            Modifiers::default(),
                        ));
                    }
                    actions.push(PanelAction::ClipRightClicked(hit.clip_id));
                    actions
                } else if let Some(layer) = self.layer_at_y(pos.y) {
                    vec![PanelAction::TrackRightClicked(beat, layer)]
                } else {
                    Vec::new()
                }
            }

            // ── DragBegin ────────────────────────────────────────
            // Mirrors Unity InteractionOverlay.OnBeginDrag:
            // MUST use origin (press position) for hit testing, not pos.
            UIEvent::DragBegin { origin, .. } => {
                // Ruler drag → scrub
                if self.ruler_rect.contains(*origin) {
                    self.drag_mode = ViewportDragMode::RulerScrub;
                    let beat = self.pixel_to_beat(origin.x).max(0.0);
                    return vec![PanelAction::Seek(beat)];
                }

                if !self.tracks_rect.contains(*origin) {
                    return Vec::new();
                }

                let hit = self.hit_test_clip(*origin);

                if let Some(hit) = hit {
                    // Locked clips: ignore drag
                    if self.clip_is_locked(&hit.clip_id) {
                        return Vec::new();
                    }

                    let beat = self.pixel_to_beat(origin.x);
                    let mut actions = Vec::new();

                    match hit.region {
                        HitRegion::TrimLeft => {
                            // Select if not selected
                            if !self.selected_clip_ids.contains(&hit.clip_id) {
                                actions.push(PanelAction::ClipClicked(
                                    hit.clip_id.clone(),
                                    Modifiers::default(),
                                ));
                            }
                            self.drag_mode = ViewportDragMode::TrimLeft;
                            actions.push(PanelAction::ClipDragStarted(
                                hit.clip_id, HitRegion::TrimLeft, beat,
                            ));
                        }
                        HitRegion::TrimRight => {
                            if !self.selected_clip_ids.contains(&hit.clip_id) {
                                actions.push(PanelAction::ClipClicked(
                                    hit.clip_id.clone(),
                                    Modifiers::default(),
                                ));
                            }
                            self.drag_mode = ViewportDragMode::TrimRight;
                            actions.push(PanelAction::ClipDragStarted(
                                hit.clip_id, HitRegion::TrimRight, beat,
                            ));
                        }
                        HitRegion::Body => {
                            // Select if not already selected (unless multi-selected)
                            if !self.selected_clip_ids.contains(&hit.clip_id) {
                                actions.push(PanelAction::ClipClicked(
                                    hit.clip_id.clone(),
                                    Modifiers::default(),
                                ));
                            }
                            self.drag_mode = ViewportDragMode::Move;
                            actions.push(PanelAction::ClipDragStarted(
                                hit.clip_id, HitRegion::Body, beat,
                            ));
                        }
                    }

                    actions
                } else {
                    // Empty area drag → region select
                    let beat = self.pixel_to_beat(origin.x);
                    if let Some(layer) = self.layer_at_y(origin.y) {
                        self.drag_mode = ViewportDragMode::RegionSelect;
                        vec![PanelAction::RegionDragStarted(beat, layer)]
                    } else {
                        Vec::new()
                    }
                }
            }

            // ── Drag ─────────────────────────────────────────────
            // Mirrors Unity InteractionOverlay.OnDrag switch on dragMode.
            UIEvent::Drag { pos, .. } => {
                let beat = self.pixel_to_beat(pos.x);
                let layer = self.layer_at_y(pos.y);

                match self.drag_mode {
                    ViewportDragMode::Move => {
                        vec![PanelAction::ClipDragMoved(beat, layer)]
                    }
                    ViewportDragMode::TrimLeft | ViewportDragMode::TrimRight => {
                        // Trim drags pass beat only, no layer target
                        vec![PanelAction::ClipDragMoved(beat, None)]
                    }
                    ViewportDragMode::RegionSelect => {
                        if let Some(layer) = layer {
                            vec![PanelAction::RegionDragMoved(beat, layer)]
                        } else {
                            Vec::new()
                        }
                    }
                    ViewportDragMode::RulerScrub => {
                        vec![PanelAction::Seek(beat.max(0.0))]
                    }
                    ViewportDragMode::None => Vec::new(),
                }
            }

            // ── DragEnd ──────────────────────────────────────────
            // Mirrors Unity InteractionOverlay.OnEndDrag:
            // emit appropriate end action, reset drag mode.
            UIEvent::DragEnd { .. } => {
                let was = self.drag_mode;
                self.drag_mode = ViewportDragMode::None;

                match was {
                    ViewportDragMode::Move
                    | ViewportDragMode::TrimLeft
                    | ViewportDragMode::TrimRight => {
                        vec![PanelAction::ClipDragEnded]
                    }
                    ViewportDragMode::RegionSelect => {
                        vec![PanelAction::RegionDragEnded]
                    }
                    ViewportDragMode::RulerScrub | ViewportDragMode::None => Vec::new(),
                }
            }

            // ── Hover ────────────────────────────────────────────
            UIEvent::HoverEnter { pos, .. } | UIEvent::PointerDown { pos, .. } => {
                if self.tracks_rect.contains(*pos) {
                    let hit = self.hit_test_clip(*pos);
                    let new_id = hit.map(|h| h.clip_id);
                    if new_id != self.hovered_clip_id {
                        self.hovered_clip_id = new_id.clone();
                        return vec![PanelAction::ViewportHoverChanged(new_id)];
                    }
                }
                Vec::new()
            }
            UIEvent::HoverExit { .. } => {
                if self.hovered_clip_id.is_some() {
                    self.hovered_clip_id = None;
                    return vec![PanelAction::ViewportHoverChanged(None)];
                }
                Vec::new()
            }

            _ => Vec::new(),
        }
    }
}

// ── Build helpers (private) ──────────────────────────────────────

impl TimelineViewportPanel {
    /// Build clip miniatures in the overview strip.
    /// From Unity OverviewStripPanel.BuildPanel (lines 218-270).
    /// Renders small colored rects for each clip, a viewport indicator,
    /// and the playhead position.
    fn build_overview_clips(&self, tree: &mut UITree, overview_rect: Rect) {
        if self.clips.is_empty() || self.tracks.is_empty() {
            return;
        }

        // Compute total content duration for normalization
        let mut max_beat = 0.0f32;
        for clip in &self.clips {
            let end = clip.start_beat + clip.duration_beats;
            if end > max_beat { max_beat = end; }
        }
        if max_beat <= 0.0 { return; }

        let layer_count = self.tracks.len();
        let row_h = overview_rect.height / layer_count as f32;
        let palette = &color::LAYER_PALETTE;

        // Clip miniatures
        for clip in &self.clips {
            let start_norm = clip.start_beat / max_beat;
            let end_norm = (clip.start_beat + clip.duration_beats) / max_beat;
            let x = overview_rect.x + start_norm * overview_rect.width;
            let w = ((end_norm - start_norm) * overview_rect.width).max(1.0);
            // Layer 0 at bottom, layer N-1 at top (matching Unity line 230)
            let y = overview_rect.y + (layer_count.saturating_sub(1).saturating_sub(clip.layer_index)) as f32 * row_h;

            let clip_color = palette[clip.layer_index % palette.len()];
            tree.add_panel(-1, x, y, w, row_h,
                UIStyle { bg_color: clip_color, ..UIStyle::default() },
            );
        }

        // Viewport indicator (semi-transparent blue showing visible portion)
        let ppb = self.mapper.pixels_per_beat();
        if ppb > 0.0 {
            let viewport_width_beats = self.tracks_rect.width / ppb;
            let vp_start_norm = self.scroll_x_beats / max_beat;
            let vp_width_norm = viewport_width_beats / max_beat;
            let vp_x = overview_rect.x + vp_start_norm * overview_rect.width;
            let vp_w = (vp_width_norm * overview_rect.width).min(overview_rect.width);
            tree.add_panel(-1, vp_x, overview_rect.y, vp_w, overview_rect.height,
                UIStyle { bg_color: color::OVERVIEW_VIEWPORT, ..UIStyle::default() },
            );
        }

        // Playhead in overview
        let ph_norm = self.playhead_beat / max_beat;
        let ph_x = overview_rect.x + (ph_norm * overview_rect.width).clamp(0.0, overview_rect.width);
        tree.add_panel(-1, ph_x, overview_rect.y, 1.0, overview_rect.height,
            UIStyle { bg_color: color::OVERVIEW_PLAYHEAD, ..UIStyle::default() },
        );
    }

    fn build_track_backgrounds(&mut self, tree: &mut UITree) {
        self.track_bg_ids.clear();

        let tr = &self.tracks_rect;
        let tr_top = tr.y;
        let tr_bottom = tr.y + tr.height;

        for (i, track) in self.tracks.iter().enumerate() {
            let y = self.track_y(i);
            let h = track.height;

            // Skip if completely outside viewport
            if y + h < tr_top || y > tr_bottom {
                continue;
            }

            // Clamp to tracks_rect bounds (prevents bleeding into video area)
            let clamped_y = y.max(tr_top);
            let clamped_h = (y + h).min(tr_bottom) - clamped_y;
            if clamped_h <= 0.0 {
                continue;
            }

            let bg_color = if i % 2 == 0 { color::TRACK_BG } else { color::TRACK_BG_ALT };
            let mut style = UIStyle { bg_color, ..UIStyle::default() };

            // Dim muted tracks
            if track.is_muted {
                style.bg_color = Color32::new(
                    bg_color.r / 2,
                    bg_color.g / 2,
                    bg_color.b / 2,
                    bg_color.a,
                );
            }

            let id = tree.add_panel(
                -1, tr.x, clamped_y,
                tr.width, clamped_h,
                style,
            ) as i32;
            self.track_bg_ids.push(id);

            // Group accent bar (only if top is visible)
            if track.is_group && y >= tr_top {
                if let Some(accent) = track.accent_color {
                    tree.add_panel(
                        -1, tr.x, clamped_y,
                        color::GROUP_ACCENT_BAR_WIDTH, clamped_h,
                        UIStyle { bg_color: accent, ..UIStyle::default() },
                    );
                }
            }

            // Collapsed group preview: miniature clip rects of child layers.
            // From Unity ViewportManager.GenerateCollapsedGroupTexture (lines 700-770).
            if track.is_group && track.is_collapsed && !track.child_layer_indices.is_empty() {
                let child_count = track.child_layer_indices.len();
                let rows_per_child = 2.0_f32.min(clamped_h / child_count.max(1) as f32);
                let palette = &color::LAYER_PALETTE;
                let (min_beat, max_beat) = self.visible_beat_range();
                let ppb = self.mapper.pixels_per_beat();

                for (ci, &child_idx) in track.child_layer_indices.iter().enumerate() {
                    let child_y = clamped_y + ci as f32 * rows_per_child;
                    let child_color = palette[ci % palette.len()];

                    // Render clips of this child layer as tiny rects
                    for clip in &self.clips {
                        if clip.layer_index != child_idx { continue; }
                        let clip_end = clip.start_beat + clip.duration_beats;
                        if clip_end < min_beat || clip.start_beat > max_beat { continue; }

                        let cx = self.beat_to_pixel(clip.start_beat).max(tr.x);
                        let cx2 = self.beat_to_pixel(clip_end).min(tr.x + tr.width);
                        let cw = (cx2 - cx).max(1.0);

                        tree.add_panel(-1, cx, child_y, cw, rows_per_child,
                            UIStyle { bg_color: child_color, ..UIStyle::default() },
                        );
                    }
                }
            }

            // Bottom separator (only if bottom edge is visible)
            let sep_y = y + h - 1.0;
            if sep_y >= tr_top && sep_y < tr_bottom {
                tree.add_panel(
                    -1, tr.x, sep_y,
                    tr.width, 1.0,
                    UIStyle { bg_color: color::SEPARATOR_COLOR, ..UIStyle::default() },
                );
            }
        }
    }

    fn build_grid_lines(&mut self, tree: &mut UITree) {
        self.grid_line_ids.clear();
        let (min_beat, max_beat) = self.visible_beat_range();
        let subdiv = self.grid_subdivision();
        let bpb = self.beats_per_bar as f32;

        // Determine step size
        let step = match subdiv {
            GridSubdivision::Bar => bpb,
            GridSubdivision::Beat => 1.0,
            GridSubdivision::Eighth => 0.5,
            GridSubdivision::Sixteenth => 0.25,
        };

        let start = (min_beat / step).floor() * step;
        let mut beat = start;
        let mut count = 0;

        while beat <= max_beat && count < MAX_GRID_LINES {
            let px = self.beat_to_pixel(beat);
            if px >= self.tracks_rect.x && px <= self.tracks_rect.x_max() {
                let is_bar = (beat % bpb).abs() < 0.001;
                let is_beat = (beat % 1.0).abs() < 0.001;

                let line_color = if is_bar {
                    color::GRID_BAR_LINE
                } else if is_beat {
                    color::GRID_BEAT_LINE
                } else if (beat * 2.0).fract().abs() < 0.01 {
                    color::GRID_SUBDIVISION_LINE
                } else {
                    color::GRID_SIXTEENTH_LINE
                };

                // Bar lines are 2px wide, all others 1px.
                // From Unity LayerBitmapRenderer.PaintGridLines.
                let line_w = if is_bar { 2.0 } else { GRID_LINE_W };
                let id = tree.add_panel(
                    -1, px, self.tracks_rect.y,
                    line_w, self.tracks_rect.height,
                    UIStyle { bg_color: line_color, ..UIStyle::default() },
                ) as i32;
                self.grid_line_ids.push(id);
                count += 1;
            }
            beat += step;
        }
    }

    fn build_ruler(&mut self, tree: &mut UITree) {
        self.ruler_tick_ids.clear();
        self.ruler_label_ids.clear();

        let (min_beat, max_beat) = self.visible_beat_range();
        let bpb = self.beats_per_bar as f32;
        let subdiv = self.grid_subdivision();

        // Tick step
        let tick_step = match subdiv {
            GridSubdivision::Bar => bpb,
            GridSubdivision::Beat => 1.0,
            GridSubdivision::Eighth => 0.5,
            GridSubdivision::Sixteenth => 0.25,
        };

        let start = (min_beat / tick_step).floor() * tick_step;
        let mut beat = start;
        let mut count = 0;
        let ruler_bottom = self.ruler_rect.y + self.ruler_rect.height;

        while beat <= max_beat && count < MAX_RULER_TICKS {
            let px = self.beat_to_pixel(beat);
            if px >= self.ruler_rect.x && px <= self.ruler_rect.x_max() {
                let is_bar = (beat % bpb).abs() < 0.001;
                let is_beat = (beat % 1.0).abs() < 0.001;

                let tick_h = if is_bar {
                    RULER_BAR_TICK_H
                } else if is_beat {
                    RULER_BEAT_TICK_H
                } else {
                    4.0
                };

                let tick_color = if is_bar {
                    color::TEXT_NORMAL
                } else {
                    color::TEXT_SUBTLE
                };

                // Tick mark (bottom-aligned)
                let id = tree.add_panel(
                    -1, px, ruler_bottom - tick_h,
                    RULER_TICK_W, tick_h,
                    UIStyle { bg_color: tick_color, ..UIStyle::default() },
                ) as i32;
                self.ruler_tick_ids.push(id);

                // Label (for bars and beats at higher zoom)
                if is_bar || (is_beat && subdiv != GridSubdivision::Bar) {
                    let bar_num = (beat / bpb).floor() as i32 + 1;
                    let beat_in_bar = ((beat % bpb) + 0.001).floor() as i32 + 1;
                    let label = if is_bar {
                        format!("{}", bar_num)
                    } else {
                        format!("{}.{}", bar_num, beat_in_bar)
                    };

                    let label_y = self.ruler_rect.y + 2.0;
                    let id = tree.add_label(
                        -1, px + 2.0, label_y, RULER_LABEL_W, RULER_LABEL_H,
                        &label,
                        UIStyle {
                            text_color: if is_bar { color::TEXT_NORMAL } else { color::TEXT_DIMMED },
                            font_size: RULER_FONT_SIZE,
                            text_align: TextAlign::Left,
                            ..UIStyle::default()
                        },
                    ) as i32;
                    self.ruler_label_ids.push(id);
                }

                count += 1;
            }
            beat += tick_step;
        }
    }

    fn build_clips(&mut self, tree: &mut UITree) {
        self.clip_bg_ids.clear();
        self.clip_label_ids.clear();
        self.clip_border_ids.clear();
        self.clip_trim_handle_ids.clear();

        let (min_beat, max_beat) = self.visible_beat_range();
        let mut count = 0;

        for clip in &self.clips {
            if count >= MAX_VISIBLE_CLIPS { break; }

            let clip_end = clip.start_beat + clip.duration_beats;

            // Skip clips outside visible range
            if clip_end < min_beat || clip.start_beat > max_beat {
                continue;
            }

            // Skip clips on non-existent layers
            if clip.layer_index >= self.tracks.len() {
                continue;
            }

            let track_y = self.track_y(clip.layer_index);
            let track_h = self.track_height(clip.layer_index);

            // Skip if track is off-screen vertically
            if track_y + track_h < self.tracks_rect.y || track_y > self.tracks_rect.y_max() {
                continue;
            }

            let x1 = self.beat_to_pixel(clip.start_beat).max(self.tracks_rect.x);
            let x2 = self.beat_to_pixel(clip_end).min(self.tracks_rect.x_max());
            let clip_w = x2 - x1;

            if clip_w < 1.0 { continue; }

            let raw_clip_y = track_y + CLIP_VERTICAL_PAD;
            let raw_clip_h = track_h - CLIP_VERTICAL_PAD * 2.0;
            // Clamp clip rect to tracks_rect bounds (prevent bleeding into video area)
            let clip_y = raw_clip_y.max(self.tracks_rect.y);
            let clip_bottom = (raw_clip_y + raw_clip_h).min(self.tracks_rect.y + self.tracks_rect.height);
            let clip_h = clip_bottom - clip_y;
            if clip_h <= 0.0 { continue; }

            // Determine clip color
            let is_selected = self.selected_clip_ids.contains(&clip.clip_id);
            let is_hovered = self.hovered_clip_id.as_ref() == Some(&clip.clip_id);
            let clip_color = get_clip_color(clip, is_selected, is_hovered);

            // ── Clip rendering (matches Unity LayerBitmapRenderer.DrawClip) ──

            // Determine border width: 2px when selected, 1px otherwise.
            // From Unity DrawClip lines 72-77.
            let border_w = if is_selected { 2.0 } else { 1.0 };

            // Clip background with top/bottom borders always drawn.
            // Left/right borders only when clip_w >= 12px (prevents "caterpillar" at low zoom).
            let border_color = if is_selected { color::ACCENT_BLUE } else {
                Color32::new(
                    clip_color.r.saturating_sub(30),
                    clip_color.g.saturating_sub(30),
                    clip_color.b.saturating_sub(30),
                    clip_color.a,
                )
            };
            let bg_id = tree.add_button(
                -1, x1, clip_y, clip_w, clip_h,
                UIStyle {
                    bg_color: clip_color,
                    hover_bg_color: if is_selected { clip_color } else {
                        Color32::new(
                            clip_color.r.saturating_add(10),
                            clip_color.g.saturating_add(10),
                            clip_color.b.saturating_add(10),
                            clip_color.a,
                        )
                    },
                    pressed_bg_color: Color32::new(
                        clip_color.r.saturating_sub(10),
                        clip_color.g.saturating_sub(10),
                        clip_color.b.saturating_sub(10),
                        clip_color.a,
                    ),
                    corner_radius: CLIP_CORNER_RADIUS,
                    border_color,
                    border_width: border_w,
                    ..UIStyle::default()
                },
                "",
            ) as i32;
            self.clip_bg_ids.push(bg_id);

            // 1px dark separator at clip left edge (only when clip_w >= 4px).
            // From Unity DrawClip line 67-68.
            if clip_w >= 4.0 {
                tree.add_panel(
                    -1, x1, clip_y, 1.0, clip_h,
                    UIStyle {
                        bg_color: color::CLIP_SEPARATOR,
                        ..UIStyle::default()
                    },
                );
            }

            // Clip name label (if wide enough)
            if clip_w > CLIP_MIN_WIDTH_PX + CLIP_LABEL_PAD * 2.0 {
                let label_x = x1 + CLIP_LABEL_PAD + 1.0; // past 1px separator
                let label_w = clip_w - CLIP_LABEL_PAD * 2.0 - 1.0;
                let text_color = if clip.is_generator {
                    color::CLIP_LABEL_BG
                } else {
                    color::CLIP_LABEL_BG_HOVER
                };

                let label_id = tree.add_label(
                    -1, label_x, clip_y + 2.0, label_w.max(1.0), clip_h - 4.0,
                    &clip.name,
                    UIStyle {
                        text_color,
                        font_size: FONT_SIZE,
                        text_align: TextAlign::Left,
                        ..UIStyle::default()
                    },
                ) as i32;
                self.clip_label_ids.push(label_id);
            }

            // Trim hint indicators: 6px inset at each end on hover OR selected.
            // Color: ACCENT_BLUE_DIM. From Unity DrawClip lines 82-91.
            if (is_selected || is_hovered) && clip_w > 12.0 {
                let hint_w = 6.0_f32.min(clip_w * 0.25);
                let trim_style = UIStyle {
                    bg_color: color::ACCENT_BLUE_DIM,
                    ..UIStyle::default()
                };

                // Left trim hint
                let left_id = tree.add_panel(
                    -1, x1, clip_y, hint_w, clip_h, trim_style,
                ) as i32;
                self.clip_trim_handle_ids.push(left_id);

                // Right trim hint
                let right_id = tree.add_panel(
                    -1, x2 - hint_w, clip_y, hint_w, clip_h, trim_style,
                ) as i32;
                self.clip_trim_handle_ids.push(right_id);
            }

            count += 1;
        }
    }

    fn build_selection_region(&mut self, tree: &mut UITree) {
        if let Some(ref sel) = self.selection_region {
            let x1 = self.beat_to_pixel(sel.start_beat).max(self.tracks_rect.x);
            let x2 = self.beat_to_pixel(sel.end_beat).min(self.tracks_rect.x_max());

            let y1 = self.track_y(sel.start_layer);
            let y2_layer = sel.end_layer.min(self.tracks.len().saturating_sub(1));
            let y2 = self.track_y(y2_layer) + self.track_height(y2_layer);

            if x2 > x1 && y2 > y1 {
                self.selection_region_id = tree.add_panel(
                    -1, x1, y1, x2 - x1, y2 - y1,
                    UIStyle {
                        bg_color: color::ACCENT_BLUE_SELECTION,
                        border_color: color::ACCENT_BLUE,
                        border_width: 1.0,
                        ..UIStyle::default()
                    },
                ) as i32;
            } else {
                self.selection_region_id = -1;
            }
        } else {
            self.selection_region_id = -1;
        }
    }

    fn build_export_markers(&mut self, tree: &mut UITree) {
        let has_range = self.export_in_beat < self.export_out_beat;
        if !has_range {
            self.export_range_id = -1;
            self.export_in_marker_id = -1;
            self.export_out_marker_id = -1;
            return;
        }

        let in_px = self.beat_to_pixel(self.export_in_beat);
        let out_px = self.beat_to_pixel(self.export_out_beat);
        let marker_w = 2.0;

        // Range highlight across tracks
        let range_left = in_px.max(self.tracks_rect.x);
        let range_right = out_px.min(self.tracks_rect.x_max());
        if range_right > range_left {
            self.export_range_id = tree.add_panel(
                -1, range_left, self.tracks_rect.y,
                range_right - range_left, self.tracks_rect.height,
                UIStyle { bg_color: color::EXPORT_RANGE_HIGHLIGHT, ..UIStyle::default() },
            ) as i32;
        } else {
            self.export_range_id = -1;
        }

        // In marker (vertical line on ruler + tracks)
        let in_visible = in_px >= self.tracks_rect.x && in_px <= self.tracks_rect.x_max();
        if in_visible {
            self.export_in_marker_id = tree.add_panel(
                -1, in_px - marker_w * 0.5, self.ruler_rect.y,
                marker_w, self.ruler_rect.height + self.tracks_rect.height,
                UIStyle { bg_color: color::EXPORT_MARKER_COLOR, ..UIStyle::default() },
            ) as i32;
        } else {
            self.export_in_marker_id = -1;
        }

        // Out marker
        let out_visible = out_px >= self.tracks_rect.x && out_px <= self.tracks_rect.x_max();
        if out_visible {
            self.export_out_marker_id = tree.add_panel(
                -1, out_px - marker_w * 0.5, self.ruler_rect.y,
                marker_w, self.ruler_rect.height + self.tracks_rect.height,
                UIStyle { bg_color: color::EXPORT_MARKER_COLOR, ..UIStyle::default() },
            ) as i32;
        } else {
            self.export_out_marker_id = -1;
        }
    }

    fn build_playhead(&mut self, tree: &mut UITree) {
        let px = self.beat_to_pixel(self.playhead_beat);
        let in_view = px >= self.tracks_rect.x && px <= self.tracks_rect.x_max();

        // Track playhead
        self.playhead_track_id = tree.add_panel(
            -1, centered_line_x(px, PLAYHEAD_WIDTH), self.tracks_rect.y,
            PLAYHEAD_WIDTH, self.tracks_rect.height,
            UIStyle { bg_color: color::PLAYHEAD_RED, ..UIStyle::default() },
        ) as i32;
        if !in_view {
            tree.set_visible(self.playhead_track_id as u32, false);
        }

        // Ruler playhead
        self.playhead_ruler_id = tree.add_panel(
            -1, centered_line_x(px, PLAYHEAD_WIDTH), self.ruler_rect.y,
            PLAYHEAD_WIDTH, self.ruler_rect.height,
            UIStyle { bg_color: color::PLAYHEAD_RED, ..UIStyle::default() },
        ) as i32;
        if !in_view {
            tree.set_visible(self.playhead_ruler_id as u32, false);
        }
    }

    fn build_insert_cursor(&mut self, tree: &mut UITree) {
        let px = self.beat_to_pixel(self.insert_cursor_beat);
        let in_view = px >= self.tracks_rect.x && px <= self.tracks_rect.x_max();

        // Track cursor line
        self.insert_cursor_track_id = tree.add_panel(
            -1, centered_line_x(px, INSERT_CURSOR_WIDTH), self.tracks_rect.y,
            INSERT_CURSOR_WIDTH, self.tracks_rect.height,
            UIStyle { bg_color: color::INSERT_CURSOR_BLUE, ..UIStyle::default() },
        ) as i32;
        if !in_view {
            tree.set_visible(self.insert_cursor_track_id as u32, false);
        }

        // Ruler marker (small triangle/rect)
        let marker_s = color::INSERT_CURSOR_RULER_MARKER_SIZE;
        self.insert_cursor_ruler_id = tree.add_panel(
            -1, px - marker_s * 0.5, self.ruler_rect.y + self.ruler_rect.height - marker_s,
            marker_s, marker_s,
            UIStyle { bg_color: color::INSERT_CURSOR_BLUE, ..UIStyle::default() },
        ) as i32;
        if !in_view {
            tree.set_visible(self.insert_cursor_ruler_id as u32, false);
        }
    }
}

impl Default for TimelineViewportPanel {
    fn default() -> Self { Self::new() }
}

// ── Helpers ──────────────────────────────────────────────────────

/// Determine clip visual color following Unity's priority chain:
/// locked → selected → hovered → normal.
/// Muted is a POST-PROCESS blend: average (base + MutedColor) / 2 per channel.
/// From Unity LayerBitmapRenderer visual state logic.
fn get_clip_color(clip: &ViewportClip, is_selected: bool, is_hovered: bool) -> Color32 {
    // Priority: locked → selected → hovered → normal
    let base = if clip.is_locked {
        color::CLIP_LOCKED
    } else if is_selected {
        if clip.is_generator { color::CLIP_GEN_SELECTED } else { color::CLIP_SELECTED }
    } else if is_hovered {
        if clip.is_generator { color::CLIP_GEN_HOVER } else { color::CLIP_HOVER }
    } else {
        if clip.is_generator { color::CLIP_GEN_NORMAL } else { clip.color }
    };

    // Muted post-process: blend 50% with MutedColor (rust-orange tint)
    if clip.is_muted {
        let m = color::MUTED_COLOR;
        Color32::new(
            ((base.r as u16 + m.r as u16) / 2) as u8,
            ((base.g as u16 + m.g as u16) / 2) as u8,
            ((base.b as u16 + m.b as u16) / 2) as u8,
            base.a,
        )
    } else {
        base
    }
}

// ── Tests ────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::UITree;
    use crate::layout::ScreenLayout;

    fn test_layout() -> ScreenLayout {
        ScreenLayout::new(1920.0, 1080.0)
    }

    fn test_tracks() -> Vec<TrackInfo> {
        vec![
            TrackInfo { height: 140.0, ..Default::default() },
            TrackInfo { height: 140.0, ..Default::default() },
        ]
    }

    fn test_clips() -> Vec<ViewportClip> {
        vec![
            ViewportClip {
                clip_id: "clip_001".into(),
                layer_index: 0,
                start_beat: 0.0,
                duration_beats: 4.0,
                name: "Intro".into(),
                color: color::CLIP_NORMAL,
                is_muted: false,
                is_locked: false,
                is_generator: false,
            },
            ViewportClip {
                clip_id: "clip_002".into(),
                layer_index: 1,
                start_beat: 4.0,
                duration_beats: 8.0,
                name: "Main".into(),
                color: color::CLIP_GEN_NORMAL,
                is_muted: false,
                is_locked: false,
                is_generator: true,
            },
        ]
    }

    #[test]
    fn build_empty_viewport() {
        let mut tree = UITree::new();
        let mut panel = TimelineViewportPanel::new();
        let layout = test_layout();
        panel.build(&mut tree, &layout);

        assert!(panel.bg_panel_id >= 0);
        assert!(panel.ruler_bg_id >= 0);
        assert!(panel.node_count > 0);
    }

    #[test]
    fn build_with_tracks_and_clips() {
        let mut tree = UITree::new();
        let mut panel = TimelineViewportPanel::new();
        let layout = test_layout();

        panel.set_tracks(test_tracks());
        panel.set_clips(test_clips());
        panel.build(&mut tree, &layout);

        // Should have clip nodes
        assert!(!panel.clip_bg_ids.is_empty());
        // Should have track backgrounds
        assert!(!panel.track_bg_ids.is_empty());
        // Should have grid lines
        assert!(!panel.grid_line_ids.is_empty());
    }

    #[test]
    fn coordinate_mapping_roundtrip() {
        let mut panel = TimelineViewportPanel::new();
        panel.tracks_rect = Rect::new(100.0, 0.0, 1000.0, 500.0);
        panel.set_zoom(120.0);
        panel.scroll_x_beats = 0.0;

        let beat = 4.0;
        let px = panel.beat_to_pixel(beat);
        let beat_back = panel.pixel_to_beat(px);
        assert!((beat - beat_back).abs() < 0.001);
    }

    #[test]
    fn coordinate_mapping_with_scroll() {
        let mut panel = TimelineViewportPanel::new();
        panel.tracks_rect = Rect::new(0.0, 0.0, 1000.0, 500.0);
        panel.set_zoom(100.0);
        panel.scroll_x_beats = 4.0;

        // Beat 4 should be at x=0 when scrolled to beat 4
        let px = panel.beat_to_pixel(4.0);
        assert!((px - 0.0).abs() < 0.001);

        // Beat 5 should be at x=100
        let px = panel.beat_to_pixel(5.0);
        assert!((px - 100.0).abs() < 0.001);
    }

    #[test]
    fn grid_subdivision_levels() {
        let mut panel = TimelineViewportPanel::new();
        panel.beats_per_bar = 4;

        // Very zoomed out: ppb=1, beat < 6px → Bar
        panel.set_zoom(1.0);
        assert_eq!(panel.grid_subdivision(), GridSubdivision::Bar);

        // ppb=8: beat=8px ≥ 6 → Beat; eighth=4px < 6 → not Eighth
        panel.set_zoom(8.0);
        assert_eq!(panel.grid_subdivision(), GridSubdivision::Beat);

        // ppb=14: eighth=7px ≥ 6 → Eighth; sixteenth=3.5px < 4 → not Sixteenth
        panel.set_zoom(14.0);
        assert_eq!(panel.grid_subdivision(), GridSubdivision::Eighth);

        // ppb=20: sixteenth=5px ≥ 4 → Sixteenth
        panel.set_zoom(20.0);
        assert_eq!(panel.grid_subdivision(), GridSubdivision::Sixteenth);
    }

    #[test]
    fn click_ruler_seeks() {
        let mut tree = UITree::new();
        let mut panel = TimelineViewportPanel::new();
        let layout = test_layout();
        panel.set_tracks(test_tracks());
        panel.build(&mut tree, &layout);

        // Click in ruler area
        let ruler_pos = Vec2::new(
            panel.ruler_rect.x + 100.0,
            panel.ruler_rect.y + 5.0,
        );
        let actions = panel.handle_event(
            &UIEvent::Click { node_id: 0, pos: ruler_pos, modifiers: Modifiers::default() },
            &tree,
        );
        assert_eq!(actions.len(), 1);
        assert!(matches!(actions[0], PanelAction::Seek(_)));
    }

    #[test]
    fn click_empty_tracks_sets_insert_cursor() {
        let mut tree = UITree::new();
        let mut panel = TimelineViewportPanel::new();
        let layout = test_layout();
        panel.set_tracks(test_tracks());
        panel.build(&mut tree, &layout);

        // Plain click in tracks area (no clips) → SetInsertCursor (matches Unity)
        let tracks_pos = Vec2::new(
            panel.tracks_rect.x + 100.0,
            panel.tracks_rect.y + 50.0,
        );
        let actions = panel.handle_event(
            &UIEvent::Click { node_id: 0, pos: tracks_pos, modifiers: Modifiers::default() },
            &tree,
        );
        assert_eq!(actions.len(), 1);
        assert!(matches!(actions[0], PanelAction::SetInsertCursor(_)));
    }

    #[test]
    fn shift_click_empty_tracks_emits_track_clicked() {
        let mut tree = UITree::new();
        let mut panel = TimelineViewportPanel::new();
        let layout = test_layout();
        panel.set_tracks(test_tracks());
        panel.build(&mut tree, &layout);

        // Shift+click in tracks area → TrackClicked with shift (extends region)
        let tracks_pos = Vec2::new(
            panel.tracks_rect.x + 100.0,
            panel.tracks_rect.y + 50.0,
        );
        let actions = panel.handle_event(
            &UIEvent::Click {
                node_id: 0,
                pos: tracks_pos,
                modifiers: Modifiers { shift: true, ..Modifiers::default() },
            },
            &tree,
        );
        assert_eq!(actions.len(), 1);
        assert!(matches!(actions[0], PanelAction::TrackClicked(_, _, _)));
    }

    #[test]
    fn sync_playhead_moves_node() {
        let mut tree = UITree::new();
        let mut panel = TimelineViewportPanel::new();
        let layout = test_layout();
        panel.set_tracks(test_tracks());
        panel.build(&mut tree, &layout);

        let original_bounds = tree.get_bounds(panel.playhead_track_id as u32);

        panel.playhead_beat = 8.0;
        panel.sync_playhead(&mut tree);

        let new_bounds = tree.get_bounds(panel.playhead_track_id as u32);
        assert!(new_bounds.x != original_bounds.x);
    }

    #[test]
    fn offscreen_clips_not_rendered() {
        let mut tree = UITree::new();
        let mut panel = TimelineViewportPanel::new();
        let layout = test_layout();
        panel.set_tracks(test_tracks());

        // Put clips far off-screen
        panel.set_clips(vec![
            ViewportClip {
                clip_id: "clip_099".into(),
                layer_index: 0,
                start_beat: 1000.0,
                duration_beats: 4.0,
                name: "Far".into(),
                color: color::CLIP_NORMAL,
                is_muted: false,
                is_locked: false,
                is_generator: false,
            },
        ]);
        panel.build(&mut tree, &layout);

        // No clip nodes should be created for off-screen clips
        assert!(panel.clip_bg_ids.is_empty());
    }

    #[test]
    fn selection_region_renders() {
        let mut tree = UITree::new();
        let mut panel = TimelineViewportPanel::new();
        let layout = test_layout();
        panel.set_tracks(test_tracks());
        panel.set_selection_region(Some(SelectionRegion {
            start_beat: 0.0,
            end_beat: 4.0,
            start_layer: 0,
            end_layer: 1,
        }));
        panel.build(&mut tree, &layout);

        assert!(panel.selection_region_id >= 0);
    }

    #[test]
    fn clip_color_states() {
        let clip = ViewportClip {
            clip_id: "test".into(), layer_index: 0, start_beat: 0.0, duration_beats: 1.0,
            name: "Test".into(), color: color::CLIP_NORMAL,
            is_muted: false, is_locked: false, is_generator: false,
        };

        let normal = get_clip_color(&clip, false, false);
        let selected = get_clip_color(&clip, true, false);
        let hovered = get_clip_color(&clip, false, true);

        assert_eq!(normal, color::CLIP_NORMAL);
        assert_eq!(selected, color::CLIP_SELECTED);
        assert_eq!(hovered, color::CLIP_HOVER);
    }
}
