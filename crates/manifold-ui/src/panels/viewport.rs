use crate::color;
use crate::input::UIEvent;
#[cfg(test)]
use crate::input::Modifiers;
use crate::layout::ScreenLayout;
use crate::node::*;
use crate::tree::UITree;
use super::{Panel, PanelAction};

// ── Layout constants ────────────────────────────────────────────

const RULER_HEIGHT: f32 = color::RULER_HEIGHT;
const PLAYHEAD_WIDTH: f32 = color::PLAYHEAD_WIDTH;
const INSERT_CURSOR_WIDTH: f32 = 1.0;
const CLIP_VERTICAL_PAD: f32 = 4.0;
const CLIP_LABEL_PAD: f32 = 4.0;
const CLIP_CORNER_RADIUS: f32 = 2.0;
const CLIP_BORDER_WIDTH: f32 = 1.0;
const CLIP_MIN_WIDTH_PX: f32 = color::CLIP_MIN_WIDTH;
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
    pub accent_color: Option<Color32>,
}

impl Default for TrackInfo {
    fn default() -> Self {
        Self {
            height: color::TRACK_HEIGHT,
            is_muted: false,
            is_group: false,
            accent_color: None,
        }
    }
}

// ── TimelineViewportPanel ───────────────────────────────────────

pub struct TimelineViewportPanel {
    // Coordinate mapping
    pixels_per_beat: f32,
    scroll_x_beats: f32,
    scroll_y_px: f32,
    beats_per_bar: u32,

    // Track layout
    tracks: Vec<TrackInfo>,
    track_y_offsets: Vec<f32>,
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ViewportDragMode {
    None,
    ClipDrag,
    RegionDrag,
    RulerScrub,
}

impl TimelineViewportPanel {
    pub fn new() -> Self {
        Self {
            pixels_per_beat: color::ZOOM_LEVELS[color::DEFAULT_ZOOM_INDEX],
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
        }
    }

    // ── Configuration ─────────────────────────────────────────────

    pub fn set_tracks(&mut self, tracks: Vec<TrackInfo>) {
        self.tracks = tracks;
        self.recompute_track_layout();
    }

    pub fn set_clips(&mut self, clips: Vec<ViewportClip>) {
        self.clips = clips;
    }

    pub fn set_zoom(&mut self, pixels_per_beat: f32) {
        self.pixels_per_beat = pixels_per_beat.max(1.0);
    }

    pub fn set_zoom_index(&mut self, index: usize) {
        if let Some(&ppb) = color::ZOOM_LEVELS.get(index) {
            self.pixels_per_beat = ppb;
        }
    }

    pub fn set_scroll(&mut self, scroll_x_beats: f32, scroll_y_px: f32) {
        self.scroll_x_beats = scroll_x_beats.max(0.0);
        self.scroll_y_px = scroll_y_px.max(0.0);
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

    pub fn pixels_per_beat(&self) -> f32 { self.pixels_per_beat }
    pub fn scroll_x_beats(&self) -> f32 { self.scroll_x_beats }
    pub fn scroll_y_px(&self) -> f32 { self.scroll_y_px }
    pub fn viewport_rect(&self) -> Rect { self.viewport_rect }
    pub fn ruler_rect(&self) -> Rect { self.ruler_rect }
    pub fn tracks_rect(&self) -> Rect { self.tracks_rect }
    pub fn first_node(&self) -> usize { self.first_node }
    pub fn node_count(&self) -> usize { self.node_count }

    // ── Coordinate mapping ────────────────────────────────────────

    /// Convert beat position to pixel X in the tracks area.
    pub fn beat_to_pixel(&self, beat: f32) -> f32 {
        (beat - self.scroll_x_beats) * self.pixels_per_beat + self.tracks_rect.x
    }

    /// Convert pixel X in the tracks area to beat position.
    pub fn pixel_to_beat(&self, px: f32) -> f32 {
        (px - self.tracks_rect.x) / self.pixels_per_beat + self.scroll_x_beats
    }

    /// Convert beat duration to pixel width.
    pub fn beat_duration_to_width(&self, beats: f32) -> f32 {
        beats * self.pixels_per_beat
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
        let max_beat = min_beat + self.tracks_rect.width / self.pixels_per_beat;
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

        // Iterate clips on this layer in reverse order (topmost/last wins)
        for clip in self.clips.iter().rev() {
            if clip.layer_index != layer_index {
                continue;
            }

            let clip_end = clip.start_beat + clip.duration_beats;
            if beat < clip.start_beat || beat >= clip_end {
                continue;
            }

            let clip_width_px = clip.duration_beats * self.pixels_per_beat;
            let local_px = (beat - clip.start_beat) * self.pixels_per_beat;

            let region = if clip_width_px > 24.0 && local_px < 8.0 {
                HitRegion::TrimLeft
            } else if clip_width_px > 24.0 && local_px > clip_width_px - 8.0 {
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
    /// Returns the best snap point within `SNAP_THRESHOLD_PX` pixels (12px), or the
    /// grid-snapped beat if no clip edge is closer.
    /// `ignore_ids` are clip IDs being dragged (don't snap to self).
    pub fn magnetic_snap(&self, beat: f32, layer_index: usize, ignore_ids: &[String]) -> f32 {
        const SNAP_THRESHOLD_PX: f32 = 12.0;

        // Clamp threshold to avoid snapping across bars at low zoom
        let max_snap_beats = 0.5_f32;
        let threshold_beats = (SNAP_THRESHOLD_PX / self.pixels_per_beat).min(max_snap_beats);

        let grid_snapped = self.snap_to_grid(beat);
        let mut best_beat = grid_snapped;
        let mut best_dist = (grid_snapped - beat).abs();

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

    /// Current grid step size in beats.
    pub fn grid_step(&self) -> f32 {
        match self.grid_subdivision() {
            GridSubdivision::Bar => self.beats_per_bar as f32,
            GridSubdivision::Beat => 1.0,
            GridSubdivision::Eighth => 0.5,
            GridSubdivision::Sixteenth => 0.25,
        }
    }

    // ── Track layout ──────────────────────────────────────────────

    fn recompute_track_layout(&mut self) {
        self.track_y_offsets.clear();
        let mut y = 0.0;
        for track in &self.tracks {
            self.track_y_offsets.push(y);
            y += track.height;
        }
        self.total_tracks_height = y;
    }

    // ── Grid subdivision ──────────────────────────────────────────

    /// Determine grid subdivision level based on zoom.
    fn grid_subdivision(&self) -> GridSubdivision {
        let bar_width = self.pixels_per_beat * self.beats_per_bar as f32;
        if bar_width >= 400.0 {
            GridSubdivision::Sixteenth
        } else if bar_width >= 200.0 {
            GridSubdivision::Eighth
        } else if bar_width >= 80.0 {
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
                    Rect::new(px - PLAYHEAD_WIDTH * 0.5, self.tracks_rect.y,
                              PLAYHEAD_WIDTH, self.tracks_rect.height),
                );
            }
        }

        if self.playhead_ruler_id >= 0 {
            tree.set_visible(self.playhead_ruler_id as u32, in_view);
            if in_view {
                tree.set_bounds(
                    self.playhead_ruler_id as u32,
                    Rect::new(px - PLAYHEAD_WIDTH * 0.5, self.ruler_rect.y,
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
                    Rect::new(px - INSERT_CURSOR_WIDTH * 0.5, self.tracks_rect.y,
                              INSERT_CURSOR_WIDTH, self.tracks_rect.height),
                );
            }
        }

        if self.insert_cursor_ruler_id >= 0 {
            tree.set_visible(self.insert_cursor_ruler_id as u32, in_view);
            if in_view {
                let marker_h = 6.0;
                tree.set_bounds(
                    self.insert_cursor_ruler_id as u32,
                    Rect::new(px - 3.0, self.ruler_rect.y + self.ruler_rect.height - marker_h,
                              6.0, marker_h),
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

        self.viewport_rect = Rect::new(body.x, body.y, tracks_w, body.height);
        self.ruler_rect = Rect::new(body.x, body.y, tracks_w, RULER_HEIGHT);
        self.tracks_rect = Rect::new(
            body.x,
            body.y + RULER_HEIGHT,
            tracks_w,
            (body.height - RULER_HEIGHT).max(0.0),
        );

        // Background
        self.bg_panel_id = tree.add_panel(
            -1, self.viewport_rect.x, self.viewport_rect.y,
            self.viewport_rect.width, self.viewport_rect.height,
            UIStyle { bg_color: color::DARK_BG, ..UIStyle::default() },
        ) as i32;

        // Ruler background
        self.ruler_bg_id = tree.add_panel(
            -1, self.ruler_rect.x, self.ruler_rect.y,
            self.ruler_rect.width, self.ruler_rect.height,
            UIStyle { bg_color: color::HEADER_BG, ..UIStyle::default() },
        ) as i32;

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
            UIEvent::Click { pos, modifiers, .. } => {
                if self.ruler_rect.contains(*pos) {
                    let beat = self.pixel_to_beat(pos.x);
                    return vec![PanelAction::Seek(beat)];
                }
                if self.tracks_rect.contains(*pos) {
                    if let Some(hit) = self.hit_test_clip(*pos) {
                        return vec![PanelAction::ClipClicked(hit.clip_id, *modifiers)];
                    } else {
                        let beat = self.pixel_to_beat(pos.x);
                        if let Some(layer) = self.layer_at_y(pos.y) {
                            return vec![PanelAction::TrackClicked(beat, layer, *modifiers)];
                        }
                        return vec![PanelAction::SetInsertCursor(beat)];
                    }
                }
            }
            UIEvent::DoubleClick { pos, .. } => {
                if self.tracks_rect.contains(*pos) {
                    if let Some(hit) = self.hit_test_clip(*pos) {
                        return vec![PanelAction::ClipDoubleClicked(hit.clip_id)];
                    } else {
                        let beat = self.pixel_to_beat(pos.x);
                        if let Some(layer) = self.layer_at_y(pos.y) {
                            return vec![PanelAction::TrackDoubleClicked(beat, layer)];
                        }
                    }
                }
            }
            UIEvent::DragBegin { pos, .. } => {
                if self.ruler_rect.contains(*pos) {
                    self.drag_mode = ViewportDragMode::RulerScrub;
                    let beat = self.pixel_to_beat(pos.x).max(0.0);
                    return vec![PanelAction::Seek(beat)];
                }
                if self.tracks_rect.contains(*pos) {
                    let beat = self.pixel_to_beat(pos.x);
                    if let Some(hit) = self.hit_test_clip(*pos) {
                        self.drag_mode = ViewportDragMode::ClipDrag;
                        return vec![PanelAction::ClipDragStarted(hit.clip_id, hit.region, beat)];
                    } else if let Some(layer) = self.layer_at_y(pos.y) {
                        self.drag_mode = ViewportDragMode::RegionDrag;
                        return vec![PanelAction::RegionDragStarted(beat, layer)];
                    }
                }
            }
            UIEvent::Drag { pos, .. } => {
                let beat = self.pixel_to_beat(pos.x);
                let layer = self.layer_at_y(pos.y);
                match self.drag_mode {
                    ViewportDragMode::RulerScrub => {
                        return vec![PanelAction::Seek(beat.max(0.0))];
                    }
                    ViewportDragMode::ClipDrag => {
                        return vec![PanelAction::ClipDragMoved(beat, layer)];
                    }
                    ViewportDragMode::RegionDrag => {
                        if let Some(layer) = layer {
                            return vec![PanelAction::RegionDragMoved(beat, layer)];
                        }
                    }
                    ViewportDragMode::None => {}
                }
            }
            UIEvent::DragEnd { .. } => {
                let was_dragging = self.drag_mode;
                self.drag_mode = ViewportDragMode::None;
                match was_dragging {
                    ViewportDragMode::RulerScrub => {} // No commit needed
                    ViewportDragMode::ClipDrag => {
                        return vec![PanelAction::ClipDragEnded];
                    }
                    ViewportDragMode::RegionDrag => {
                        return vec![PanelAction::RegionDragEnded];
                    }
                    ViewportDragMode::None => {}
                }
            }
            UIEvent::RightClick { pos, .. } => {
                if self.tracks_rect.contains(*pos) {
                    let beat = self.pixel_to_beat(pos.x);
                    if let Some(hit) = self.hit_test_clip(*pos) {
                        return vec![PanelAction::ClipRightClicked(hit.clip_id)];
                    } else if let Some(layer) = self.layer_at_y(pos.y) {
                        return vec![PanelAction::TrackRightClicked(beat, layer)];
                    }
                }
            }
            UIEvent::HoverEnter { pos, .. } | UIEvent::PointerDown { pos, .. } => {
                if self.tracks_rect.contains(*pos) {
                    let hit = self.hit_test_clip(*pos);
                    let new_id = hit.map(|h| h.clip_id);
                    if new_id != self.hovered_clip_id {
                        self.hovered_clip_id = new_id.clone();
                        return vec![PanelAction::ViewportHoverChanged(new_id)];
                    }
                }
            }
            _ => {}
        }
        Vec::new()
    }
}

// ── Build helpers (private) ──────────────────────────────────────

impl TimelineViewportPanel {
    fn build_track_backgrounds(&mut self, tree: &mut UITree) {
        self.track_bg_ids.clear();

        for (i, track) in self.tracks.iter().enumerate() {
            let y = self.track_y(i);
            let h = track.height;

            // Skip if completely outside viewport
            if y + h < self.tracks_rect.y || y > self.tracks_rect.y_max() {
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
                -1, self.tracks_rect.x, y,
                self.tracks_rect.width, h,
                style,
            ) as i32;
            self.track_bg_ids.push(id);

            // Group accent bar
            if track.is_group {
                if let Some(accent) = track.accent_color {
                    tree.add_panel(
                        -1, self.tracks_rect.x, y,
                        color::GROUP_ACCENT_BAR_WIDTH, h,
                        UIStyle { bg_color: accent, ..UIStyle::default() },
                    );
                }
            }

            // Bottom separator
            tree.add_panel(
                -1, self.tracks_rect.x, y + h - 1.0,
                self.tracks_rect.width, 1.0,
                UIStyle { bg_color: color::SEPARATOR_COLOR, ..UIStyle::default() },
            );
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

                let id = tree.add_panel(
                    -1, px, self.tracks_rect.y,
                    GRID_LINE_W, self.tracks_rect.height,
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

            let clip_y = track_y + CLIP_VERTICAL_PAD;
            let clip_h = track_h - CLIP_VERTICAL_PAD * 2.0;

            // Determine clip color
            let is_selected = self.selected_clip_ids.contains(&clip.clip_id);
            let is_hovered = self.hovered_clip_id.as_ref() == Some(&clip.clip_id);
            let clip_color = get_clip_color(clip, is_selected, is_hovered);

            // Clip background
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
                    border_color: if is_selected { color::SELECTED_BORDER } else { Color32::TRANSPARENT },
                    border_width: if is_selected { CLIP_BORDER_WIDTH } else { 0.0 },
                    ..UIStyle::default()
                },
                "",
            ) as i32;
            self.clip_bg_ids.push(bg_id);

            // Left separator (Ableton-style dark edge)
            if clip_w > 6.0 {
                let sep_id = tree.add_panel(
                    -1, x1, clip_y, 2.0, clip_h,
                    UIStyle {
                        bg_color: color::CLIP_SEPARATOR,
                        corner_radius: CLIP_CORNER_RADIUS,
                        ..UIStyle::default()
                    },
                ) as i32;
                self.clip_border_ids.push(sep_id);
            }

            // Clip name label (if wide enough)
            if clip_w > CLIP_MIN_WIDTH_PX + CLIP_LABEL_PAD * 2.0 {
                let label_x = x1 + CLIP_LABEL_PAD + 2.0; // past separator
                let label_w = clip_w - CLIP_LABEL_PAD * 2.0 - 2.0;
                let text_color = if clip.is_generator {
                    Color32::new(20, 20, 22, 255)
                } else {
                    Color32::new(20, 20, 22, 220)
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

            // Trim handle indicators (on hovered or selected clips, when wide enough)
            if (is_selected || is_hovered) && clip_w > 24.0 {
                let handle_w = 8.0_f32.min(clip_w * 0.25);
                let trim_style = UIStyle {
                    bg_color: color::TRIM_HANDLE_COLOR,
                    corner_radius: CLIP_CORNER_RADIUS,
                    ..UIStyle::default()
                };

                // Left trim handle
                let left_id = tree.add_panel(
                    -1, x1, clip_y, handle_w, clip_h, trim_style,
                ) as i32;
                self.clip_trim_handle_ids.push(left_id);

                // Right trim handle
                let right_id = tree.add_panel(
                    -1, x2 - handle_w, clip_y, handle_w, clip_h, trim_style,
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

    fn build_playhead(&mut self, tree: &mut UITree) {
        let px = self.beat_to_pixel(self.playhead_beat);
        let in_view = px >= self.tracks_rect.x && px <= self.tracks_rect.x_max();

        // Track playhead
        self.playhead_track_id = tree.add_panel(
            -1, px - PLAYHEAD_WIDTH * 0.5, self.tracks_rect.y,
            PLAYHEAD_WIDTH, self.tracks_rect.height,
            UIStyle { bg_color: color::PLAYHEAD_RED, ..UIStyle::default() },
        ) as i32;
        if !in_view {
            tree.set_visible(self.playhead_track_id as u32, false);
        }

        // Ruler playhead
        self.playhead_ruler_id = tree.add_panel(
            -1, px - PLAYHEAD_WIDTH * 0.5, self.ruler_rect.y,
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
            -1, px - INSERT_CURSOR_WIDTH * 0.5, self.tracks_rect.y,
            INSERT_CURSOR_WIDTH, self.tracks_rect.height,
            UIStyle { bg_color: color::INSERT_CURSOR_BLUE, ..UIStyle::default() },
        ) as i32;
        if !in_view {
            tree.set_visible(self.insert_cursor_track_id as u32, false);
        }

        // Ruler marker (small triangle/rect)
        let marker_h = 6.0;
        self.insert_cursor_ruler_id = tree.add_panel(
            -1, px - 3.0, self.ruler_rect.y + self.ruler_rect.height - marker_h,
            6.0, marker_h,
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

fn get_clip_color(clip: &ViewportClip, is_selected: bool, is_hovered: bool) -> Color32 {
    if clip.is_locked {
        return color::CLIP_LOCKED;
    }
    if clip.is_muted {
        let base = if clip.is_generator { color::CLIP_GEN_NORMAL } else { color::CLIP_NORMAL };
        return Color32::new(base.r / 2, base.g / 2, base.b / 2, 128);
    }

    if clip.is_generator {
        if is_selected { color::CLIP_GEN_SELECTED }
        else if is_hovered { color::CLIP_GEN_HOVER }
        else { clip.color }
    } else {
        if is_selected { color::CLIP_SELECTED }
        else if is_hovered { color::CLIP_HOVER }
        else { clip.color }
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
        panel.pixels_per_beat = 120.0;
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
        panel.pixels_per_beat = 100.0;
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

        panel.pixels_per_beat = 1.0; // Very zoomed out: bar_width = 4
        assert_eq!(panel.grid_subdivision(), GridSubdivision::Bar);

        panel.pixels_per_beat = 40.0; // bar_width = 160
        assert_eq!(panel.grid_subdivision(), GridSubdivision::Beat);

        panel.pixels_per_beat = 80.0; // bar_width = 320
        assert_eq!(panel.grid_subdivision(), GridSubdivision::Eighth);

        panel.pixels_per_beat = 200.0; // bar_width = 800
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
    fn click_tracks_emits_track_clicked() {
        let mut tree = UITree::new();
        let mut panel = TimelineViewportPanel::new();
        let layout = test_layout();
        panel.set_tracks(test_tracks());
        panel.build(&mut tree, &layout);

        // Click in tracks area (no clips, so should be TrackClicked)
        let tracks_pos = Vec2::new(
            panel.tracks_rect.x + 100.0,
            panel.tracks_rect.y + 50.0,
        );
        let actions = panel.handle_event(
            &UIEvent::Click { node_id: 0, pos: tracks_pos, modifiers: Modifiers::default() },
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
