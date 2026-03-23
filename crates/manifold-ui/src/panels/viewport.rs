use manifold_core::{ClipId, LayerId};
use crate::color;
use crate::coordinate_mapper::CoordinateMapper;
use crate::input::UIEvent;
use crate::layout::ScreenLayout;
use crate::node::*;
use crate::snap;
use crate::tree::UITree;
use super::{Panel, PanelAction};

// ── Layout constants ────────────────────────────────────────────

const RULER_HEIGHT: f32 = color::RULER_HEIGHT;
const CLIP_VERTICAL_PAD: f32 = 12.0;

const RULER_FONT_SIZE: u16 = 9;
const RULER_TICK_W: f32 = 1.0;
const RULER_BEAT_TICK_H: f32 = 8.0;
const RULER_BAR_TICK_H: f32 = 14.0;
const RULER_LABEL_H: f32 = 14.0;
const RULER_LABEL_W: f32 = 40.0;
// Maximum nodes to allocate for ruler ticks (avoid unbounded allocation)
const MAX_RULER_TICKS: usize = 1500;

// ── Data types ──────────────────────────────────────────────────

/// A clip to be rendered in the timeline viewport.
#[derive(Debug, Clone)]
pub struct ViewportClip {
    pub clip_id: ClipId,
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
    pub clip_id: ClipId,
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

    // Layer IDs (kept in sync with project layers)
    pub layer_ids: Vec<LayerId>,

    // Track layout (kept in sync with mapper via set_tracks)
    tracks: Vec<TrackInfo>,
    track_y_offsets: Vec<f32>,    // cumulative Y offsets from tracks
    total_tracks_height: f32,

    // Clip data
    clips: Vec<ViewportClip>,
    clips_by_layer: Vec<Vec<ViewportClip>>,

    // Per-layer bitmap renderers (None for group layers)
    bitmap_renderers: Vec<Option<crate::bitmap_renderer::LayerBitmapRenderer>>,
    render_scale: f32,

    // Playback state
    playhead_beat: f32,
    insert_cursor_beat: f32,
    is_playing: bool,
    selection_region: Option<SelectionRegion>,
    selected_clip_ids: Vec<ClipId>,
    hovered_clip_id: Option<ClipId>,

    // Viewport rects
    viewport_rect: Rect,
    overview_rect: Rect,
    ruler_rect: Rect,
    tracks_rect: Rect,

    // Node IDs — fixed elements
    bg_panel_id: i32,
    overview_btn_id: i32,
    ruler_bg_id: i32,
    // playhead: unified overlay quad in app.rs (ruler → waveform → stems → tracks)
    insert_cursor_ruler_id: i32,
    // insert_cursor_track_id: removed — painted into bitmap
    // selection_region_id: removed — painted into bitmap

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
    // grid_line_ids: removed — grid painted into bitmap
    track_bg_ids: Vec<i32>,
    // clip_bg_ids, clip_label_ids, clip_border_ids, clip_trim_handle_ids: removed — painted into bitmap

    // Node range
    first_node: usize,
    node_count: usize,

    // Drag interaction state
    drag_mode: ViewportDragMode,
    /// True when Alt was held at drag start — bypasses grid snapping for
    /// sample-accurate scrubbing. Unity: `RulerScrubHandler.ShouldUseFreeScrub()`.
    scrub_free: bool,

    // Dirty-checking fingerprint to skip unnecessary rebuilds.
    // From Unity LayerBitmapRenderer dirty-checking (lines 99-186).
    cached_fingerprint: u64,
}

/// Viewport-local drag mode. Only tracks ruler scrub — all clip interaction
/// (move, trim, region) is handled by InteractionOverlay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ViewportDragMode {
    None,
    RulerScrub,
    OverviewScrub,
}

impl TimelineViewportPanel {
    pub fn new() -> Self {
        Self {
            mapper: CoordinateMapper::new(),
            scroll_x_beats: 0.0,
            scroll_y_px: 0.0,
            beats_per_bar: 4,
            layer_ids: Vec::new(),
            tracks: Vec::new(),
            track_y_offsets: Vec::new(),
            total_tracks_height: 0.0,
            clips: Vec::new(),
            clips_by_layer: Vec::new(),
            bitmap_renderers: Vec::new(),
            render_scale: 2.0, // default HiDPI (macOS Retina)
            playhead_beat: 0.0,
            insert_cursor_beat: 0.0,
            is_playing: false,
            selection_region: None,
            selected_clip_ids: Vec::new(),
            hovered_clip_id: None,
            viewport_rect: Rect::ZERO,
            overview_rect: Rect::ZERO,
            ruler_rect: Rect::ZERO,
            tracks_rect: Rect::ZERO,
            bg_panel_id: -1,
            overview_btn_id: -1,
            ruler_bg_id: -1,
            insert_cursor_ruler_id: -1,
            export_in_beat: 0.0,
            export_out_beat: 0.0,
            export_range_id: -1,
            export_in_marker_id: -1,
            export_out_marker_id: -1,
            ruler_tick_ids: Vec::new(),
            ruler_label_ids: Vec::new(),
            track_bg_ids: Vec::new(),
            first_node: 0,
            node_count: 0,
            drag_mode: ViewportDragMode::None,
            scrub_free: false,
            cached_fingerprint: 0,
        }
    }

    /// Look up a LayerId by visual index.
    pub fn layer_id_at_index(&self, i: usize) -> Option<&LayerId> {
        self.layer_ids.get(i)
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

        // Create/resize bitmap renderers (one per non-group layer)
        self.bitmap_renderers.clear();
        for (i, track) in self.tracks.iter().enumerate() {
            if track.is_group || track.height <= 0.0 {
                self.bitmap_renderers.push(None);
            } else {
                self.bitmap_renderers.push(Some(
                    crate::bitmap_renderer::LayerBitmapRenderer::new(
                        i,
                        self.render_scale,
                        track.height,
                        CLIP_VERTICAL_PAD,
                    ),
                ));
            }
        }
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
        // Bucket clips by layer for per-layer bitmap rendering
        self.clips_by_layer.clear();
        self.clips_by_layer.resize(self.tracks.len(), Vec::new());
        for clip in &clips {
            if clip.layer_index < self.clips_by_layer.len() {
                self.clips_by_layer[clip.layer_index].push(clip.clone());
            }
        }
        self.clips = clips;
    }

    pub fn set_render_scale(&mut self, scale: f32) {
        self.render_scale = scale.max(1.0);
        for r in self.bitmap_renderers.iter_mut().flatten() {
            r.set_render_scale(self.render_scale);
        }
    }

    /// Playhead pixel X position in the tracks area (for overlay rendering in app.rs).
    /// Returns None if the playhead is outside the visible viewport.
    pub fn playhead_pixel(&self) -> Option<f32> {
        let px = self.beat_to_pixel(self.playhead_beat);
        if px >= self.tracks_rect.x && px <= self.tracks_rect.x_max() {
            Some(px)
        } else {
            None
        }
    }

    /// Tracks area rect (screen-space). Used by app.rs for overlay rendering.
    pub fn get_tracks_rect(&self) -> Rect {
        self.tracks_rect
    }

    /// Current hovered clip ID (for bitmap dirty-checking).
    pub fn hovered_clip_id(&self) -> Option<&str> {
        self.hovered_clip_id.as_deref()
    }

    /// Current selection region reference (for bitmap dirty-checking).
    pub fn selection_region_ref(&self) -> Option<&SelectionRegion> {
        self.selection_region.as_ref()
    }

    /// Current insert cursor beat (for bitmap painting).
    pub fn insert_cursor_beat(&self) -> f32 {
        self.insert_cursor_beat
    }

    /// Repaint all dirty layer bitmaps (CPU pixel painting).
    /// Call once per frame before GPU upload.
    pub fn repaint_dirty_layers(
        &mut self,
        state: &crate::bitmap_renderer::BitmapRepaintState,
    ) {
        let (min_beat, max_beat) = self.visible_beat_range();
        let viewport_width_px = self.tracks_rect.width;
        let time_sig = self.beats_per_bar;

        for (i, renderer_opt) in self.bitmap_renderers.iter_mut().enumerate() {
            if let Some(renderer) = renderer_opt {
                let clips = if i < self.clips_by_layer.len() {
                    &self.clips_by_layer[i]
                } else {
                    &[] as &[ViewportClip]
                };
                let is_muted = i < self.tracks.len() && self.tracks[i].is_muted;
                renderer.repaint(
                    clips,
                    min_beat,
                    max_beat,
                    viewport_width_px,
                    is_muted,
                    time_sig,
                    state,
                );
            }
        }
    }

    /// Iterate layer bitmaps that were repainted (for GPU upload).
    /// Yields (layer_index, pixels, tex_w, tex_h) for dirty layers.
    pub fn dirty_layer_iter(&self) -> impl Iterator<Item = (usize, &[crate::node::Color32], usize, usize)> {
        self.bitmap_renderers.iter().enumerate().filter_map(|(i, opt)| {
            opt.as_ref().and_then(|r| {
                if r.was_dirty() && r.tex_w() > 0 && r.tex_h() > 0 {
                    Some((i, r.pixels(), r.tex_w(), r.tex_h()))
                } else {
                    None
                }
            })
        })
    }

    /// Get screen-space rects for each layer bitmap texture (for GPU rendering).
    /// Returns (layer_index, rect) for each active bitmap renderer.
    pub fn layer_bitmap_rects(&self) -> Vec<(usize, Rect)> {
        let mut rects = Vec::new();
        for (i, renderer_opt) in self.bitmap_renderers.iter().enumerate() {
            if renderer_opt.is_some() && i < self.tracks.len() {
                let track_y = self.track_y(i);
                let track_h = self.track_height(i);
                // Only include layers that are visible in the viewport
                if track_y + track_h >= self.tracks_rect.y
                    && track_y < self.tracks_rect.y + self.tracks_rect.height
                    && track_h > 0.0
                {
                    // Clamp to tracks rect bounds
                    let y = track_y.max(self.tracks_rect.y);
                    let y_end = (track_y + track_h).min(self.tracks_rect.y + self.tracks_rect.height);
                    rects.push((i, Rect::new(
                        self.tracks_rect.x,
                        y,
                        self.tracks_rect.width,
                        y_end - y,
                    )));
                }
            }
        }
        rects
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

    pub fn set_selected_clip_ids(&mut self, ids: Vec<ClipId>) {
        self.selected_clip_ids = ids;
    }

    pub fn set_hovered_clip_id(&mut self, id: Option<ClipId>) {
        self.hovered_clip_id = id;
    }

    // ── Accessors ─────────────────────────────────────────────────

    pub fn pixels_per_beat(&self) -> f32 { self.mapper.pixels_per_beat() }
    pub fn scroll_x_beats(&self) -> f32 { self.scroll_x_beats }
    pub fn scroll_y_px(&self) -> f32 { self.scroll_y_px }
    pub fn viewport_rect(&self) -> Rect { self.viewport_rect }
    pub fn ruler_rect(&self) -> Rect { self.ruler_rect }
    pub fn tracks_rect(&self) -> Rect { self.tracks_rect }

    /// Max beat across all clips (for overview strip normalization).
    pub fn max_content_beat(&self) -> f32 {
        self.clips.iter()
            .map(|c| c.start_beat + c.duration_beats)
            .fold(0.0f32, f32::max)
    }

    /// Screen rect for the waveform lane (between ruler and tracks).
    /// Returns ZERO if waveform lane is not visible.
    pub fn waveform_lane_rect(&self) -> Rect {
        let waveform_y = self.ruler_rect.y + self.ruler_rect.height;
        if self.tracks_rect.y > waveform_y + 1.0 {
            // There's space between ruler and tracks — that's the waveform area
            let h = (self.tracks_rect.y - waveform_y).min(color::WAVEFORM_LANE_HEIGHT);
            Rect::new(self.ruler_rect.x, waveform_y, self.ruler_rect.width, h)
        } else {
            Rect::ZERO
        }
    }

    /// Screen rect for the stem lanes (below waveform lane, above tracks).
    pub fn stem_lanes_rect(&self) -> Rect {
        let waveform_y = self.ruler_rect.y + self.ruler_rect.height;
        let stem_y = waveform_y + color::WAVEFORM_LANE_HEIGHT;
        if self.tracks_rect.y > stem_y + 1.0 {
            let h = self.tracks_rect.y - stem_y;
            Rect::new(self.ruler_rect.x, stem_y, self.ruler_rect.width, h)
        } else {
            Rect::ZERO
        }
    }
    pub fn first_node(&self) -> usize { self.first_node }
    pub fn node_count(&self) -> usize { self.node_count }

    /// Read-only access to the flat clip list (for hit testing and rendering).
    pub fn clips(&self) -> &[ViewportClip] { &self.clips }

    /// Whether a layer is a group track (not directly renderable).
    pub fn is_group_layer(&self, layer_index: usize) -> bool {
        self.tracks.get(layer_index).is_some_and(|t| t.is_group)
    }

    // ── Coordinate mapping ────────────────────────────────────────

    /// Convert beat position to pixel X in the tracks area (screen-space).
    pub fn beat_to_pixel(&self, beat: f32) -> f32 {
        (beat - self.scroll_x_beats) * self.mapper.pixels_per_beat() + self.tracks_rect.x
    }

    /// Convert pixel X in the tracks area to beat position.
    pub fn pixel_to_beat(&self, px: f32) -> f32 {
        (px - self.tracks_rect.x) / self.mapper.pixels_per_beat() + self.scroll_x_beats
    }

    /// Convert panel-local pixel X (0 = left edge of tracks area) to beat position.
    /// Used by waveform/stem scrub where events are already offset to local coords.
    pub fn local_pixel_to_beat(&self, local_px: f32) -> f32 {
        local_px / self.mapper.pixels_per_beat() + self.scroll_x_beats
    }

    /// Snap a beat to the grid for ruler scrubbing, unless free-scrub is active.
    ///
    /// Unity `RulerScrubHandler.ScrubToPosition()`:
    /// - Default: snap to nearest grid line via `SnapBeatToGrid(beat, beatsPerBar)`
    /// - Alt/Option held: free scrub (no snap) for sample-accurate positioning
    /// - At max zoom level: auto-disable snapping (can place between grid lines)
    fn scrub_snap_beat(&self, beat: f32, free: bool) -> f32 {
        if free {
            return beat.max(0.0);
        }
        // At max zoom, disable snapping (Unity: ShouldUseFreeScrub, lines 64-66)
        let max_zoom = *color::ZOOM_LEVELS.last().unwrap();
        if self.mapper.pixels_per_beat() >= max_zoom - 0.001 {
            return beat.max(0.0);
        }
        let grid = snap::grid_interval_for_zoom(
            self.mapper.pixels_per_beat(),
            self.beats_per_bar as f32,
        );
        snap::snap_beat_to_grid(beat, grid).max(0.0)
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
    pub fn magnetic_snap(&self, beat: f32, layer_index: usize, ignore_ids: &[ClipId]) -> f32 {
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


    /// Floor-snap a beat to the current grid subdivision.
    /// Unlike `snap_to_grid` (rounds to nearest), this floors to the grid line
    /// at or before the beat. Used for clip creation (Unity: FloorBeatToGrid).
    pub fn floor_to_grid(&self, beat: f32) -> f32 {
        let step = self.grid_step();
        (beat / step).floor() * step
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

    /// Update insert cursor ruler marker position without rebuilding.
    /// Track-area cursor is painted into bitmap.
    fn sync_insert_cursor_ruler(&self, tree: &mut UITree) {
        if self.insert_cursor_ruler_id >= 0 {
            let px = self.beat_to_pixel(self.insert_cursor_beat);
            let in_view = px >= self.tracks_rect.x && px <= self.tracks_rect.x_max();
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

        // Header stack: overview strip + ruler + optional waveform/stem lanes.
        // INVARIANT: MUST use layout.track_header_height() — same source as
        // layer_header.rs uses for panel_origin. If these diverge, layer controls
        // will be vertically misaligned with their tracks.
        let header_h = layout.track_header_height();
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
        self.overview_rect = Rect::new(body.x, body.y, tracks_w, color::OVERVIEW_STRIP_HEIGHT);
        let overview_rect = self.overview_rect;
        // Interactive button so hit_test returns valid ID for click/drag scrubbing.
        // Clip miniatures (non-interactive panels) are added on top but fall through
        // to this button on hit_test. Same pattern as the tracks area overlay.
        self.overview_btn_id = tree.add_button(
            -1, overview_rect.x, overview_rect.y, overview_rect.width, overview_rect.height,
            UIStyle { bg_color: color::OVERVIEW_BG, ..UIStyle::default() }, "",
        ) as i32;

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

        // Grid lines: painted into per-layer bitmap (not UITree nodes)
        // Clips: painted into per-layer bitmap (not UITree nodes)
        // Selection region: painted into per-layer bitmap (not UITree nodes)

        // Build ruler ticks and labels
        self.build_ruler(tree);

        // Export range markers
        self.build_export_markers(tree);

        // Insert cursor ruler marker only (track cursor painted into bitmap)
        self.build_insert_cursor_ruler(tree);

        // Playhead: unified overlay quad in app.rs (no UITree node needed)

        self.node_count = tree.count() - self.first_node;
    }

    fn update(&mut self, tree: &mut UITree) {
        self.sync_insert_cursor_ruler(tree);
    }

    fn handle_event(&mut self, event: &UIEvent, _tree: &UITree) -> Vec<PanelAction> {
        // Tracks-area interaction (click, drag, hover) is handled by
        // InteractionOverlay in app.rs — NOT here. This method only
        // handles ruler events (seek/scrub) which are viewport-specific.
        match event {
            // ── Click: ruler or overview strip ────────────────────
            UIEvent::Click { pos, modifiers, .. } => {
                if self.overview_rect.contains(*pos) {
                    let norm = ((pos.x - self.overview_rect.x) / self.overview_rect.width).clamp(0.0, 1.0);
                    return vec![PanelAction::OverviewScrub(norm)];
                }
                if self.ruler_rect.contains(*pos) {
                    let raw = self.pixel_to_beat(pos.x);
                    let beat = self.scrub_snap_beat(raw, modifiers.alt);
                    return vec![PanelAction::Seek(beat)];
                }
                Vec::new()
            }

            // ── DragBegin: ruler or overview scrub ───────────────
            UIEvent::DragBegin { origin, modifiers, .. } => {
                if self.overview_rect.contains(*origin) {
                    self.drag_mode = ViewportDragMode::OverviewScrub;
                    self.scrub_free = false;
                    let norm = ((origin.x - self.overview_rect.x) / self.overview_rect.width).clamp(0.0, 1.0);
                    return vec![PanelAction::OverviewScrub(norm)];
                }
                if self.ruler_rect.contains(*origin) {
                    self.drag_mode = ViewportDragMode::RulerScrub;
                    // Latch Alt state at drag start — Unity checks per-frame but
                    // Drag events don't carry modifiers, so we capture once.
                    self.scrub_free = modifiers.alt;
                    let raw = self.pixel_to_beat(origin.x);
                    let beat = self.scrub_snap_beat(raw, self.scrub_free);
                    return vec![PanelAction::Seek(beat)];
                }
                Vec::new()
            }

            // ── Drag: ruler or overview scrub continuation ───────
            UIEvent::Drag { pos, .. } => {
                if self.drag_mode == ViewportDragMode::OverviewScrub {
                    let norm = ((pos.x - self.overview_rect.x) / self.overview_rect.width).clamp(0.0, 1.0);
                    return vec![PanelAction::OverviewScrub(norm)];
                }
                if self.drag_mode == ViewportDragMode::RulerScrub {
                    let raw = self.pixel_to_beat(pos.x);
                    let beat = self.scrub_snap_beat(raw, self.scrub_free);
                    return vec![PanelAction::Seek(beat)];
                }
                Vec::new()
            }

            // ── DragEnd: reset drag mode ─────────────────────────
            UIEvent::DragEnd { .. } => {
                if self.drag_mode != ViewportDragMode::None {
                    self.drag_mode = ViewportDragMode::None;
                    self.scrub_free = false;
                }
                Vec::new()
            }

            // All other events (DoubleClick, RightClick, Hover) handled
            // by InteractionOverlay — return empty.
            _ => Vec::new()
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

        // Clip miniatures — cap to avoid thousands of nodes at low zoom.
        // At 1258 clips, uncapped overview creates 1258 panel nodes.
        const MAX_OVERVIEW_CLIPS: usize = 200;
        for (overview_count, clip) in self.clips.iter().enumerate() {
            if overview_count >= MAX_OVERVIEW_CLIPS { break; }
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
            if track.is_group && y >= tr_top
                && let Some(accent) = track.accent_color {
                    tree.add_panel(
                        -1, tr.x, clamped_y,
                        color::GROUP_ACCENT_BAR_WIDTH, clamped_h,
                        UIStyle { bg_color: accent, ..UIStyle::default() },
                    );
                }

            // Collapsed group preview: miniature clip rects of child layers.
            // From Unity ViewportManager.GenerateCollapsedGroupTexture (lines 700-770).
            if track.is_group && track.is_collapsed && !track.child_layer_indices.is_empty() {
                let child_count = track.child_layer_indices.len();
                let rows_per_child = 2.0_f32.min(clamped_h / child_count.max(1) as f32);
                let palette = &color::LAYER_PALETTE;
                let (min_beat, max_beat) = self.visible_beat_range();
                let _ppb = self.mapper.pixels_per_beat();

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

    // build_grid_lines: REMOVED — grid lines are now painted into per-layer bitmaps
    // by LayerBitmapRenderer.paint_grid_lines() (matching Unity exactly).

    fn build_ruler(&mut self, tree: &mut UITree) {
        self.ruler_tick_ids.clear();
        self.ruler_label_ids.clear();

        let (min_beat, max_beat) = self.visible_beat_range();
        let bpb = self.beats_per_bar as f32;
        let ppb = self.mapper.pixels_per_beat();
        let subdiv = self.grid_subdivision();

        // ── Tick step (controls which tick marks appear) ──
        let tick_step = match subdiv {
            GridSubdivision::Bar => bpb,
            GridSubdivision::Beat => 1.0,
            GridSubdivision::Eighth => 0.5,
            GridSubdivision::Sixteenth => 0.25,
        };

        // ── Label step (adaptive — ensures labels never overlap) ──
        // Find the smallest musically-meaningful interval where labels
        // are at least MIN_LABEL_SPACING pixels apart.
        const MIN_LABEL_SPACING: f32 = 50.0;
        let label_step: f32 = if ppb >= MIN_LABEL_SPACING {
            // Enough room for per-beat labels (bar.beat format)
            1.0
        } else if bpb * ppb >= MIN_LABEL_SPACING {
            // Enough room for per-bar labels
            bpb
        } else {
            // Skip bars — double until labels fit
            let bar_px = bpb * ppb;
            let mut n_bars = 2.0_f32;
            while n_bars * bar_px < MIN_LABEL_SPACING && n_bars <= 1024.0 {
                n_bars *= 2.0;
            }
            bpb * n_bars
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
                let is_label_beat = (beat % label_step).abs() < 0.001;

                // Labeled bars get taller ticks for visual anchoring
                let tick_h = if is_label_beat && is_bar {
                    RULER_BAR_TICK_H + 4.0
                } else if is_bar {
                    RULER_BAR_TICK_H
                } else if is_beat {
                    RULER_BEAT_TICK_H
                } else {
                    4.0
                };

                let tick_color = if is_label_beat && is_bar {
                    color::TEXT_NORMAL
                } else if is_bar {
                    color::TEXT_SUBTLE
                } else {
                    color::TEXT_FAINT
                };

                // Tick mark (bottom-aligned)
                let id = tree.add_panel(
                    -1, px, ruler_bottom - tick_h,
                    RULER_TICK_W, tick_h,
                    UIStyle { bg_color: tick_color, ..UIStyle::default() },
                ) as i32;
                self.ruler_tick_ids.push(id);

                // Label (only at label_step intervals to prevent overlap)
                if is_label_beat {
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

    // build_clips: REMOVED — clips are now painted into per-layer bitmaps
    // by LayerBitmapRenderer (matching Unity's LayerBitmapPainter.DrawClip exactly).

    // build_selection_region: REMOVED — selection region is now painted into
    // per-layer bitmaps by LayerBitmapRenderer (matching Unity exactly).

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

    /// Build insert cursor ruler marker only. Track-area cursor is painted
    /// into the per-layer bitmap by LayerBitmapRenderer.
    fn build_insert_cursor_ruler(&mut self, tree: &mut UITree) {
        let px = self.beat_to_pixel(self.insert_cursor_beat);
        let in_view = px >= self.tracks_rect.x && px <= self.tracks_rect.x_max();

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


// ── Tests ────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::UITree;
    use crate::layout::ScreenLayout;
    use crate::input::Modifiers;

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

        // Clips and grid lines are painted into bitmaps, not UITree nodes.
        // Track backgrounds should still exist as UITree nodes.
        assert!(!panel.track_bg_ids.is_empty());
        // Bitmap renderers should be created (one per non-group layer)
        assert_eq!(panel.bitmap_renderers.len(), 2);
        assert!(panel.bitmap_renderers[0].is_some());
        assert!(panel.bitmap_renderers[1].is_some());
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
    fn click_empty_tracks_handled_by_overlay() {
        // Tracks-area clicks are now handled by InteractionOverlay (not viewport).
        // Viewport.handle_event returns empty for tracks clicks.
        let mut tree = UITree::new();
        let mut panel = TimelineViewportPanel::new();
        let layout = test_layout();
        panel.set_tracks(test_tracks());
        panel.build(&mut tree, &layout);

        let tracks_pos = Vec2::new(
            panel.tracks_rect.x + 100.0,
            panel.tracks_rect.y + 50.0,
        );
        let actions = panel.handle_event(
            &UIEvent::Click { node_id: 0, pos: tracks_pos, modifiers: Modifiers::default() },
            &tree,
        );
        assert!(actions.is_empty(), "tracks clicks handled by overlay, not viewport");
    }

    #[test]
    fn shift_click_empty_tracks_handled_by_overlay() {
        // Tracks-area Shift+clicks are now handled by InteractionOverlay.
        let mut tree = UITree::new();
        let mut panel = TimelineViewportPanel::new();
        let layout = test_layout();
        panel.set_tracks(test_tracks());
        panel.build(&mut tree, &layout);

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
        assert!(actions.is_empty(), "tracks clicks handled by overlay, not viewport");
    }

    #[test]
    fn offscreen_clips_bucketed_but_not_rendered_as_nodes() {
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

        // Clips are now painted into bitmaps, not UITree nodes.
        // They should be bucketed by layer.
        assert_eq!(panel.clips_by_layer[0].len(), 1);
    }

    #[test]
    fn clip_color_states() {
        // Clip color testing now uses bitmap_painter::get_clip_color
        use crate::bitmap_painter;

        let normal = bitmap_painter::get_clip_color(false, false, false, false, false);
        let selected = bitmap_painter::get_clip_color(true, false, false, false, false);
        let hovered = bitmap_painter::get_clip_color(false, true, false, false, false);

        assert_eq!(normal, color::CLIP_NORMAL);
        assert_eq!(selected, color::CLIP_SELECTED);
        assert_eq!(hovered, color::CLIP_HOVER);
    }
}
