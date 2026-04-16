use super::{Panel, PanelAction};
use crate::bitmap_painter;
use crate::color;
use crate::coordinate_mapper::CoordinateMapper;
use crate::input::UIEvent;
use crate::layout::ScreenLayout;
use crate::node::*;
use crate::snap;
use crate::tree::UITree;
use manifold_core::marker::TimelineMarker;
use manifold_core::{Beats, ClipId, LayerId, MarkerId};

// ── Layout constants ────────────────────────────────────────────

const RULER_HEIGHT: f32 = color::RULER_HEIGHT;
const CLIP_VERTICAL_PAD: f32 = 12.0;

const RULER_FONT_SIZE: u16 = color::FONT_SMALL;
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
    pub start_beat: Beats,
    pub duration_beats: Beats,
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
    pub start_beat: Beats,
    pub end_beat: Beats,
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

// ── Marker node group for update-in-place ──────────────────────

/// Structured storage for one timeline marker's nodes.
/// Enables update-in-place by providing a 1:1 mapping between markers and their node IDs.
struct MarkerNodeGroup {
    flag_id: i32,
    outline_id: i32, // -1 if not selected
    label_id: i32,   // -1 if no name
}

// ── Track background node group for update-in-place ────────────

/// Structured storage for one track's background nodes.
struct TrackBgGroup {
    bg_id: i32,
    accent_id: i32,    // -1 if no accent bar
    separator_id: i32,
}

// ── Collapsed group bitmap ──────────────────────────────────────

/// CPU pixel buffer for a single collapsed group's clip preview.
struct CollapsedGroupBitmap {
    pixels: Vec<Color32>,
    tex_w: usize,
    tex_h: usize,
    dirty: bool,
    last_min_beat: f32,
    last_max_beat: f32,
    last_viewport_w: f32,
    last_track_h: f32,
    last_clip_count: usize,
}

impl CollapsedGroupBitmap {
    fn new() -> Self {
        Self {
            pixels: Vec::new(),
            tex_w: 0,
            tex_h: 0,
            dirty: true,
            last_min_beat: 0.0,
            last_max_beat: 0.0,
            last_viewport_w: 0.0,
            last_track_h: 0.0,
            last_clip_count: 0,
        }
    }
}

// ── TimelineViewportPanel ───────────────────────────────────────

pub struct TimelineViewportPanel {
    // Shared coordinate mapper (owns zoom, Y-layout, grid snapping).
    // The viewport adds screen-space offset (tracks_rect.x) on top.
    mapper: CoordinateMapper,

    // Viewport-specific scroll state (in beats, not pixels)
    scroll_x_beats: Beats,
    scroll_y_px: f32,
    beats_per_bar: u32,

    // Layer IDs (kept in sync with project layers)
    pub layer_ids: Vec<LayerId>,

    // Track layout (kept in sync with mapper via set_tracks)
    tracks: Vec<TrackInfo>,
    track_y_offsets: Vec<f32>, // cumulative Y offsets from tracks
    total_tracks_height: f32,

    // Clip data — single storage, bucketed by layer index.
    // Access all clips via clips_by_layer.iter().flatten().
    clips_by_layer: Vec<Vec<ViewportClip>>,

    // Per-layer bitmap renderers (None for group layers)
    bitmap_renderers: Vec<Option<crate::bitmap_renderer::LayerBitmapRenderer>>,
    render_scale: f32,

    // Playback state
    playhead_beat: Beats,
    insert_cursor_beat: Beats,
    is_playing: bool,
    selection_region: Option<SelectionRegion>,
    selected_clip_ids: Vec<ClipId>,
    last_selection_version: u64,
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
    viewport_clip_id: i32,
    // playhead: unified overlay quad in app.rs (ruler → waveform → stems → tracks)
    insert_cursor_ruler_id: i32,
    // insert_cursor_track_id: removed — painted into bitmap
    // selection_region_id: removed — painted into bitmap

    // Export range
    export_in_beat: Beats,
    export_out_beat: Beats,
    export_range_enabled: bool,

    // Node IDs — fixed export elements
    export_range_id: i32,
    export_in_marker_id: i32,
    export_out_marker_id: i32,

    // Node IDs — dynamic elements (rebuilt on scroll/zoom)
    ruler_tick_ids: Vec<i32>,
    ruler_label_ids: Vec<i32>,
    // grid_line_ids: removed — grid painted into bitmap
    track_bg_ids: Vec<i32>,
    track_bg_groups: Vec<TrackBgGroup>,
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

    // Overview bitmap — CPU pixel buffer for the full-timeline minimap.
    // Replaces per-clip panel nodes with a single texture (no clip cap).
    // Two-layer design: cached clip layer + lightweight overlay compositing.
    overview_pixels: Vec<Color32>,
    /// Cached clip-only layer — only repainted when clip data changes.
    /// On scroll/playhead changes, this is copied and the overlay is composited on top.
    overview_clip_pixels: Vec<Color32>,
    overview_tex_w: usize,
    overview_tex_h: usize,
    overview_dirty: bool,
    /// True when clip data changed and clip layer needs full repaint.
    overview_clips_dirty: bool,
    overview_last_clip_fingerprint: u64,
    overview_last_playhead: f32,
    overview_last_scroll_x: f32,
    overview_last_ppb: f32,
    overview_last_track_count: usize,
    overview_last_width: f32,

    // Collapsed group bitmaps — one per group track (None for non-groups).
    // Replaces per-clip panel nodes with per-group textures.
    collapsed_group_bitmaps: Vec<Option<CollapsedGroupBitmap>>,

    // Timeline markers
    marker_groups: Vec<MarkerNodeGroup>,
    markers: Vec<TimelineMarker>,
    marker_line_cache: Vec<(f32, Color32)>,
    selected_marker_ids: Vec<MarkerId>,
    marker_flag_rects: Vec<(MarkerId, Rect)>,
    marker_node_ids: Vec<i32>,
    marker_drag_id: Option<MarkerId>,
    marker_drag_start_beat: Beats,
}

/// Viewport-local drag mode. Only tracks ruler scrub — all clip interaction
/// (move, trim, region) is handled by InteractionOverlay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ViewportDragMode {
    None,
    RulerScrub,
    OverviewScrub,
    MarkerDrag,
}

impl TimelineViewportPanel {
    pub fn new() -> Self {
        Self {
            mapper: CoordinateMapper::new(),
            scroll_x_beats: Beats::ZERO,
            scroll_y_px: 0.0,
            beats_per_bar: 4,
            layer_ids: Vec::new(),
            tracks: Vec::new(),
            track_y_offsets: Vec::new(),
            total_tracks_height: 0.0,
            clips_by_layer: Vec::new(),
            bitmap_renderers: Vec::new(),
            render_scale: 2.0, // default HiDPI (macOS Retina)
            playhead_beat: Beats::ZERO,
            insert_cursor_beat: Beats::ZERO,
            is_playing: false,
            selection_region: None,
            selected_clip_ids: Vec::new(),
            last_selection_version: 0,
            hovered_clip_id: None,
            viewport_rect: Rect::ZERO,
            overview_rect: Rect::ZERO,
            ruler_rect: Rect::ZERO,
            tracks_rect: Rect::ZERO,
            bg_panel_id: -1,
            overview_btn_id: -1,
            ruler_bg_id: -1,
            viewport_clip_id: -1,
            insert_cursor_ruler_id: -1,
            export_in_beat: Beats::ZERO,
            export_out_beat: Beats::ZERO,
            export_range_enabled: false,
            export_range_id: -1,
            export_in_marker_id: -1,
            export_out_marker_id: -1,
            ruler_tick_ids: Vec::new(),
            ruler_label_ids: Vec::new(),
            track_bg_ids: Vec::new(),
            track_bg_groups: Vec::new(),
            first_node: 0,
            node_count: 0,
            drag_mode: ViewportDragMode::None,
            scrub_free: false,
            cached_fingerprint: 0,
            overview_pixels: Vec::new(),
            overview_clip_pixels: Vec::new(),
            overview_tex_w: 0,
            overview_tex_h: 0,
            overview_dirty: true,
            overview_clips_dirty: true,
            overview_last_clip_fingerprint: 0,
            overview_last_playhead: 0.0,
            overview_last_scroll_x: 0.0,
            overview_last_ppb: 0.0,
            overview_last_track_count: 0,
            overview_last_width: 0.0,
            collapsed_group_bitmaps: Vec::new(),
            marker_groups: Vec::new(),
            markers: Vec::new(),
            marker_line_cache: Vec::new(),
            selected_marker_ids: Vec::new(),
            marker_flag_rects: Vec::new(),
            marker_node_ids: Vec::new(),
            marker_drag_id: None,
            marker_drag_start_beat: Beats::ZERO,
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
        let mut hash = self.total_clip_count() as u64;
        hash = hash
            .wrapping_mul(31)
            .wrapping_add(self.mapper.pixels_per_beat().to_bits() as u64);
        hash = hash
            .wrapping_mul(31)
            .wrapping_add(self.scroll_x_beats.as_f32().to_bits() as u64);
        hash = hash
            .wrapping_mul(31)
            .wrapping_add(self.scroll_y_px.to_bits() as u64);
        hash = hash.wrapping_mul(31).wrapping_add(self.tracks.len() as u64);
        hash = hash
            .wrapping_mul(31)
            .wrapping_add(self.selected_clip_ids.len() as u64);
        hash = hash.wrapping_mul(31).wrapping_add(
            self.hovered_clip_id
                .as_ref()
                .map(|s| {
                    let mut h = 0u64;
                    for b in s.bytes() {
                        h = h.wrapping_mul(31).wrapping_add(b as u64);
                    }
                    h
                })
                .unwrap_or(0),
        );
        // Per-visible-clip fingerprint
        for clip in self.clips_by_layer.iter().flatten() {
            let clip_end = (clip.start_beat + clip.duration_beats).as_f32();
            if clip_end <= min_beat || clip.start_beat.as_f32() >= max_beat {
                continue;
            }
            hash = hash
                .wrapping_mul(31)
                .wrapping_add(clip.start_beat.as_f32().to_bits() as u64);
            hash = hash
                .wrapping_mul(31)
                .wrapping_add(clip_end.to_bits() as u64);
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
        // Track layout changed — overview clip layer needs full repaint
        self.overview_clips_dirty = true;
        // Recompute cumulative Y offsets from track heights
        self.track_y_offsets.clear();
        let mut y = 0.0;
        for track in &self.tracks {
            self.track_y_offsets.push(y);
            y += track.height;
        }
        self.total_tracks_height = y;

        // Create/resize bitmap renderers (one per visible layer, including groups)
        self.bitmap_renderers.clear();
        for (i, track) in self.tracks.iter().enumerate() {
            if track.height <= 0.0 {
                self.bitmap_renderers.push(None);
            } else {
                self.bitmap_renderers
                    .push(Some(crate::bitmap_renderer::LayerBitmapRenderer::new(
                        i,
                        self.render_scale,
                        track.height,
                        CLIP_VERTICAL_PAD,
                    )));
            }
        }

        // Collapsed group bitmap slots (one per track, None for non-groups)
        self.collapsed_group_bitmaps.clear();
        for track in &self.tracks {
            if track.is_group && track.is_collapsed && !track.child_layer_indices.is_empty() {
                self.collapsed_group_bitmaps.push(Some(CollapsedGroupBitmap::new()));
            } else {
                self.collapsed_group_bitmaps.push(None);
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
        // Compute clip fingerprint for overview dirty-checking (O(N) but only on clip change)
        let mut clip_fp: u64 = clips.len() as u64;
        for c in &clips {
            clip_fp = clip_fp
                .wrapping_mul(31)
                .wrapping_add(c.start_beat.as_f32().to_bits() as u64);
            clip_fp = clip_fp
                .wrapping_mul(31)
                .wrapping_add(c.duration_beats.as_f32().to_bits() as u64);
            clip_fp = clip_fp
                .wrapping_mul(31)
                .wrapping_add(c.layer_index as u64);
        }
        if clip_fp != self.overview_last_clip_fingerprint {
            self.overview_clips_dirty = true;
        }
        self.overview_last_clip_fingerprint = clip_fp;
        self.overview_dirty = true;

        // Bucket clips by layer — clear inner vecs (preserving capacity)
        // instead of dropping and reallocating them.
        for v in &mut self.clips_by_layer {
            v.clear();
        }
        // Match track count: resize_with grows OR truncates as needed.
        self.clips_by_layer.resize_with(self.tracks.len(), Vec::new);
        for clip in clips {
            if clip.layer_index < self.clips_by_layer.len() {
                self.clips_by_layer[clip.layer_index].push(clip);
            }
        }
    }

    /// Total number of clips across all layers.
    fn total_clip_count(&self) -> usize {
        self.clips_by_layer.iter().map(|v| v.len()).sum()
    }

    /// Force a specific layer's bitmap to repaint on the next frame.
    /// Used for per-layer invalidation from editing operations.
    pub fn invalidate_layer_bitmap(&mut self, layer_index: usize) {
        if let Some(Some(r)) = self.bitmap_renderers.get_mut(layer_index) {
            r.invalidate();
        }
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
    pub fn insert_cursor_beat(&self) -> Beats {
        self.insert_cursor_beat
    }

    /// Repaint all dirty layer bitmaps (CPU pixel painting).
    /// Call once per frame before GPU upload.
    pub fn repaint_dirty_layers(&mut self, state: &crate::bitmap_renderer::BitmapRepaintState) {
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
    pub fn dirty_layer_iter(
        &self,
    ) -> impl Iterator<Item = (usize, &[crate::node::Color32], usize, usize)> {
        self.bitmap_renderers
            .iter()
            .enumerate()
            .filter_map(|(i, opt)| {
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
                    let y_end =
                        (track_y + track_h).min(self.tracks_rect.y + self.tracks_rect.height);
                    rects.push((
                        i,
                        Rect::new(self.tracks_rect.x, y, self.tracks_rect.width, y_end - y),
                    ));
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

        let changed = (new_x - self.scroll_x_beats.as_f32()).abs() > 0.001
            || (new_y - self.scroll_y_px).abs() > 0.001;
        self.scroll_x_beats = Beats(new_x as f64);
        self.scroll_y_px = new_y;
        changed
    }

    pub fn set_beats_per_bar(&mut self, bpb: u32) {
        self.beats_per_bar = bpb.max(1);
    }

    pub fn set_playhead(&mut self, beat: Beats) {
        self.playhead_beat = beat;
    }

    pub fn set_insert_cursor(&mut self, beat: Beats) {
        self.insert_cursor_beat = beat;
    }

    pub fn set_export_range(&mut self, in_beat: Beats, out_beat: Beats, enabled: bool) {
        self.export_in_beat = in_beat;
        self.export_out_beat = out_beat;
        self.export_range_enabled = enabled;
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

    /// Update selection sets only when selection_version has changed.
    /// Avoids per-frame Vec allocation when selection is stable.
    pub fn sync_selection(
        &mut self,
        version: u64,
        clip_ids: impl FnOnce() -> Vec<ClipId>,
        marker_ids: impl FnOnce() -> Vec<MarkerId>,
    ) {
        if version != self.last_selection_version {
            self.last_selection_version = version;
            self.selected_clip_ids = clip_ids();
            self.selected_marker_ids = marker_ids();
        }
    }

    pub fn set_hovered_clip_id(&mut self, id: Option<ClipId>) {
        self.hovered_clip_id = id;
    }

    pub fn set_markers(&mut self, markers: Vec<TimelineMarker>) {
        self.marker_line_cache = markers
            .iter()
            .map(|m| {
                let mc = color::marker_color_to_color32(m.color);
                let line_color = Color32::new(mc.r, mc.g, mc.b, color::MARKER_LINE_ALPHA);
                (m.beat.as_f32(), line_color)
            })
            .collect();
        self.markers = markers;
    }

    /// Check if the provided markers differ from the cached set.
    /// Uses length + first/last beat as a fast proxy to avoid full comparison.
    pub fn markers_stale(&self, source: &[TimelineMarker]) -> bool {
        if self.markers.len() != source.len() {
            return true;
        }
        if let (Some(a), Some(b)) = (self.markers.last(), source.last())
            && (a.beat != b.beat || a.id != b.id)
        {
            return true;
        }
        false
    }

    /// Marker positions and colors for bitmap rendering (beat, color with line alpha).
    /// Cached — rebuilt when markers change via set_markers(). Returns owned Vec
    /// (from cache) so the caller can use it without borrowing the viewport.
    pub fn marker_line_data(&self) -> Vec<(f32, Color32)> {
        self.marker_line_cache.clone()
    }

    pub fn set_selected_marker_ids(&mut self, ids: Vec<MarkerId>) {
        self.selected_marker_ids = ids;
    }

    /// Hit-test a point against marker flags in the ruler area.
    /// Returns the MarkerId if a flag was hit.
    pub fn hit_test_marker_flag(&self, pos: Vec2) -> Option<MarkerId> {
        for (id, rect) in &self.marker_flag_rects {
            if rect.contains(pos) {
                return Some(id.clone());
            }
        }
        None
    }

    // ── Accessors ─────────────────────────────────────────────────

    pub fn pixels_per_beat(&self) -> f32 {
        self.mapper.pixels_per_beat()
    }
    pub fn scroll_x_beats(&self) -> Beats {
        self.scroll_x_beats
    }
    pub fn scroll_y_px(&self) -> f32 {
        self.scroll_y_px
    }
    pub fn viewport_rect(&self) -> Rect {
        self.viewport_rect
    }
    pub fn ruler_rect(&self) -> Rect {
        self.ruler_rect
    }
    pub fn tracks_rect(&self) -> Rect {
        self.tracks_rect
    }

    /// Max beat across all clips (for overview strip normalization).
    pub fn max_content_beat(&self) -> f32 {
        self.clips_by_layer
            .iter()
            .flatten()
            .map(|c| c.start_beat.as_f32() + c.duration_beats.as_f32())
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
    pub fn first_node(&self) -> usize {
        self.first_node
    }
    pub fn node_count(&self) -> usize {
        self.node_count
    }

    /// Read-only access to clips for a specific layer.
    pub fn clips_for_layer(&self, layer_index: usize) -> &[ViewportClip] {
        self.clips_by_layer
            .get(layer_index)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    /// Total layer count (for iterating all clips across layers).
    pub fn layer_count(&self) -> usize {
        self.clips_by_layer.len()
    }

    /// Whether a layer is a group track (not directly renderable).
    pub fn is_group_layer(&self, layer_index: usize) -> bool {
        self.tracks.get(layer_index).is_some_and(|t| t.is_group)
    }

    // ── Coordinate mapping ────────────────────────────────────────

    /// Convert beat position to pixel X in the tracks area (screen-space).
    pub fn beat_to_pixel(&self, beat: Beats) -> f32 {
        (beat.as_f32() - self.scroll_x_beats.as_f32()) * self.mapper.pixels_per_beat()
            + self.tracks_rect.x
    }

    /// Convert pixel X in the tracks area to beat position.
    pub fn pixel_to_beat(&self, px: f32) -> Beats {
        Beats(
            ((px - self.tracks_rect.x) / self.mapper.pixels_per_beat()) as f64
                + self.scroll_x_beats.0,
        )
    }

    /// Convert panel-local pixel X (0 = left edge of tracks area) to beat position.
    /// Used by waveform/stem scrub where events are already offset to local coords.
    pub fn local_pixel_to_beat(&self, local_px: f32) -> Beats {
        Beats((local_px / self.mapper.pixels_per_beat()) as f64 + self.scroll_x_beats.0)
    }

    /// Snap a beat to the grid for ruler scrubbing, unless free-scrub is active.
    ///
    /// Unity `RulerScrubHandler.ScrubToPosition()`:
    /// - Default: snap to nearest grid line via `SnapBeatToGrid(beat, beatsPerBar)`
    /// - Alt/Option held: free scrub (no snap) for sample-accurate positioning
    /// - At max zoom level: auto-disable snapping (can place between grid lines)
    fn scrub_snap_beat(&self, beat: Beats, free: bool) -> Beats {
        if free {
            return beat.max(Beats::ZERO);
        }
        // At max zoom, disable snapping (Unity: ShouldUseFreeScrub, lines 64-66)
        let max_zoom = *color::ZOOM_LEVELS.last().unwrap();
        if self.mapper.pixels_per_beat() >= max_zoom - 0.001 {
            return beat.max(Beats::ZERO);
        }
        let grid =
            snap::grid_interval_for_zoom(self.mapper.pixels_per_beat(), self.beats_per_bar as f32);
        snap::snap_beat_to_grid(beat, Beats::from_f32(grid)).max(Beats::ZERO)
    }

    /// Convert beat duration to pixel width.
    pub fn beat_duration_to_width(&self, beats: f32) -> f32 {
        self.mapper.beat_duration_to_width(Beats::from_f32(beats))
    }

    /// Get Y position of a track (relative to tracks_rect top, before scroll).
    pub fn track_y(&self, layer_index: usize) -> f32 {
        self.track_y_offsets
            .get(layer_index)
            .copied()
            .unwrap_or(0.0)
            + self.tracks_rect.y
            - self.scroll_y_px
    }

    /// Get height of a track.
    pub fn track_height(&self, layer_index: usize) -> f32 {
        self.tracks
            .get(layer_index)
            .map(|t| t.height)
            .unwrap_or(color::TRACK_HEIGHT)
    }

    /// Visible beat range (with buffer).
    fn visible_beat_range(&self) -> (f32, f32) {
        let min_beat = self.scroll_x_beats.as_f32();
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
        let beat = self.pixel_to_beat(pos.x).as_f32();

        // Reject clicks in vertical padding — only the padded clip rect is interactive.
        let track_y = self.track_y(layer_index);
        let track_h = self.track_height(layer_index);
        let clip_top = track_y + CLIP_VERTICAL_PAD;
        let clip_bottom = track_y + track_h - CLIP_VERTICAL_PAD;
        if pos.y < clip_top || pos.y > clip_bottom {
            return None;
        }

        // Iterate clips on this layer in reverse order (topmost/last wins)
        let layer_clips = self.clips_by_layer.get(layer_index)
            .map(|v| v.as_slice())
            .unwrap_or(&[]);
        for clip in layer_clips.iter().rev() {

            let clip_start_f32 = clip.start_beat.as_f32();
            let clip_end = clip_start_f32 + clip.duration_beats.as_f32();
            if beat < clip_start_f32 || beat >= clip_end {
                continue;
            }

            let clip_width_px = clip.duration_beats.as_f32() * self.mapper.pixels_per_beat();
            let local_px = (beat - clip_start_f32) * self.mapper.pixels_per_beat();

            let region = if clip_width_px > color::TRIM_HANDLE_MIN_CLIP_WIDTH_PX
                && local_px < color::TRIM_HANDLE_THRESHOLD_PX
            {
                HitRegion::TrimLeft
            } else if clip_width_px > color::TRIM_HANDLE_MIN_CLIP_WIDTH_PX
                && local_px > clip_width_px - color::TRIM_HANDLE_THRESHOLD_PX
            {
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

    /// Snap a beat position to the snap grid (matches prominently visible grid lines).
    pub fn snap_to_grid(&self, beat: Beats) -> Beats {
        let step = self.snap_grid_step() as f64;
        Beats((beat.0 / step).round() * step)
    }

    /// Magnetic snap: snap to grid lines AND neighboring clip edges within threshold.
    ///
    /// Grid snap uses a threshold of at least half the grid interval, ensuring clips
    /// always jump between grid positions (standard DAW behavior). Clip edge snap
    /// uses the pixel-based threshold for fine-grained magnetic pull.
    /// `ignore_ids` are clip IDs being dragged (don't snap to self).
    pub fn magnetic_snap(&self, beat: Beats, layer_index: usize, ignore_ids: &[ClipId]) -> Beats {
        use crate::snap::SNAP_THRESHOLD_PX;

        let ppb = self.mapper.pixels_per_beat() as f64;

        // Pixel-based threshold (for clip edge snapping)
        let pixel_threshold_beats = if ppb > 0.0 {
            SNAP_THRESHOLD_PX as f64 / ppb
        } else {
            0.0
        };

        // Grid threshold: half the grid interval so every position snaps to the
        // nearest visible grid line (full cell coverage, standard DAW behavior).
        let half_grid = self.snap_grid_step() as f64 / 2.0;
        let grid_threshold = pixel_threshold_beats.max(half_grid);

        // Start with raw beat — only snap if a candidate is within threshold.
        let mut best_beat = beat;
        let mut best_dist = f64::MAX;

        // Grid candidate (uses wider threshold for full-coverage grid snap)
        let grid_snapped = self.snap_to_grid(beat);
        let grid_dist = (grid_snapped.0 - beat.0).abs();
        if grid_dist <= grid_threshold && grid_dist < best_dist {
            best_dist = grid_dist;
            best_beat = grid_snapped;
        }

        // Neighboring clip edges on the same layer (uses pixel-based threshold).
        // Clip edges that are closer than the grid snap win — this lets you
        // align clip boundaries precisely even between grid lines.
        let layer_clips = self.clips_by_layer.get(layer_index)
            .map(|v| v.as_slice())
            .unwrap_or(&[]);
        for clip in layer_clips {
            if ignore_ids.contains(&clip.clip_id) {
                continue;
            }

            // Check start edge
            let dist_start = (clip.start_beat.0 - beat.0).abs();
            if dist_start < grid_threshold && dist_start < best_dist {
                best_dist = dist_start;
                best_beat = clip.start_beat;
            }

            // Check end edge
            let end_beat = Beats(clip.start_beat.0 + clip.duration_beats.0);
            let dist_end = (end_beat.0 - beat.0).abs();
            if dist_end < grid_threshold && dist_end < best_dist {
                best_dist = dist_end;
                best_beat = end_beat;
            }
        }

        best_beat
    }

    /// Floor-snap a beat to the snap grid subdivision.
    /// Unlike `snap_to_grid` (rounds to nearest), this floors to the grid line
    /// at or before the beat. Used for clip creation (Unity: FloorBeatToGrid).
    pub fn floor_to_grid(&self, beat: Beats) -> Beats {
        let step = self.snap_grid_step() as f64;
        Beats((beat.0 / step).floor() * step)
    }

    /// Current visual grid step size in beats (for rendering: ruler ticks, etc.).
    pub fn grid_step(&self) -> f32 {
        match self.grid_subdivision() {
            GridSubdivision::Bar => self.beats_per_bar as f32,
            GridSubdivision::Beat => 1.0,
            GridSubdivision::Eighth => 0.5,
            GridSubdivision::Sixteenth => 0.25,
        }
    }

    /// Snap grid step — matches the visible grid lines so snapping targets
    /// exactly what the user sees. Delegates to `grid_step()`.
    pub fn snap_grid_step(&self) -> f32 {
        self.grid_step()
    }

    /// Grid-aligned step for clip creation, guaranteed to produce a clip
    /// at least `MIN_CREATION_PX` wide on screen. Walks up musical grid
    /// levels (16th → 8th → beat → bar) until the threshold is met.
    const MIN_CREATION_PX: f32 = 40.0;

    pub fn clip_creation_step(&self) -> Beats {
        let ppb = self.mapper.pixels_per_beat();
        let candidates: [f32; 4] = [0.25, 0.5, 1.0, self.beats_per_bar as f32];
        let grid = self.snap_grid_step();
        for &step in &candidates {
            if step >= grid && step * ppb >= Self::MIN_CREATION_PX {
                return Beats(step as f64);
            }
        }
        // Fallback: bar is always the coarsest grid level
        Beats(self.beats_per_bar as f64)
    }

    /// At extreme zoom-out, bar lines are too dense. Returns the number of
    /// bars to skip between visible bar lines (1 = show every bar).
    fn bar_skip(&self) -> u32 {
        let bar_px = self.mapper.pixels_per_beat() * self.beats_per_bar as f32;
        if bar_px >= 8.0 {
            1
        } else if bar_px >= 4.0 {
            2
        } else if bar_px >= 2.0 {
            4
        } else {
            8
        }
    }

    // ── Grid subdivision ──────────────────────────────────────────

    /// Determine visual grid subdivision level based on zoom.
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
                    Rect::new(
                        px - marker_s * 0.5,
                        self.ruler_rect.y + self.ruler_rect.height - marker_s,
                        marker_s,
                        marker_s,
                    ),
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
        self.ruler_rect = Rect::new(
            body.x,
            body.y + color::OVERVIEW_STRIP_HEIGHT,
            tracks_w,
            RULER_HEIGHT,
        );
        self.tracks_rect = Rect::new(
            body.x,
            body.y + header_h,
            tracks_w,
            (body.height - header_h).max(0.0),
        );

        // Background
        self.bg_panel_id = tree.add_panel(
            -1,
            self.viewport_rect.x,
            self.viewport_rect.y,
            self.viewport_rect.width,
            self.viewport_rect.height,
            UIStyle {
                bg_color: color::DARK_BG,
                ..UIStyle::default()
            },
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
            -1,
            overview_rect.x,
            overview_rect.y,
            overview_rect.width,
            overview_rect.height,
            UIStyle {
                bg_color: color::OVERVIEW_BG,
                ..UIStyle::default()
            },
            "",
        ) as i32;

        // Overview strip bitmap — repainted in repaint_overview() each frame,
        // uploaded and rendered via the layer bitmap GPU path (index 1002).
        self.overview_dirty = true;

        // Ruler background — INTERACTIVE so clicks register for playhead scrubbing
        self.ruler_bg_id = tree.add_button(
            -1,
            self.ruler_rect.x,
            self.ruler_rect.y,
            self.ruler_rect.width,
            self.ruler_rect.height,
            UIStyle {
                bg_color: color::HEADER_BG,
                ..UIStyle::default()
            },
            "",
        ) as i32;

        // Clip region covering ruler + tracks — prevents ticks, labels, markers,
        // and export range elements from bleeding past the viewport bounds into
        // adjacent panels (e.g. live recording section to the right).
        self.viewport_clip_id = tree.add_node(
            -1,
            self.viewport_rect,
            UINodeType::ClipRegion,
            UIStyle::default(),
            None,
            UIFlags::CLIPS_CHILDREN,
        ) as i32;

        // Interactive overlay covering entire tracks area — catches all clicks/drags
        // (matches Unity's InteractionOverlay which is a transparent MonoBehaviour
        // covering the tracks viewport). Without this, clicks on non-interactive
        // panel nodes (track backgrounds, grid lines) won't generate events.
        tree.add_button(
            -1,
            self.tracks_rect.x,
            self.tracks_rect.y,
            self.tracks_rect.width,
            self.tracks_rect.height,
            UIStyle {
                bg_color: Color32::TRANSPARENT,
                ..UIStyle::default()
            },
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

        // Timeline markers (user-placed)
        self.build_markers(tree);

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
            // ── Click: marker flag → ruler → overview strip ───────
            UIEvent::Click { pos, modifiers, .. } => {
                // Marker flag hit-test (priority over ruler scrub)
                if let Some(marker_id) = self.hit_test_marker_flag(*pos) {
                    return vec![PanelAction::MarkerClicked(
                        marker_id.to_string(),
                        *modifiers,
                    )];
                }
                if self.overview_rect.contains(*pos) {
                    let norm =
                        ((pos.x - self.overview_rect.x) / self.overview_rect.width).clamp(0.0, 1.0);
                    return vec![PanelAction::OverviewScrub(norm)];
                }
                if self.ruler_rect.contains(*pos) {
                    let raw = self.pixel_to_beat(pos.x);
                    let beat = self.scrub_snap_beat(raw, modifiers.alt);
                    return vec![PanelAction::Seek(beat.as_f32())];
                }
                Vec::new()
            }

            // ── DragBegin: marker flag → ruler → overview scrub ──
            UIEvent::DragBegin {
                origin, modifiers, ..
            } => {
                // Marker flag drag (priority over ruler scrub)
                if let Some(marker_id) = self.hit_test_marker_flag(*origin) {
                    self.drag_mode = ViewportDragMode::MarkerDrag;
                    // Store start beat for undo
                    self.marker_drag_start_beat = self
                        .markers
                        .iter()
                        .find(|m| m.id == marker_id)
                        .map(|m| m.beat)
                        .unwrap_or(Beats::ZERO);
                    self.marker_drag_id = Some(marker_id.clone());
                    return vec![PanelAction::MarkerDragStarted(marker_id.to_string())];
                }
                if self.overview_rect.contains(*origin) {
                    self.drag_mode = ViewportDragMode::OverviewScrub;
                    self.scrub_free = false;
                    let norm = ((origin.x - self.overview_rect.x) / self.overview_rect.width)
                        .clamp(0.0, 1.0);
                    return vec![PanelAction::OverviewScrub(norm)];
                }
                if self.ruler_rect.contains(*origin) {
                    self.drag_mode = ViewportDragMode::RulerScrub;
                    // Latch Alt state at drag start — Unity checks per-frame but
                    // Drag events don't carry modifiers, so we capture once.
                    self.scrub_free = modifiers.alt;
                    let raw = self.pixel_to_beat(origin.x);
                    let beat = self.scrub_snap_beat(raw, self.scrub_free);
                    return vec![PanelAction::Seek(beat.as_f32())];
                }
                Vec::new()
            }

            // ── Drag: marker → ruler → overview scrub continuation
            UIEvent::Drag { pos, .. } => {
                if self.drag_mode == ViewportDragMode::MarkerDrag
                    && let Some(marker_id) = &self.marker_drag_id
                {
                    let raw = self.pixel_to_beat(pos.x);
                    let beat = self.scrub_snap_beat(raw, false).max(Beats::ZERO);
                    return vec![PanelAction::MarkerDragMoved(
                        marker_id.to_string(),
                        beat.as_f32(),
                    )];
                }
                if self.drag_mode == ViewportDragMode::OverviewScrub {
                    let norm =
                        ((pos.x - self.overview_rect.x) / self.overview_rect.width).clamp(0.0, 1.0);
                    return vec![PanelAction::OverviewScrub(norm)];
                }
                if self.drag_mode == ViewportDragMode::RulerScrub {
                    // Clamp pixel to ruler rect so dragging outside the viewport
                    // doesn't seek to extreme positions.
                    let clamped_x =
                        pos.x.clamp(self.ruler_rect.x, self.ruler_rect.x + self.ruler_rect.width);
                    let raw = self.pixel_to_beat(clamped_x);
                    let beat = self.scrub_snap_beat(raw, self.scrub_free);
                    return vec![PanelAction::Seek(beat.as_f32())];
                }
                Vec::new()
            }

            // ── DragEnd: reset drag mode ─────────────────────────
            UIEvent::DragEnd { pos, .. } => {
                if self.drag_mode == ViewportDragMode::MarkerDrag {
                    let result = if let Some(marker_id) = self.marker_drag_id.take() {
                        let raw = self.pixel_to_beat(pos.x);
                        let beat = self.scrub_snap_beat(raw, false).max(Beats::ZERO);
                        vec![PanelAction::MarkerDragEnded(
                            marker_id.to_string(),
                            beat.as_f32(),
                        )]
                    } else {
                        Vec::new()
                    };
                    self.drag_mode = ViewportDragMode::None;
                    return result;
                }
                if self.drag_mode != ViewportDragMode::None {
                    self.drag_mode = ViewportDragMode::None;
                    self.scrub_free = false;
                }
                Vec::new()
            }

            // ── DoubleClick: marker rename ────────────────────────
            UIEvent::DoubleClick { pos, .. } => {
                if let Some(marker_id) = self.hit_test_marker_flag(*pos) {
                    return vec![PanelAction::MarkerDoubleClicked(marker_id.to_string())];
                }
                Vec::new()
            }

            // ── RightClick: marker context menu ──────────────────
            UIEvent::RightClick { pos, .. } => {
                if let Some(marker_id) = self.hit_test_marker_flag(*pos) {
                    return vec![PanelAction::MarkerRightClicked(marker_id.to_string())];
                }
                Vec::new()
            }

            // All other events handled by InteractionOverlay — return empty.
            _ => Vec::new(),
        }
    }

    fn first_node(&self) -> usize {
        self.first_node
    }
    fn node_count(&self) -> usize {
        self.node_count
    }
}

// ── Build helpers (private) ──────────────────────────────────────

impl TimelineViewportPanel {
    /// Build clip miniatures in the overview strip.
    /// From Unity OverviewStripPanel.BuildPanel (lines 218-270).
    /// Renders small colored rects for each clip, a viewport indicator,
    /// and the playhead position.
    /// Repaint the overview strip bitmap. Call once per frame before GPU upload.
    /// Paints ALL clips (no cap) into a small CPU pixel buffer, then overlays
    /// the viewport indicator and playhead. Group layers are excluded.
    pub fn repaint_overview(&mut self) {
        let scale = self.render_scale;
        let tex_w = (self.overview_rect.width * scale).round().max(1.0) as usize;
        let tex_h = (self.overview_rect.height * scale).round().max(1.0) as usize;

        // Dirty-checking: skip if nothing changed at all.
        let ppb = self.mapper.pixels_per_beat();
        let scroll_x = self.scroll_x_beats.as_f32();
        let playhead = self.playhead_beat.as_f32();
        if !self.overview_dirty
            && !self.overview_clips_dirty
            && self.overview_last_playhead == playhead
            && self.overview_last_scroll_x == scroll_x
            && self.overview_last_ppb == ppb
            && self.overview_last_track_count == self.tracks.len()
            && self.overview_last_width == self.overview_rect.width
        {
            return;
        }

        // Check if clip layer needs repaint (expensive) vs overlay-only (cheap).
        let size_changed = tex_w != self.overview_tex_w || tex_h != self.overview_tex_h;
        let clips_need_repaint = self.overview_clips_dirty
            || size_changed
            || self.overview_last_track_count != self.tracks.len();

        self.overview_last_playhead = playhead;
        self.overview_last_scroll_x = scroll_x;
        self.overview_last_ppb = ppb;
        self.overview_last_track_count = self.tracks.len();
        self.overview_last_width = self.overview_rect.width;
        self.overview_tex_w = tex_w;
        self.overview_tex_h = tex_h;

        let total = tex_w * tex_h;

        if self.total_clip_count() == 0 || self.tracks.is_empty() {
            self.overview_pixels.resize(total, Color32::TRANSPARENT);
            self.overview_pixels.fill(Color32::TRANSPARENT);
            self.overview_dirty = true;
            return;
        }

        // Content duration for normalization
        let mut max_beat = 0.0f32;
        for clip in self.clips_by_layer.iter().flatten() {
            let end = clip.start_beat.as_f32() + clip.duration_beats.as_f32();
            if end > max_beat {
                max_beat = end;
            }
        }
        if max_beat <= 0.0 {
            self.overview_pixels.resize(total, Color32::TRANSPARENT);
            self.overview_pixels.fill(Color32::TRANSPARENT);
            self.overview_dirty = true;
            return;
        }

        // ── Layer 1: Clip layer (cached, only repainted on clip data change) ──
        if clips_need_repaint {
            self.overview_clip_pixels.resize(total, Color32::TRANSPARENT);
            self.overview_clip_pixels.fill(Color32::TRANSPARENT);

            // Remap: skip group layers
            let mut non_group_row: Vec<Option<usize>> =
                Vec::with_capacity(self.tracks.len());
            let mut non_group_count: usize = 0;
            for track in &self.tracks {
                if track.is_group {
                    non_group_row.push(None);
                } else {
                    non_group_row.push(Some(non_group_count));
                    non_group_count += 1;
                }
            }

            if non_group_count > 0 {
                let row_h = tex_h as f32 / non_group_count as f32;

                for clip in self.clips_by_layer.iter().flatten() {
                    let row = match non_group_row.get(clip.layer_index).copied().flatten() {
                        Some(r) => r,
                        None => continue,
                    };
                    let start_norm = clip.start_beat.as_f32() / max_beat;
                    let end_norm =
                        (clip.start_beat.as_f32() + clip.duration_beats.as_f32()) / max_beat;
                    let x = (start_norm * tex_w as f32).round() as i32;
                    let w =
                        ((end_norm - start_norm) * tex_w as f32).round().max(1.0) as i32;
                    let y = (row as f32 * row_h).round() as i32;
                    let h = row_h.round().max(1.0) as i32;

                    bitmap_painter::fill_rect(
                        &mut self.overview_clip_pixels,
                        tex_w,
                        tex_h,
                        x,
                        y,
                        w,
                        h,
                        clip.color,
                    );
                }
            }
            self.overview_clips_dirty = false;
        }

        // ── Layer 2: Composite — copy cached clips, then overlay indicator + playhead ──
        self.overview_pixels.resize(total, Color32::TRANSPARENT);
        self.overview_pixels.copy_from_slice(&self.overview_clip_pixels);

        // Viewport indicator (semi-transparent blue)
        if ppb > 0.0 {
            let viewport_width_beats = self.tracks_rect.width / ppb;
            let vp_start_norm = scroll_x / max_beat;
            let vp_width_norm = viewport_width_beats / max_beat;
            let vp_x = (vp_start_norm * tex_w as f32).round() as i32;
            let vp_w = (vp_width_norm * tex_w as f32)
                .round()
                .min(tex_w as f32) as i32;
            bitmap_painter::fill_rect(
                &mut self.overview_pixels,
                tex_w,
                tex_h,
                vp_x,
                0,
                vp_w,
                tex_h as i32,
                color::OVERVIEW_VIEWPORT,
            );
            bitmap_painter::draw_border(
                &mut self.overview_pixels,
                tex_w,
                tex_h,
                vp_x,
                0,
                vp_w,
                tex_h as i32,
                color::OVERVIEW_VIEWPORT_BORDER,
                1,
            );
        }

        // Playhead (red line, 1-2px)
        let ph_norm = playhead / max_beat;
        let ph_x = (ph_norm * tex_w as f32).round().clamp(0.0, tex_w as f32) as i32;
        let ph_w = (1.0 * scale).round().max(1.0) as i32;
        bitmap_painter::fill_rect(
            &mut self.overview_pixels,
            tex_w,
            tex_h,
            ph_x,
            0,
            ph_w,
            tex_h as i32,
            color::OVERVIEW_PLAYHEAD,
        );

        self.overview_dirty = true;
    }

    /// Overview bitmap data for GPU upload. Returns (pixels, w, h) if dirty.
    pub fn overview_bitmap(&mut self) -> Option<(&[Color32], usize, usize)> {
        if self.overview_dirty
            && self.overview_tex_w > 0
            && self.overview_tex_h > 0
        {
            self.overview_dirty = false;
            Some((
                &self.overview_pixels,
                self.overview_tex_w,
                self.overview_tex_h,
            ))
        } else {
            None
        }
    }

    /// Overview rect (screen-space) for GPU rendering.
    pub fn overview_rect(&self) -> Rect {
        self.overview_rect
    }

    /// Repaint collapsed group bitmaps. Call once per frame before GPU upload.
    pub fn repaint_collapsed_groups(&mut self) {
        let (min_beat, max_beat) = self.visible_beat_range();
        let viewport_w = self.tracks_rect.width;
        let scale = self.render_scale;

        for (i, bmp_opt) in self.collapsed_group_bitmaps.iter_mut().enumerate() {
            let bmp = match bmp_opt.as_mut() {
                Some(b) => b,
                None => continue,
            };
            let track = &self.tracks[i];
            if !track.is_group || !track.is_collapsed || track.child_layer_indices.is_empty() {
                continue;
            }

            let track_h = track.height;
            if track_h <= 0.0 || viewport_w <= 0.0 {
                continue;
            }

            // Count child clips for dirty check
            let mut child_clip_count = 0usize;
            for &ci in &track.child_layer_indices {
                if ci < self.clips_by_layer.len() {
                    child_clip_count += self.clips_by_layer[ci].len();
                }
            }

            // Dirty-checking
            if !bmp.dirty
                && bmp.last_min_beat == min_beat
                && bmp.last_max_beat == max_beat
                && bmp.last_viewport_w == viewport_w
                && bmp.last_track_h == track_h
                && bmp.last_clip_count == child_clip_count
            {
                continue;
            }
            bmp.last_min_beat = min_beat;
            bmp.last_max_beat = max_beat;
            bmp.last_viewport_w = viewport_w;
            bmp.last_track_h = track_h;
            bmp.last_clip_count = child_clip_count;

            let tex_w = (viewport_w * scale).round().max(1.0) as usize;
            let tex_h = (track_h * scale).round().max(1.0) as usize;
            let total = tex_w * tex_h;
            bmp.pixels.resize(total, Color32::TRANSPARENT);
            bmp.pixels.fill(Color32::TRANSPARENT);
            bmp.tex_w = tex_w;
            bmp.tex_h = tex_h;

            let child_count = track.child_layer_indices.len();
            let rows_per_child = tex_h as f32 / child_count.max(1) as f32;
            let beat_range = max_beat - min_beat;
            if beat_range <= 0.0 {
                bmp.dirty = true;
                continue;
            }

            for (ci, &child_idx) in track.child_layer_indices.iter().enumerate() {
                let child_y = (ci as f32 * rows_per_child).round() as i32;
                let child_h = rows_per_child.round().max(1.0) as i32;

                let child_clips = if child_idx < self.clips_by_layer.len() {
                    &self.clips_by_layer[child_idx]
                } else {
                    continue;
                };

                for clip in child_clips {
                    let clip_start = clip.start_beat.as_f32();
                    let clip_end = clip_start + clip.duration_beats.as_f32();
                    if clip_end < min_beat || clip_start > max_beat {
                        continue;
                    }

                    let x_norm = (clip_start - min_beat) / beat_range;
                    let x2_norm = (clip_end - min_beat) / beat_range;
                    let x = (x_norm * tex_w as f32).round().max(0.0) as i32;
                    let x2 = (x2_norm * tex_w as f32).round().min(tex_w as f32) as i32;
                    let w = (x2 - x).max(1);

                    bitmap_painter::fill_rect(
                        &mut bmp.pixels,
                        tex_w,
                        tex_h,
                        x,
                        child_y,
                        w,
                        child_h,
                        clip.color,
                    );
                }
            }
            bmp.dirty = true;
        }
    }

    /// Iterate collapsed group bitmaps that need GPU upload.
    /// Yields (track_index, pixels, tex_w, tex_h) for dirty groups.
    pub fn dirty_collapsed_group_iter(
        &mut self,
    ) -> impl Iterator<Item = (usize, &[Color32], usize, usize)> {
        self.collapsed_group_bitmaps
            .iter_mut()
            .enumerate()
            .filter_map(|(i, opt)| {
                opt.as_mut().and_then(|bmp| {
                    if bmp.dirty && bmp.tex_w > 0 && bmp.tex_h > 0 {
                        bmp.dirty = false;
                        Some((i, bmp.pixels.as_slice(), bmp.tex_w, bmp.tex_h))
                    } else {
                        None
                    }
                })
            })
    }

    /// Screen-space rects for collapsed group bitmaps (for GPU rendering).
    /// Returns (layer_index_offset, rect) where layer_index_offset = 2000 + track_index.
    pub fn collapsed_group_rects(&self) -> Vec<(usize, Rect)> {
        let tr = &self.tracks_rect;
        let tr_top = tr.y;
        let tr_bottom = tr.y + tr.height;

        let mut rects = Vec::new();
        for (i, bmp_opt) in self.collapsed_group_bitmaps.iter().enumerate() {
            if bmp_opt.is_none() {
                continue;
            }
            let track = &self.tracks[i];
            if track.height <= 0.0 {
                continue;
            }
            let y = self.track_y(i);
            let clamped_y = y.max(tr_top);
            let clamped_h = (y + track.height).min(tr_bottom) - clamped_y;
            if clamped_h <= 0.0 {
                continue;
            }
            rects.push((2000 + i, Rect::new(tr.x, clamped_y, tr.width, clamped_h)));
        }
        rects
    }

    fn build_track_backgrounds(&mut self, tree: &mut UITree) {
        self.track_bg_ids.clear();
        self.track_bg_groups.clear();

        let tr = &self.tracks_rect;
        let tr_top = tr.y;
        let tr_bottom = tr.y + tr.height;

        // Pre-allocate ALL tracks (including off-screen) for update-in-place.
        // Off-screen tracks get set_visible(false).
        for (i, track) in self.tracks.iter().enumerate() {
            let y = self.track_y(i);
            let h = track.height;

            let clamped_y = y.max(tr_top);
            let clamped_h = (y + h).min(tr_bottom) - clamped_y;
            let visible = clamped_h > 0.0 && y + h >= tr_top && y <= tr_bottom;

            let bg_color = if i % 2 == 0 {
                color::TRACK_BG
            } else {
                color::TRACK_BG_ALT
            };
            let mut style = UIStyle {
                bg_color,
                ..UIStyle::default()
            };
            if track.is_muted {
                style.bg_color =
                    Color32::new(bg_color.r / 2, bg_color.g / 2, bg_color.b / 2, bg_color.a);
            }

            let bg_id = tree.add_panel(
                -1,
                tr.x,
                if visible { clamped_y } else { tr_top },
                tr.width,
                if visible { clamped_h } else { 0.0 },
                style,
            ) as i32;
            if !visible {
                tree.set_visible(bg_id as u32, false);
            }
            self.track_bg_ids.push(bg_id);

            // Group child accent bar — always allocated
            let accent_id = if let Some(accent) = track.accent_color.filter(|_| !track.is_group) {
                let aid = tree.add_panel(
                    -1,
                    tr.x,
                    if visible { clamped_y } else { tr_top },
                    color::GROUP_ACCENT_BAR_WIDTH,
                    if visible { clamped_h } else { 0.0 },
                    UIStyle {
                        bg_color: accent,
                        ..UIStyle::default()
                    },
                ) as i32;
                if !visible || y < tr_top {
                    tree.set_visible(aid as u32, false);
                }
                aid
            } else {
                -1
            };

            // Bottom separator — always allocated
            let (sep_h, sep_color) = if track.is_group {
                (color::GROUP_SEPARATOR_HEIGHT, color::GROUP_SEPARATOR_COLOR)
            } else {
                (color::TRACK_SEPARATOR_HEIGHT, color::SEPARATOR_COLOR)
            };
            let sep_y = y + h - sep_h;
            let sep_vis = visible && sep_y + sep_h > tr_top && sep_y < tr_bottom;
            let separator_id = tree.add_panel(
                -1,
                tr.x,
                if sep_vis { sep_y.max(tr_top) } else { tr_top },
                tr.width,
                if sep_vis {
                    (sep_y + sep_h).min(tr_bottom) - sep_y.max(tr_top)
                } else {
                    0.0
                },
                UIStyle {
                    bg_color: sep_color,
                    ..UIStyle::default()
                },
            ) as i32;
            if !sep_vis {
                tree.set_visible(separator_id as u32, false);
            }

            self.track_bg_groups.push(TrackBgGroup {
                bg_id,
                accent_id,
                separator_id,
            });
        }

        // Top separator is painted into the first layer's bitmap (not a UITree node)
        // because the layer bitmap textures render on top of UITree panels in a later
        // GPU pass, covering any UITree-based separator.
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

        let bar_skip = self.bar_skip();
        let start = (min_beat / tick_step).floor() * tick_step;
        let mut beat = start;
        let mut count = 0;
        let ruler_bottom = self.ruler_rect.y + self.ruler_rect.height;

        while beat <= max_beat && count < MAX_RULER_TICKS {
            let px = self.beat_to_pixel(Beats::from_f32(beat));
            if px >= self.ruler_rect.x && px <= self.ruler_rect.x_max() {
                let is_bar = (beat % bpb).abs() < 0.001;
                let is_beat = (beat % 1.0).abs() < 0.001;
                let is_label_beat = (beat % label_step).abs() < 0.001;

                // Skip intermediate bars at extreme zoom-out
                if is_bar && bar_skip > 1 {
                    let bar_num = (beat / bpb).round() as u32;
                    if !bar_num.is_multiple_of(bar_skip) {
                        beat += tick_step;
                        continue;
                    }
                }

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
                    self.viewport_clip_id,
                    px,
                    ruler_bottom - tick_h,
                    RULER_TICK_W,
                    tick_h,
                    UIStyle {
                        bg_color: tick_color,
                        ..UIStyle::default()
                    },
                ) as i32;
                self.ruler_tick_ids.push(id);

                // Label (only at label_step intervals to prevent overlap)
                // Skip labels at beats where a marker exists — markers take priority.
                let has_marker_at_beat = self
                    .markers
                    .iter()
                    .any(|m| (m.beat.as_f32() - beat).abs() < 0.001 && !m.name.is_empty());
                if is_label_beat && !has_marker_at_beat {
                    let bar_num = (beat / bpb).floor() as i32 + 1;
                    let beat_in_bar = ((beat % bpb) + 0.001).floor() as i32 + 1;
                    let label = if is_bar {
                        format!("{}", bar_num)
                    } else {
                        format!("{}.{}", bar_num, beat_in_bar)
                    };

                    let label_y = self.ruler_rect.y + 2.0;
                    let id = tree.add_label(
                        self.viewport_clip_id,
                        px + 2.0,
                        label_y,
                        RULER_LABEL_W,
                        RULER_LABEL_H,
                        &label,
                        UIStyle {
                            text_color: if is_bar {
                                color::TEXT_NORMAL
                            } else {
                                color::TEXT_DIMMED
                            },
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
        // Always pre-allocate all 3 export marker nodes for update-in-place.
        // Use set_visible(false) when not needed.
        let marker_w = 2.0;
        let marker_h = self.ruler_rect.height + self.tracks_rect.height;
        let marker_style = UIStyle {
            bg_color: color::EXPORT_MARKER_COLOR,
            ..UIStyle::default()
        };

        // In marker
        let in_px = self.beat_to_pixel(self.export_in_beat);
        self.export_in_marker_id = tree.add_panel(
            self.viewport_clip_id,
            in_px - marker_w * 0.5,
            self.ruler_rect.y,
            marker_w,
            marker_h,
            marker_style,
        ) as i32;

        // Range highlight
        let out_px = self.beat_to_pixel(self.export_out_beat);
        let range_left = in_px.max(self.tracks_rect.x);
        let range_right = out_px.min(self.tracks_rect.x_max());
        let range_w = (range_right - range_left).max(0.0);
        self.export_range_id = tree.add_panel(
            self.viewport_clip_id,
            range_left,
            self.tracks_rect.y,
            range_w,
            self.tracks_rect.height,
            UIStyle {
                bg_color: color::EXPORT_RANGE_HIGHLIGHT,
                ..UIStyle::default()
            },
        ) as i32;

        // Out marker
        self.export_out_marker_id = tree.add_panel(
            self.viewport_clip_id,
            out_px - marker_w * 0.5,
            self.ruler_rect.y,
            marker_w,
            marker_h,
            marker_style,
        ) as i32;

        // Apply visibility
        let enabled = self.export_range_enabled;
        let has_out = self.export_out_beat > self.export_in_beat;
        let in_visible =
            enabled && in_px >= self.tracks_rect.x && in_px <= self.tracks_rect.x_max();
        let out_visible = enabled
            && has_out
            && out_px >= self.tracks_rect.x
            && out_px <= self.tracks_rect.x_max();
        let range_visible = enabled && has_out && range_w > 0.0;

        if !in_visible {
            tree.set_visible(self.export_in_marker_id as u32, false);
        }
        if !range_visible {
            tree.set_visible(self.export_range_id as u32, false);
        }
        if !out_visible {
            tree.set_visible(self.export_out_marker_id as u32, false);
        }
    }

    /// Build insert cursor ruler marker only. Track-area cursor is painted
    /// into the per-layer bitmap by LayerBitmapRenderer.
    fn build_insert_cursor_ruler(&mut self, tree: &mut UITree) {
        let px = self.beat_to_pixel(self.insert_cursor_beat);
        let in_view = px >= self.tracks_rect.x && px <= self.tracks_rect.x_max();

        let marker_s = color::INSERT_CURSOR_RULER_MARKER_SIZE;
        self.insert_cursor_ruler_id = tree.add_panel(
            self.viewport_clip_id,
            px - marker_s * 0.5,
            self.ruler_rect.y + self.ruler_rect.height - marker_s,
            marker_s,
            marker_s,
            UIStyle {
                bg_color: color::INSERT_CURSOR_BLUE,
                ..UIStyle::default()
            },
        ) as i32;
        if !in_view {
            tree.set_visible(self.insert_cursor_ruler_id as u32, false);
        }
    }

    /// Build timeline marker vertical lines and flags in the ruler.
    fn build_markers(&mut self, tree: &mut UITree) {
        self.marker_flag_rects.clear();
        self.marker_node_ids.clear();
        self.marker_groups.clear();

        let flag_w = color::MARKER_FLAG_WIDTH;
        let flag_h = color::MARKER_FLAG_HEIGHT;

        // Pre-allocate ALL markers (including off-screen) for update-in-place.
        // Off-screen markers get set_visible(false).
        for marker in &self.markers {
            let px = self.beat_to_pixel(marker.beat);
            let in_view =
                px >= self.tracks_rect.x - flag_w && px <= self.tracks_rect.x_max() + flag_w;

            let mc = color::marker_color_to_color32(marker.color);
            let is_selected = self.selected_marker_ids.contains(&marker.id);

            // Flag in ruler (small colored rectangle at top)
            let flag_x = px - flag_w * 0.5;
            let flag_y = self.ruler_rect.y;
            let flag_color = if is_selected {
                Color32::new(
                    mc.r.saturating_add(40),
                    mc.g.saturating_add(40),
                    mc.b.saturating_add(40),
                    255,
                )
            } else {
                mc
            };
            let flag_id = tree.add_panel(
                self.viewport_clip_id,
                flag_x,
                flag_y,
                flag_w,
                flag_h,
                UIStyle {
                    bg_color: flag_color,
                    ..UIStyle::default()
                },
            ) as i32;
            if !in_view {
                tree.set_visible(flag_id as u32, false);
            }
            self.marker_node_ids.push(flag_id);

            // Selection outline — always allocated, hidden if not selected
            let outline_id = tree.add_panel(
                self.viewport_clip_id,
                flag_x - 1.0,
                flag_y - 1.0,
                flag_w + 2.0,
                flag_h + 2.0,
                UIStyle {
                    bg_color: color::MARKER_SELECTED_OUTLINE,
                    ..UIStyle::default()
                },
            ) as i32;
            if !is_selected || !in_view {
                tree.set_visible(outline_id as u32, false);
            }
            self.marker_node_ids.push(outline_id);

            // Label — always allocated, hidden if empty or off-screen
            let label_x = flag_x + flag_w + 2.0;
            let label_y = flag_y + (flag_h - color::MARKER_LABEL_HEIGHT) * 0.5;
            let label_id = tree.add_label(
                self.viewport_clip_id,
                label_x,
                label_y,
                color::MARKER_LABEL_WIDTH,
                color::MARKER_LABEL_HEIGHT,
                if marker.name.is_empty() { "" } else { &marker.name },
                UIStyle {
                    bg_color: color::MARKER_LABEL_BG,
                    text_color: mc,
                    font_size: RULER_FONT_SIZE,
                    text_align: TextAlign::Left,
                    ..UIStyle::default()
                },
            ) as i32;
            if marker.name.is_empty() || !in_view {
                tree.set_visible(label_id as u32, false);
            }
            self.marker_node_ids.push(label_id);

            self.marker_groups.push(MarkerNodeGroup {
                flag_id,
                outline_id,
                label_id,
            });

            // Store flag rect for hit-testing
            self.marker_flag_rects
                .push((marker.id.clone(), Rect::new(flag_x, flag_y, flag_w, flag_h)));
        }
    }

    // ── Update-in-place (Phase 1: horizontal scroll) ───────────

    /// Try to update ruler ticks, labels, markers, and export markers in-place
    /// for a horizontal-only scroll. Returns `true` if successful, `false` if
    /// a full rebuild is needed (count mismatch or never built).
    pub fn try_update_horizontal_scroll(&mut self, tree: &mut UITree) -> bool {
        // Guard: must have been built at least once
        if self.ruler_tick_ids.is_empty() {
            return false;
        }

        // ── Recompute ruler parameters (same logic as build_ruler) ──

        let (min_beat, max_beat) = self.visible_beat_range();
        let bpb = self.beats_per_bar as f32;
        let ppb = self.mapper.pixels_per_beat();
        let subdiv = self.grid_subdivision();

        let tick_step = match subdiv {
            GridSubdivision::Bar => bpb,
            GridSubdivision::Beat => 1.0,
            GridSubdivision::Eighth => 0.5,
            GridSubdivision::Sixteenth => 0.25,
        };

        const MIN_LABEL_SPACING: f32 = 50.0;
        let label_step: f32 = if ppb >= MIN_LABEL_SPACING {
            1.0
        } else if bpb * ppb >= MIN_LABEL_SPACING {
            bpb
        } else {
            let bar_px = bpb * ppb;
            let mut n_bars = 2.0_f32;
            while n_bars * bar_px < MIN_LABEL_SPACING && n_bars <= 1024.0 {
                n_bars *= 2.0;
            }
            bpb * n_bars
        };

        let bar_skip = self.bar_skip();
        let ruler_bottom = self.ruler_rect.y + self.ruler_rect.height;
        let start = (min_beat / tick_step).floor() * tick_step;
        let label_y = self.ruler_rect.y + 2.0;

        // ── Count ticks and labels, collect update data ──

        let mut tick_count = 0usize;
        let mut label_count = 0usize;
        let mut beat = start;

        // First pass: count only (to compare with existing)
        while beat <= max_beat && tick_count < MAX_RULER_TICKS {
            let px = self.beat_to_pixel(Beats::from_f32(beat));
            if px >= self.ruler_rect.x && px <= self.ruler_rect.x_max() {
                let is_bar = (beat % bpb).abs() < 0.001;

                // Skip intermediate bars at extreme zoom-out
                if is_bar && bar_skip > 1 {
                    let bar_num = (beat / bpb).round() as u32;
                    if !bar_num.is_multiple_of(bar_skip) {
                        beat += tick_step;
                        continue;
                    }
                }

                tick_count += 1;

                if (beat % label_step).abs() < 0.001
                    && !self.markers.iter().any(|m| {
                        (m.beat.as_f32() - beat).abs() < 0.001 && !m.name.is_empty()
                    })
                {
                    label_count += 1;
                }
            }
            beat += tick_step;
        }

        // Count mismatch → fallback to full rebuild
        if tick_count != self.ruler_tick_ids.len()
            || label_count != self.ruler_label_ids.len()
        {
            return false;
        }

        // Marker count changed → fallback
        if self.marker_groups.len() != self.markers.len() {
            return false;
        }

        // ── Second pass: update tick and label nodes in-place ──

        let mut tick_idx = 0usize;
        let mut label_idx = 0usize;
        beat = start;

        while beat <= max_beat && tick_idx < tick_count {
            let px = self.beat_to_pixel(Beats::from_f32(beat));
            if px >= self.ruler_rect.x && px <= self.ruler_rect.x_max() {
                let is_bar = (beat % bpb).abs() < 0.001;

                // Skip intermediate bars at extreme zoom-out
                if is_bar && bar_skip > 1 {
                    let bar_num = (beat / bpb).round() as u32;
                    if !bar_num.is_multiple_of(bar_skip) {
                        beat += tick_step;
                        continue;
                    }
                }

                let is_beat = (beat % 1.0).abs() < 0.001;
                let is_label_beat = (beat % label_step).abs() < 0.001;

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

                let id = self.ruler_tick_ids[tick_idx] as u32;
                tree.set_bounds(
                    id,
                    Rect::new(px, ruler_bottom - tick_h, RULER_TICK_W, tick_h),
                );
                tree.set_style(
                    id,
                    UIStyle {
                        bg_color: tick_color,
                        ..UIStyle::default()
                    },
                );
                tick_idx += 1;

                // Update label
                let has_marker_at_beat = self
                    .markers
                    .iter()
                    .any(|m| (m.beat.as_f32() - beat).abs() < 0.001 && !m.name.is_empty());
                if is_label_beat && !has_marker_at_beat && label_idx < label_count {
                    let bar_num = (beat / bpb).floor() as i32 + 1;
                    let beat_in_bar = ((beat % bpb) + 0.001).floor() as i32 + 1;
                    let label = if is_bar {
                        format!("{}", bar_num)
                    } else {
                        format!("{}.{}", bar_num, beat_in_bar)
                    };

                    let lid = self.ruler_label_ids[label_idx] as u32;
                    tree.set_bounds(
                        lid,
                        Rect::new(px + 2.0, label_y, RULER_LABEL_W, RULER_LABEL_H),
                    );
                    tree.set_text(lid, &label);
                    tree.set_style(
                        lid,
                        UIStyle {
                            text_color: if is_bar {
                                color::TEXT_NORMAL
                            } else {
                                color::TEXT_DIMMED
                            },
                            font_size: RULER_FONT_SIZE,
                            text_align: TextAlign::Left,
                            ..UIStyle::default()
                        },
                    );
                    label_idx += 1;
                }
            }
            beat += tick_step;
        }

        // ── Update export markers in-place ──

        if self.export_in_marker_id >= 0 {
            let marker_w = 2.0;
            let marker_h = self.ruler_rect.height + self.tracks_rect.height;
            let enabled = self.export_range_enabled;
            let has_out = self.export_out_beat > self.export_in_beat;

            let in_px = self.beat_to_pixel(self.export_in_beat);
            let in_vis = enabled
                && in_px >= self.tracks_rect.x
                && in_px <= self.tracks_rect.x_max();
            tree.set_visible(self.export_in_marker_id as u32, in_vis);
            if in_vis {
                tree.set_bounds(
                    self.export_in_marker_id as u32,
                    Rect::new(
                        in_px - marker_w * 0.5,
                        self.ruler_rect.y,
                        marker_w,
                        marker_h,
                    ),
                );
            }

            let out_px = self.beat_to_pixel(self.export_out_beat);
            let range_left = in_px.max(self.tracks_rect.x);
            let range_right = out_px.min(self.tracks_rect.x_max());
            let range_w = (range_right - range_left).max(0.0);
            let range_vis = enabled && has_out && range_w > 0.0;
            tree.set_visible(self.export_range_id as u32, range_vis);
            if range_vis {
                tree.set_bounds(
                    self.export_range_id as u32,
                    Rect::new(
                        range_left,
                        self.tracks_rect.y,
                        range_w,
                        self.tracks_rect.height,
                    ),
                );
            }

            let out_vis = enabled
                && has_out
                && out_px >= self.tracks_rect.x
                && out_px <= self.tracks_rect.x_max();
            tree.set_visible(self.export_out_marker_id as u32, out_vis);
            if out_vis {
                tree.set_bounds(
                    self.export_out_marker_id as u32,
                    Rect::new(
                        out_px - marker_w * 0.5,
                        self.ruler_rect.y,
                        marker_w,
                        marker_h,
                    ),
                );
            }
        }

        // ── Update timeline markers in-place ──

        let flag_w = color::MARKER_FLAG_WIDTH;
        let flag_h = color::MARKER_FLAG_HEIGHT;
        self.marker_flag_rects.clear();

        for (i, marker) in self.markers.iter().enumerate() {
            let group = &self.marker_groups[i];
            let px = self.beat_to_pixel(marker.beat);
            let in_view =
                px >= self.tracks_rect.x - flag_w && px <= self.tracks_rect.x_max() + flag_w;

            let mc = color::marker_color_to_color32(marker.color);
            let is_selected = self.selected_marker_ids.contains(&marker.id);
            let flag_x = px - flag_w * 0.5;
            let flag_y = self.ruler_rect.y;

            // Flag
            tree.set_visible(group.flag_id as u32, in_view);
            if in_view {
                let flag_color = if is_selected {
                    Color32::new(
                        mc.r.saturating_add(40),
                        mc.g.saturating_add(40),
                        mc.b.saturating_add(40),
                        255,
                    )
                } else {
                    mc
                };
                tree.set_bounds(
                    group.flag_id as u32,
                    Rect::new(flag_x, flag_y, flag_w, flag_h),
                );
                tree.set_style(
                    group.flag_id as u32,
                    UIStyle {
                        bg_color: flag_color,
                        ..UIStyle::default()
                    },
                );
            }

            // Outline
            tree.set_visible(group.outline_id as u32, in_view && is_selected);
            if in_view && is_selected {
                tree.set_bounds(
                    group.outline_id as u32,
                    Rect::new(flag_x - 1.0, flag_y - 1.0, flag_w + 2.0, flag_h + 2.0),
                );
            }

            // Label
            let has_name = !marker.name.is_empty();
            tree.set_visible(group.label_id as u32, in_view && has_name);
            if in_view && has_name {
                let label_x = flag_x + flag_w + 2.0;
                let label_y_m =
                    flag_y + (flag_h - color::MARKER_LABEL_HEIGHT) * 0.5;
                tree.set_bounds(
                    group.label_id as u32,
                    Rect::new(
                        label_x,
                        label_y_m,
                        color::MARKER_LABEL_WIDTH,
                        color::MARKER_LABEL_HEIGHT,
                    ),
                );
            }

            self.marker_flag_rects
                .push((marker.id.clone(), Rect::new(flag_x, flag_y, flag_w, flag_h)));
        }

        // ── Update insert cursor ──
        self.sync_insert_cursor_ruler(tree);

        true
    }

    // ── Update-in-place (Phase 2: vertical scroll) ─────────────

    /// Try to update track background Y positions in-place for vertical scroll.
    /// Returns `true` if successful, `false` if full rebuild needed.
    pub fn try_update_vertical_scroll(&mut self, tree: &mut UITree) -> bool {
        // Guard: must match current track count
        if self.track_bg_groups.len() != self.tracks.len()
            || self.track_bg_groups.is_empty()
        {
            return false;
        }

        let tr = &self.tracks_rect;
        let tr_top = tr.y;
        let tr_bottom = tr.y + tr.height;
        let tr_x = tr.x;
        let tr_w = tr.width;

        for (i, track) in self.tracks.iter().enumerate() {
            let group = &self.track_bg_groups[i];
            let y = self.track_y(i);
            let h = track.height;

            let clamped_y = y.max(tr_top);
            let clamped_h = (y + h).min(tr_bottom) - clamped_y;
            let visible = clamped_h > 0.0 && y + h >= tr_top && y <= tr_bottom;

            // Background
            tree.set_visible(group.bg_id as u32, visible);
            if visible {
                tree.set_bounds(
                    group.bg_id as u32,
                    Rect::new(tr_x, clamped_y, tr_w, clamped_h),
                );
            }

            // Accent bar
            if group.accent_id >= 0 {
                let accent_vis = visible && y >= tr_top;
                tree.set_visible(group.accent_id as u32, accent_vis);
                if accent_vis {
                    tree.set_bounds(
                        group.accent_id as u32,
                        Rect::new(tr_x, clamped_y, color::GROUP_ACCENT_BAR_WIDTH, clamped_h),
                    );
                }
            }

            // Separator
            let sep_h = if track.is_group {
                color::GROUP_SEPARATOR_HEIGHT
            } else {
                color::TRACK_SEPARATOR_HEIGHT
            };
            let sep_y = y + h - sep_h;
            let sep_vis = visible && sep_y + sep_h > tr_top && sep_y < tr_bottom;
            tree.set_visible(group.separator_id as u32, sep_vis);
            if sep_vis {
                tree.set_bounds(
                    group.separator_id as u32,
                    Rect::new(
                        tr_x,
                        sep_y.max(tr_top),
                        tr_w,
                        (sep_y + sep_h).min(tr_bottom) - sep_y.max(tr_top),
                    ),
                );
            }
        }

        true
    }
}

impl Default for TimelineViewportPanel {
    fn default() -> Self {
        Self::new()
    }
}

// ── Helpers ──────────────────────────────────────────────────────

// ── Tests ────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::Modifiers;
    use crate::layout::ScreenLayout;
    use crate::tree::UITree;

    fn test_layout() -> ScreenLayout {
        ScreenLayout::new(1920.0, 1080.0)
    }

    fn test_tracks() -> Vec<TrackInfo> {
        vec![
            TrackInfo {
                height: 140.0,
                ..Default::default()
            },
            TrackInfo {
                height: 140.0,
                ..Default::default()
            },
        ]
    }

    fn test_clips() -> Vec<ViewportClip> {
        vec![
            ViewportClip {
                clip_id: "clip_001".into(),
                layer_index: 0,
                start_beat: Beats::from_f32(0.0),
                duration_beats: Beats(4.0),
                name: "Intro".into(),
                color: color::CLIP_NORMAL,
                is_muted: false,
                is_locked: false,
                is_generator: false,
            },
            ViewportClip {
                clip_id: "clip_002".into(),
                layer_index: 1,
                start_beat: Beats::from_f32(4.0),
                duration_beats: Beats(8.0),
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
        panel.scroll_x_beats = Beats(0.0);

        let beat = Beats::from_f32(4.0);
        let px = panel.beat_to_pixel(beat);
        let beat_back = panel.pixel_to_beat(px);
        assert!((beat.as_f32() - beat_back.as_f32()).abs() < 0.001);
    }

    #[test]
    fn coordinate_mapping_with_scroll() {
        let mut panel = TimelineViewportPanel::new();
        panel.tracks_rect = Rect::new(0.0, 0.0, 1000.0, 500.0);
        panel.set_zoom(100.0);
        panel.scroll_x_beats = Beats(4.0);

        // Beat 4 should be at x=0 when scrolled to beat 4
        let px = panel.beat_to_pixel(Beats::from_f32(4.0));
        assert!((px - 0.0).abs() < 0.001);

        // Beat 5 should be at x=100
        let px = panel.beat_to_pixel(Beats::from_f32(5.0));
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
    fn clip_creation_step_walks_up_grid_levels() {
        let mut panel = TimelineViewportPanel::new();
        panel.beats_per_bar = 4;

        // ppb=200: sixteenth = 50px ≥ 40 → stays at 0.25
        panel.set_zoom(200.0);
        assert_eq!(panel.clip_creation_step(), Beats(0.25));

        // ppb=100: sixteenth = 25px < 40, eighth = 50px ≥ 40 → 0.5
        panel.set_zoom(100.0);
        assert_eq!(panel.clip_creation_step(), Beats(0.5));

        // ppb=20: sixteenth = 5px, eighth = 10px, beat = 20px < 40,
        //         bar = 80px ≥ 40 → 4.0
        panel.set_zoom(20.0);
        assert_eq!(panel.clip_creation_step(), Beats(4.0));

        // ppb=50: sixteenth = 12.5px < 40, eighth = 25px < 40,
        //         beat = 50px ≥ 40 → 1.0
        panel.set_zoom(50.0);
        assert_eq!(panel.clip_creation_step(), Beats(1.0));

        // Very zoomed out ppb=5: grid_step = bar (4.0), 4*5=20px < 40 → still bar (fallback)
        panel.set_zoom(5.0);
        assert_eq!(panel.clip_creation_step(), Beats(4.0));
    }

    #[test]
    fn click_ruler_seeks() {
        let mut tree = UITree::new();
        let mut panel = TimelineViewportPanel::new();
        let layout = test_layout();
        panel.set_tracks(test_tracks());
        panel.build(&mut tree, &layout);

        // Click in ruler area
        let ruler_pos = Vec2::new(panel.ruler_rect.x + 100.0, panel.ruler_rect.y + 5.0);
        let actions = panel.handle_event(
            &UIEvent::Click {
                node_id: 0,
                pos: ruler_pos,
                modifiers: Modifiers::default(),
            },
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

        let tracks_pos = Vec2::new(panel.tracks_rect.x + 100.0, panel.tracks_rect.y + 50.0);
        let actions = panel.handle_event(
            &UIEvent::Click {
                node_id: 0,
                pos: tracks_pos,
                modifiers: Modifiers::default(),
            },
            &tree,
        );
        assert!(
            actions.is_empty(),
            "tracks clicks handled by overlay, not viewport"
        );
    }

    #[test]
    fn shift_click_empty_tracks_handled_by_overlay() {
        // Tracks-area Shift+clicks are now handled by InteractionOverlay.
        let mut tree = UITree::new();
        let mut panel = TimelineViewportPanel::new();
        let layout = test_layout();
        panel.set_tracks(test_tracks());
        panel.build(&mut tree, &layout);

        let tracks_pos = Vec2::new(panel.tracks_rect.x + 100.0, panel.tracks_rect.y + 50.0);
        let actions = panel.handle_event(
            &UIEvent::Click {
                node_id: 0,
                pos: tracks_pos,
                modifiers: Modifiers {
                    shift: true,
                    ..Modifiers::default()
                },
            },
            &tree,
        );
        assert!(
            actions.is_empty(),
            "tracks clicks handled by overlay, not viewport"
        );
    }

    #[test]
    fn offscreen_clips_bucketed_but_not_rendered_as_nodes() {
        let mut tree = UITree::new();
        let mut panel = TimelineViewportPanel::new();
        let layout = test_layout();
        panel.set_tracks(test_tracks());

        // Put clips far off-screen
        panel.set_clips(vec![ViewportClip {
            clip_id: "clip_099".into(),
            layer_index: 0,
            start_beat: Beats::from_f32(1000.0),
            duration_beats: Beats(4.0),
            name: "Far".into(),
            color: color::CLIP_NORMAL,
            is_muted: false,
            is_locked: false,
            is_generator: false,
        }]);
        panel.build(&mut tree, &layout);

        // Clips are now painted into bitmaps, not UITree nodes.
        // They should be bucketed by layer.
        assert_eq!(panel.clips_by_layer[0].len(), 1);
    }

    #[test]
    fn clip_color_uses_layer_color() {
        use crate::bitmap_painter;
        use crate::node::Color32;

        let lc = Color32::new(180, 120, 60, 220);
        let normal = bitmap_painter::get_clip_color(false, false, false, false, false, lc);
        // Normal state = layer color at brightness 1.0
        assert_eq!(normal.r, 180);
        assert_eq!(normal.g, 120);
        assert_eq!(normal.b, 60);

        let locked = bitmap_painter::get_clip_color(false, false, false, true, false, lc);
        assert_eq!(locked, color::CLIP_LOCKED);
    }
}
