use super::{Panel, PanelAction};
use crate::bitmap_painter;
use crate::color;
use crate::coordinate_mapper::CoordinateMapper;
use crate::input::UIEvent;
use crate::layout::ScreenLayout;
use crate::node::*;
use crate::snap;
use crate::tree::UITree;
use crate::view::UiMarker;
use manifold_foundation::{Beats, ClipId, LayerId, MarkerId};

// ── Layout constants ────────────────────────────────────────────

const RULER_HEIGHT: f32 = color::RULER_HEIGHT;
// One source for the clip vertical inset: the hover hit-tester, the click/drag
// hit-tester (via `app.rs` → `color::CLIP_VERTICAL_PAD`), and the renderer all read
// this. A second independent `= 12.0` here is exactly how the two hit-test paths
// drifted before — keep it aliased so they can't.
const CLIP_VERTICAL_PAD: f32 = color::CLIP_VERTICAL_PAD;
/// Insert-cursor bar width in logical pixels (Unity UIConstants.InsertCursorWidthPx).
const INSERT_CURSOR_WIDTH: f32 = 2.0;

const RULER_FONT_SIZE: u16 = color::FONT_SMALL;
const RULER_TICK_W: f32 = 1.0;
const RULER_BEAT_TICK_H: f32 = 8.0;
const RULER_BAR_TICK_H: f32 = 14.0;
const RULER_LABEL_H: f32 = 14.0;
const RULER_LABEL_W: f32 = 40.0;
// Maximum nodes to allocate for ruler ticks (avoid unbounded allocation)
const MAX_RULER_TICKS: usize = 1500;

// ── Submodules (the god-panel split: model / coordinate / render / interaction) ──
// `panels/viewport.rs` stays as the parent module and owns these concern files.
// See `docs/TIMELINE_API_DESIGN.md` §3.6.
mod coordinate;
mod interaction;
mod model;
mod render;

// One hit-result type for the whole timeline: defined in `clip_hit_tester` (the
// shared hit-tester) and surfaced here so viewport consumers and the click/drag
// overlay name the same type.
pub use crate::clip_hit_tester::{ClipHitResult, HitRegion};
pub use model::{ClipScreenRect, SelectionRegion, TimelineOverlays, TrackInfo, ViewportClip};
use coordinate::GridSubdivision;
use model::{CollapsedGroupBitmap, MarkerNodeGroup, TrackBgGroup};

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

    // Per-track style/state (heights + Y come from `mapper`, the sole authority)
    tracks: Vec<TrackInfo>,

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
    bg_panel_id: Option<NodeId>,
    overview_btn_id: Option<NodeId>,
    ruler_bg_id: Option<NodeId>,
    viewport_clip_id: Option<NodeId>,
    // playhead: unified overlay quad in app.rs (ruler → waveform → stems → tracks)
    insert_cursor_ruler_id: Option<NodeId>,

    // Horizontal scrollbar (§24 5e): a reserved strip below the tracks. The strip
    // rect is the single source for its draw + hit geometry; the button exists
    // only so the input system routes presses/drags here.
    scrollbar_h_rect: Rect,
    scrollbar_h_btn_id: Option<NodeId>,
    /// Pointer-to-thumb-left offset captured at drag start, so the thumb tracks
    /// the pointer 1:1 instead of snapping its left edge under the cursor.
    scrollbar_grab_dx: f32,
    // insert_cursor_track_id: removed — painted into bitmap
    // selection_region_id: removed — painted into bitmap

    // Export range
    export_in_beat: Beats,
    export_out_beat: Beats,
    export_range_enabled: bool,

    // Node IDs — fixed export elements
    export_range_id: Option<NodeId>,
    export_in_marker_id: Option<NodeId>,
    export_out_marker_id: Option<NodeId>,

    // Node IDs — dynamic elements (rebuilt on scroll/zoom)
    ruler_tick_ids: Vec<NodeId>,
    ruler_label_ids: Vec<NodeId>,
    // grid_line_ids: removed — grid painted into bitmap
    track_bg_ids: Vec<NodeId>,
    track_bg_groups: Vec<TrackBgGroup>,
    /// The focused layer's track index — its lane background lifts one ramp step
    /// (§19 timeline echo), the same focus emphasis the inspector card gets.
    /// `cached_*` drives the dirty-checked in-place recolor on selection change.
    active_track_index: Option<usize>,
    cached_active_track_index: Option<usize>,
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
    markers: Vec<UiMarker>,
    marker_line_cache: Vec<(f32, Color32)>,
    selected_marker_ids: Vec<MarkerId>,
    marker_node_ids: Vec<NodeId>,
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
    ScrollbarHDrag,
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
            bg_panel_id: None,
            overview_btn_id: None,
            ruler_bg_id: None,
            viewport_clip_id: None,
            insert_cursor_ruler_id: None,
            scrollbar_h_rect: Rect::ZERO,
            scrollbar_h_btn_id: None,
            scrollbar_grab_dx: 0.0,
            export_in_beat: Beats::ZERO,
            export_out_beat: Beats::ZERO,
            export_range_enabled: false,
            export_range_id: None,
            export_in_marker_id: None,
            export_out_marker_id: None,
            ruler_tick_ids: Vec::new(),
            ruler_label_ids: Vec::new(),
            track_bg_ids: Vec::new(),
            track_bg_groups: Vec::new(),
            active_track_index: None,
            cached_active_track_index: None,
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

        // Create/resize bitmap renderers (one per visible layer, including groups).
        // Heights come from the CoordinateMapper — the sole Y-layout authority.
        // `rebuild_mapper_layout` runs before `set_tracks` (state_sync), so the
        // mapper is current here; a zero height means a hidden child layer.
        self.bitmap_renderers.clear();
        for i in 0..self.tracks.len() {
            let height = self.mapper.get_layer_height(i);
            if height <= 0.0 {
                self.bitmap_renderers.push(None);
            } else {
                self.bitmap_renderers
                    .push(Some(crate::bitmap_renderer::LayerBitmapRenderer::new(
                        i,
                        self.render_scale,
                        height,
                    )));
            }
        }

        // Collapsed group bitmap slots (one per track, None for non-groups)
        self.collapsed_group_bitmaps.clear();
        for track in &self.tracks {
            if track.is_group && track.is_collapsed && !track.child_layer_indices.is_empty() {
                self.collapsed_group_bitmaps
                    .push(Some(CollapsedGroupBitmap::new()));
            } else {
                self.collapsed_group_bitmaps.push(None);
            }
        }
    }

    /// Rebuild the CoordinateMapper's Y-layout from layer data.
    /// Call this from app.rs when layers change (before build).
    pub fn rebuild_mapper_layout(&mut self, layers: &[crate::view::UiLayer]) {
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
            clip_fp = clip_fp.wrapping_mul(31).wrapping_add(c.layer_index as u64);
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

    /// Repaint all dirty layer grid bitmaps. Call once per frame before GPU
    /// upload. The grid is a pure function of the viewport (scroll / zoom / width
    /// / time-sig), so this takes no selection/clip state — clip bodies, content,
    /// and overlays are all GPU now.
    pub fn repaint_dirty_layers(&mut self) {
        let (min_beat, max_beat) = self.visible_beat_range();
        let viewport_width_px = self.tracks_rect.width;
        let time_sig = self.beats_per_bar;
        let ppb = self.mapper.pixels_per_beat();

        for renderer in self.bitmap_renderers.iter_mut().flatten() {
            renderer.repaint(min_beat, max_beat, viewport_width_px, time_sig, ppb);
        }
    }

    /// Iterate layer grid bitmaps that were repainted (for GPU upload).
    /// Yields `(layer_index, pixels, tex_w, tex_h)` for dirty layers.
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

    /// On-screen clip rectangles for the GPU clip pass (§24 5b), rebuilt every
    /// frame into `out`. Mirrors `layer_bitmap_rects`' visibility cull and the
    /// hit-tester's geometry (same `beat_to_pixel` / `track_y` + `CLIP_VERTICAL_PAD`),
    /// so the drawn body and the clickable region can't disagree. Group layers are
    /// skipped — collapsed groups draw their own summary bitmap and expanded group
    /// rows carry no clips. Selection / hover / marquee are resolved by the caller.
    pub fn visible_clip_rects(&self, out: &mut Vec<ClipScreenRect>) {
        out.clear();
        let tx0 = self.tracks_rect.x;
        let tx1 = self.tracks_rect.x + self.tracks_rect.width;
        let ty0 = self.tracks_rect.y;
        let ty1 = self.tracks_rect.y + self.tracks_rect.height;
        for (i, renderer_opt) in self.bitmap_renderers.iter().enumerate() {
            if renderer_opt.is_none() || self.is_group_layer(i) {
                continue;
            }
            let track_y = self.track_y(i);
            let track_h = self.track_height(i);
            // Same visibility cull as layer_bitmap_rects.
            if track_h <= 0.0 || track_y + track_h < ty0 || track_y > ty1 {
                continue;
            }
            let clip_top = track_y + CLIP_VERTICAL_PAD;
            let clip_h = track_h - CLIP_VERTICAL_PAD * 2.0;
            if clip_h <= 0.0 {
                continue;
            }
            for clip in self.clips_for_layer(i) {
                let x = self.beat_to_pixel(clip.start_beat);
                let w = self.beat_duration_to_width(clip.duration_beats.as_f32());
                // Sub-pixel clips and clips fully outside the tracks rect are
                // skipped; the GPU scissor clamps partials at the edges.
                if w < 1.0 || x + w < tx0 || x > tx1 {
                    continue;
                }
                out.push(ClipScreenRect {
                    clip_id: clip.clip_id.clone(),
                    layer_index: i,
                    rect: Rect::new(x, clip_top, w, clip_h),
                    base_color: clip.color,
                    name: clip.name.clone(),
                    start_beat: clip.start_beat,
                    end_beat: clip.start_beat + clip.duration_beats,
                    is_muted: clip.is_muted,
                    is_locked: clip.is_locked,
                    is_generator: clip.is_generator,
                    is_audio: clip.is_audio,
                    waveform: clip.waveform.clone(),
                    in_point_seconds: clip.in_point_seconds,
                    warped_secs_per_beat: clip.warped_secs_per_beat,
                });
            }
        }
    }

    /// Screen-space geometry for the timeline overlays that sit ON TOP of the
    /// clip bodies + waveforms: the marquee/region highlight, the insert cursor,
    /// and the beat markers. Since §24 5b these are GPU rects emitted in the
    /// overlay pass rather than baked into a per-layer bitmap. The caller scissors
    /// to the tracks rect and draws them under the clip names. `insert_cursor_layer`
    /// + `has_insert` come from the app (it owns the resolved cursor layer).
    pub fn timeline_overlays(
        &self,
        insert_cursor_layer: Option<usize>,
        has_insert: bool,
        markers_out: &mut Vec<(f32, Color32)>,
    ) -> TimelineOverlays {
        let (min_beat, max_beat) = self.visible_beat_range();

        // Region / marquee highlight: one translucent rect over the contiguous
        // beat × layer span (mirrors the per-layer bitmap fill, unioned).
        let region = self.selection_region.and_then(|r| {
            if r.end_layer >= self.tracks.len() {
                return None;
            }
            let x0 = self.beat_to_pixel(r.start_beat);
            let x1 = self.beat_to_pixel(r.end_beat);
            let y0 = self.track_y(r.start_layer);
            let y1 = self.track_y(r.end_layer) + self.track_height(r.end_layer);
            if x1 <= x0 || y1 <= y0 {
                return None;
            }
            Some((
                Rect::new(x0, y0, x1 - x0, y1 - y0),
                color::ACCENT_BLUE_SELECTION,
            ))
        });

        // Insert cursor: a thin vertical bar on its target layer's row.
        let cursor = if has_insert {
            insert_cursor_layer.and_then(|layer| {
                if layer >= self.tracks.len() {
                    return None;
                }
                let x = self.beat_to_pixel(self.insert_cursor_beat);
                let y = self.track_y(layer);
                let h = self.track_height(layer);
                if h <= 0.0 {
                    return None;
                }
                Some((Rect::new(x, y, INSERT_CURSOR_WIDTH, h), color::INSERT_CURSOR_BLUE))
            })
        } else {
            None
        };

        // Beat markers: full-height vertical lines, culled to the visible range.
        // Written into the caller's reusable scratch so this allocates nothing.
        markers_out.clear();
        for &(beat, color) in &self.marker_line_cache {
            if beat < min_beat || beat > max_beat {
                continue;
            }
            markers_out.push((self.beat_to_pixel(Beats::from_f32(beat)), color));
        }

        TimelineOverlays { region, cursor }
    }

    pub fn set_zoom(&mut self, pixels_per_beat: f32) {
        self.mapper.set_zoom(pixels_per_beat);
    }

    pub fn set_zoom_index(&mut self, index: usize) {
        self.mapper.set_zoom_by_index(index);
    }

    /// New ppb after stepping `delta` discrete zoom levels from the nearest current
    /// level (the +/- buttons / keyboard). See [`CoordinateMapper::zoom_level_stepped`].
    pub fn zoom_level_stepped(&self, delta: i32) -> f32 {
        self.mapper.zoom_level_stepped(delta)
    }

    /// New ppb after a continuous multiplicative zoom by `factor`, clamped to the
    /// zoom range. See [`CoordinateMapper::zoom_continuous`].
    pub fn zoom_continuous(&self, factor: f32) -> f32 {
        self.mapper.zoom_continuous(factor)
    }

    /// Re-zoom to `new_ppb` while keeping `anchor_beat` under `anchor_screen_x`
    /// (clamped into the tracks rect). Sets zoom + horizontal scroll atomically —
    /// the one anchored-zoom entry point shared by scroll-wheel zoom (anchor =
    /// cursor) and the +/- buttons / keyboard (anchor = playhead), so the anchor
    /// maths can't drift between them (§24 5e).
    pub fn zoom_to(&mut self, new_ppb: f32, anchor_beat: f32, anchor_screen_x: f32) {
        let local_x =
            (anchor_screen_x - self.tracks_rect.x).clamp(0.0, self.tracks_rect.width.max(0.0));
        self.mapper.set_zoom(new_ppb);
        // set_zoom clamps to a floor, so read back the applied ppb for the anchor.
        let applied_ppb = self.mapper.pixels_per_beat();
        let new_scroll = anchor_beat - local_x / applied_ppb;
        self.set_scroll(new_scroll.max(0.0), self.scroll_y_px);
    }

    // ── Horizontal scrollbar (§24 5e) ────────────────────────────────

    /// Total content length in beats represented by the horizontal scrollbar:
    /// the furthest clip end, but never less than one visible window (so a short
    /// timeline still shows a full thumb). `visible_beats` is the on-screen span.
    fn scrollbar_content_beats(&self, visible_beats: f32) -> f32 {
        self.max_content_beat().max(visible_beats)
    }

    /// Horizontal scrollbar geometry: `(track, thumb)` rects in screen space, or
    /// `None` when the whole timeline fits (nothing to scroll). The single source
    /// for both the GPU draw (app.rs) and the drag hit-test, so the drawn thumb
    /// and the grabbable region can't drift apart.
    pub fn scrollbar_h_layout(&self) -> Option<(Rect, Rect)> {
        let track = self.scrollbar_h_rect;
        let ppb = self.mapper.pixels_per_beat();
        if track.width <= 0.0 || track.height <= 0.0 || ppb <= 0.0 {
            return None;
        }
        let visible_beats = track.width / ppb;
        let content_beats = self.scrollbar_content_beats(visible_beats);
        let scrollable_beats = content_beats - visible_beats;
        if scrollable_beats <= 0.001 {
            return None; // everything fits — no scrollbar
        }

        let inset = color::TIMELINE_SCROLLBAR_THUMB_INSET;
        let track_inner_x = track.x + inset;
        let track_inner_w = (track.width - inset * 2.0).max(1.0);

        let frac_visible = (visible_beats / content_beats).clamp(0.0, 1.0);
        let min_thumb = color::TIMELINE_SCROLLBAR_MIN_THUMB.min(track_inner_w);
        let thumb_w = (track_inner_w * frac_visible).max(min_thumb).min(track_inner_w);

        let scroll_frac = (self.scroll_x_beats.as_f32() / scrollable_beats).clamp(0.0, 1.0);
        let thumb_x = track_inner_x + scroll_frac * (track_inner_w - thumb_w);
        let thumb = Rect::new(
            thumb_x,
            track.y + inset,
            thumb_w,
            (track.height - inset * 2.0).max(1.0),
        );
        Some((track, thumb))
    }

    /// Map a desired thumb-left screen-x to the scroll-x (beats) it represents.
    /// Returns `None` when there is no scrollbar. Linear over the content range,
    /// so dragging is stable regardless of the current scroll.
    fn scrollbar_h_scroll_at(&self, thumb_left: f32) -> Option<f32> {
        let (track, thumb) = self.scrollbar_h_layout()?;
        let ppb = self.mapper.pixels_per_beat();
        let visible_beats = track.width / ppb;
        let scrollable_beats = self.scrollbar_content_beats(visible_beats) - visible_beats;
        let inset = color::TIMELINE_SCROLLBAR_THUMB_INSET;
        let track_inner_x = track.x + inset;
        let track_inner_w = (track.width - inset * 2.0).max(1.0);
        let travel = (track_inner_w - thumb.width).max(0.0001);
        let frac = ((thumb_left - track_inner_x) / travel).clamp(0.0, 1.0);
        Some(frac * scrollable_beats.max(0.0))
    }

    /// True while the horizontal scrollbar thumb is being dragged (app.rs uses it
    /// to draw the thumb in its active colour).
    pub fn scrollbar_h_dragging(&self) -> bool {
        self.drag_mode == ViewportDragMode::ScrollbarHDrag
    }

    /// Set scroll position (clamped). Returns true if the value actually changed.
    pub fn set_scroll(&mut self, scroll_x_beats: f32, scroll_y_px: f32) -> bool {
        let new_x = scroll_x_beats.max(0.0);
        // Clamp vertical scroll: never scroll past the last track
        let viewport_h = self.tracks_rect.height;
        let max_scroll_y = (self.mapper.total_content_height() - viewport_h).max(0.0);
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

    /// Set the focused layer's track index — its lane lifts one ramp step (§19
    /// timeline echo). Stored now, applied in `update()` via dirty-check; `None`
    /// clears the focus. Cheap to call every frame.
    pub fn set_active_track_index(&mut self, index: Option<usize>) {
        self.active_track_index = index;
    }

    /// Recolor the focused-lane highlight in place when the active layer changes,
    /// without a track rebuild (§19 echo): the old lane drops to its resting
    /// stripe, the new lane lifts one ramp step. Dirty-checked, so a stable
    /// selection costs one comparison per frame.
    fn sync_active_track_lane(&mut self, tree: &mut UITree) {
        if self.active_track_index == self.cached_active_track_index {
            return;
        }
        let changed = [self.cached_active_track_index, self.active_track_index];
        self.cached_active_track_index = self.active_track_index;
        for idx in changed.into_iter().flatten() {
            let Some(&bg_id) = self.track_bg_ids.get(idx) else {
                continue;
            };
            let style = UIStyle {
                bg_color: self.track_bg_color(idx),
                ..UIStyle::default()
            };
            tree.set_style(bg_id, style);
        }
    }

    pub fn set_markers(&mut self, markers: Vec<UiMarker>) {
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
    pub fn markers_stale(&self, source: &[UiMarker]) -> bool {
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

}

impl Panel for TimelineViewportPanel {
    fn build(&mut self, tree: &mut UITree, layout: &ScreenLayout) {
        self.first_node = tree.count();

        // Clear the scrollbar strip up front so a build that early-returns (the
        // timeline body or tracks width collapsed to 0 — e.g. the split dragged
        // shut) can't leave a stale, still-grabbable scrollbar behind. The normal
        // path overwrites this below; `Rect::ZERO` makes `scrollbar_h_layout`
        // return `None` (no draw) and its hit-test `contains` false (no grab).
        self.scrollbar_h_rect = Rect::ZERO;

        let body = layout.timeline_body();
        if body.width <= 0.0 || body.height <= 0.0 {
            self.node_count = 0;
            return;
        }

        // Viewport areas. The tracks occupy the timeline body to the RIGHT of the
        // layer controls; `layout.timeline_tracks()` is the single source for that
        // x/width split (the layer headers read the matching
        // `layout.layer_controls()`), so the two cannot drift apart.
        let tracks = layout.timeline_tracks();
        let tracks_x = tracks.x;
        let tracks_w = tracks.width;
        if tracks_w <= 0.0 {
            self.node_count = 0;
            return;
        }

        // Header stack: overview strip + ruler + optional waveform/stem lanes.
        // `track_header_height()` is the single source for this offset — both the
        // viewport and `layer_header` read it, so the layer controls cannot drift
        // out of vertical alignment with their tracks (nothing recomputes it).
        let header_h = layout.track_header_height();
        // Reserve a slim strip at the very bottom of the timeline body for the
        // horizontal scrollbar (§24 5e). It sits OUTSIDE `tracks_rect`, so a drag
        // there never reaches the clip-marquee InteractionOverlay.
        let sb_h = color::TIMELINE_SCROLLBAR_HEIGHT;
        self.viewport_rect = Rect::new(tracks_x, body.y, tracks_w, body.height);
        self.ruler_rect = Rect::new(
            tracks_x,
            body.y + color::OVERVIEW_STRIP_HEIGHT,
            tracks_w,
            RULER_HEIGHT,
        );
        self.tracks_rect = Rect::new(
            tracks_x,
            body.y + header_h,
            tracks_w,
            (body.height - header_h - sb_h).max(0.0),
        );
        self.scrollbar_h_rect = Rect::new(tracks_x, body.y + body.height - sb_h, tracks_w, sb_h);

        // Background
        self.bg_panel_id = Some(tree.add_panel(
            None,
            self.viewport_rect.x,
            self.viewport_rect.y,
            self.viewport_rect.width,
            self.viewport_rect.height,
            UIStyle {
                bg_color: color::DARK_BG,
                ..UIStyle::default()
            },
        ));

        // Overview strip at top of viewport.
        // From Unity OverviewStripPanel.cs — mini-timeline with clip miniatures,
        // viewport indicator, playhead, and export range markers.
        self.overview_rect = Rect::new(tracks_x, body.y, tracks_w, color::OVERVIEW_STRIP_HEIGHT);
        let overview_rect = self.overview_rect;
        // Interactive button so hit_test returns valid ID for click/drag scrubbing.
        // Clip miniatures (non-interactive panels) are added on top but fall through
        // to this button on hit_test. Same pattern as the tracks area overlay.
        self.overview_btn_id = Some(tree.add_button(
            None,
            overview_rect.x,
            overview_rect.y,
            overview_rect.width,
            overview_rect.height,
            UIStyle {
                bg_color: color::OVERVIEW_BG,
                ..UIStyle::default()
            },
            "",
        ));

        // Overview strip bitmap — repainted in repaint_overview() each frame,
        // uploaded and rendered via the layer bitmap GPU path (index 1002).
        self.overview_dirty = true;

        // Ruler background — INTERACTIVE so clicks register for playhead scrubbing
        self.ruler_bg_id = Some(tree.add_button(
            None,
            self.ruler_rect.x,
            self.ruler_rect.y,
            self.ruler_rect.width,
            self.ruler_rect.height,
            UIStyle {
                bg_color: color::HEADER_BG,
                ..UIStyle::default()
            },
            "",
        ));

        // Clip region covering ruler + tracks — prevents ticks, labels, markers,
        // and export range elements from bleeding past the viewport bounds into
        // adjacent panels (e.g. live recording section to the right).
        self.viewport_clip_id = Some(tree.add_node(
            None,
            self.viewport_rect,
            UINodeType::ClipRegion,
            UIStyle::default(),
            None,
            UIFlags::CLIPS_CHILDREN,
        ));

        // Interactive overlay covering entire tracks area — catches all clicks/drags
        // (matches Unity's InteractionOverlay which is a transparent MonoBehaviour
        // covering the tracks viewport). Without this, clicks on non-interactive
        // panel nodes (track backgrounds, grid lines) won't generate events.
        tree.add_button(
            None,
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

        // Horizontal scrollbar routing button (§24 5e). Transparent — the track
        // and thumb are drawn as GPU rects in app.rs from `scrollbar_h_layout`
        // (one geometry source for draw + hit). The button only exists so the
        // input system emits press/drag events over the strip; the actual routing
        // is by rect containment in `on_timeline_event`.
        self.scrollbar_h_btn_id = Some(tree.add_button(
            None,
            self.scrollbar_h_rect.x,
            self.scrollbar_h_rect.y,
            self.scrollbar_h_rect.width,
            self.scrollbar_h_rect.height,
            UIStyle {
                bg_color: Color32::TRANSPARENT,
                ..UIStyle::default()
            },
            "",
        ));

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
        self.sync_active_track_lane(tree);
    }

    fn handle_event(&mut self, event: &UIEvent, _tree: &UITree) -> Vec<PanelAction> {
        // Tracks-area interaction (click, drag, hover) is handled by
        // InteractionOverlay in app.rs — NOT here. The viewport-local ruler /
        // overview / marker routing lives in `interaction::on_timeline_event`.
        self.on_timeline_event(event)
    }

    fn first_node(&self) -> usize {
        self.first_node
    }
    fn node_count(&self) -> usize {
        self.node_count
    }
}

// ── Build helpers (private) ──────────────────────────────────────

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
        // Heights live on the mapper now; an unconfigured mapper defaults every
        // layer to TRACK_HEIGHT (140), which is what these two tracks expect.
        vec![TrackInfo::default(), TrackInfo::default()]
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
                is_audio: false,
                waveform: None,
                in_point_seconds: 0.0,
                warped_secs_per_beat: 0.0,
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
                is_audio: false,
                waveform: None,
                in_point_seconds: 0.0,
                warped_secs_per_beat: 0.0,
            },
        ]
    }

    #[test]
    fn focused_lane_lifts_one_ramp_step() {
        let mut panel = TimelineViewportPanel::new();
        panel.set_tracks(test_tracks()); // two unmuted default tracks
        // No focus → both lanes sit at their resting zebra stripe.
        assert_eq!(panel.track_bg_color(0), crate::color::TRACK_BG);
        assert_eq!(panel.track_bg_color(1), crate::color::TRACK_BG_ALT);
        // Focus lane 0 → it lifts one ramp step; the other lane is untouched.
        panel.set_active_track_index(Some(0));
        assert_eq!(
            panel.track_bg_color(0),
            crate::color::lighten(crate::color::TRACK_BG, crate::color::FOCUS_LIFT_STEP)
        );
        assert_eq!(panel.track_bg_color(1), crate::color::TRACK_BG_ALT);
        // Clearing focus returns the lane to its resting stripe.
        panel.set_active_track_index(None);
        assert_eq!(panel.track_bg_color(0), crate::color::TRACK_BG);
    }

    #[test]
    fn build_empty_viewport() {
        let mut tree = UITree::new();
        let mut panel = TimelineViewportPanel::new();
        let layout = test_layout();
        panel.build(&mut tree, &layout);

        assert!(panel.bg_panel_id.is_some());
        assert!(panel.ruler_bg_id.is_some());
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
    fn marker_hit_test_matches_drawn_flag() {
        let mut panel = TimelineViewportPanel::new();
        panel.tracks_rect = Rect::new(100.0, 200.0, 1000.0, 500.0);
        panel.ruler_rect = Rect::new(100.0, 160.0, 1000.0, 40.0);
        panel.set_zoom(120.0);
        panel.scroll_x_beats = Beats(2.0);

        let marker = UiMarker::new(Beats::from_f32(6.0));
        let marker_id = marker.id.clone();
        panel.set_markers(vec![marker]);

        // The hit rect must be centered on the drawn pixel, at the ruler top.
        let rect = panel.marker_flag_rect(Beats::from_f32(6.0));
        let cx = rect.x + rect.width * 0.5;
        let cy = rect.y + rect.height * 0.5;
        let px = panel.beat_to_pixel(Beats::from_f32(6.0));
        assert!((cx - px).abs() < 0.001);
        assert!((rect.y - panel.ruler_rect.y).abs() < 0.001);

        // A click at the flag centre hits; far away misses. No parallel rect
        // list — this proves the click area equals the drawn geometry.
        assert_eq!(
            panel.hit_test_marker_flag(Vec2::new(cx, cy)),
            Some(marker_id)
        );
        assert_eq!(
            panel.hit_test_marker_flag(Vec2::new(cx + 200.0, cy)),
            None
        );
    }

    #[test]
    fn hit_test_clip_delegates_to_shared_hit_tester() {
        // Hover now routes through the same `ClipHitTester` the click/drag path uses,
        // so the two agree on trim zones. This checks the coordinate wiring (screen Y →
        // scroll-adjusted track-content Y, screen X → beat) lands the right region.
        let mut panel = TimelineViewportPanel::new();
        panel.tracks_rect = Rect::new(0.0, 100.0, 1000.0, 600.0);
        panel.set_zoom(100.0); // 100 px/beat
        panel.set_tracks(vec![TrackInfo::default(), TrackInfo::default()]);
        panel.set_clips(vec![ViewportClip {
            clip_id: "c1".into(),
            layer_index: 0,
            start_beat: Beats::from_f32(0.0),
            duration_beats: Beats(4.0), // 400px wide → 8px proportional trim handles
            name: "".into(),
            color: color::CLIP_NORMAL,
            is_muted: false,
            is_locked: false,
            is_generator: false,
            is_audio: false,
            waveform: None,
            in_point_seconds: 0.0,
            warped_secs_per_beat: 0.0,
        }]);
        panel.mapper.set_layout(&[140.0, 140.0]); // two 140px layers

        // Screen Y inside layer-0's clip body band (content-y 70, pad is 12).
        let body_y = panel.tracks_rect.y + 70.0;

        let body = panel
            .hit_test_clip(Vec2::new(panel.beat_to_pixel(Beats::from_f32(2.0)), body_y))
            .expect("body hit");
        assert_eq!(body.clip_id, "c1");
        assert_eq!(body.region, HitRegion::Body);

        // 2px into a 400px clip → inside the 8px proportional trim handle.
        let trim = panel
            .hit_test_clip(Vec2::new(panel.beat_to_pixel(Beats::from_f32(0.02)), body_y))
            .expect("trim-left hit");
        assert_eq!(trim.region, HitRegion::TrimLeft);

        // With vertical scroll, the content-space conversion must add scroll_y_px —
        // exactly as the click/drag path does — or hover and click disagree again.
        // Set the field directly (set_scroll would clamp to 0 here).
        panel.scroll_y_px = 60.0;
        let scrolled = panel
            .hit_test_clip(Vec2::new(
                panel.beat_to_pixel(Beats::from_f32(2.0)),
                panel.tracks_rect.y + 10.0, // +10 screen, +60 scroll → content-y 70
            ))
            .expect("body hit under scroll");
        assert_eq!(scrolled.clip_id, "c1");
        assert_eq!(scrolled.region, HitRegion::Body);
    }

    #[test]
    fn hit_test_clip_skips_group_layers() {
        // Regression for the divergence bug: the old hover hit-tester did NOT skip
        // group layers, so a clip on a group track would hover even though the
        // click/drag path ignored it. Now both skip groups.
        let mut panel = TimelineViewportPanel::new();
        panel.tracks_rect = Rect::new(0.0, 100.0, 1000.0, 600.0);
        panel.set_zoom(100.0);
        panel.set_tracks(vec![TrackInfo {
            is_group: true,
            ..Default::default()
        }]);
        panel.set_clips(vec![ViewportClip {
            clip_id: "g1".into(),
            layer_index: 0,
            start_beat: Beats::from_f32(0.0),
            duration_beats: Beats(4.0),
            name: "".into(),
            color: color::CLIP_NORMAL,
            is_muted: false,
            is_locked: false,
            is_generator: false,
            is_audio: false,
            waveform: None,
            in_point_seconds: 0.0,
            warped_secs_per_beat: 0.0,
        }]);
        panel.mapper.set_layout(&[140.0]);

        let body_y = panel.tracks_rect.y + 70.0;
        assert!(
            panel
                .hit_test_clip(Vec2::new(panel.beat_to_pixel(Beats::from_f32(2.0)), body_y))
                .is_none(),
            "a clip on a group layer must not be hit — group layers are skipped",
        );
    }

    #[test]
    fn zeroed_scrollbar_rect_yields_no_layout() {
        // §24 5e-C regression: a build that early-returns (collapsed timeline)
        // zeroes `scrollbar_h_rect`, and a zero strip must produce neither a
        // drawable nor a grabbable scrollbar — even with overflow content.
        let mut panel = TimelineViewportPanel::new();
        panel.set_zoom(120.0);
        panel.tracks_rect = Rect::new(0.0, 0.0, 1000.0, 500.0);
        panel.set_tracks(vec![TrackInfo::default()]);
        panel.set_clips(vec![ViewportClip {
            clip_id: "far".into(),
            layer_index: 0,
            start_beat: Beats::from_f32(1000.0),
            duration_beats: Beats(4.0),
            name: "Far".into(),
            color: color::CLIP_NORMAL,
            is_muted: false,
            is_locked: false,
            is_generator: false,
            is_audio: false,
            waveform: None,
            in_point_seconds: 0.0,
            warped_secs_per_beat: 0.0,
        }]);

        // A real strip + overflow content → the scrollbar shows.
        panel.scrollbar_h_rect = Rect::new(0.0, 500.0, 1000.0, 11.0);
        assert!(
            panel.scrollbar_h_layout().is_some(),
            "overflow content should show a scrollbar with a real strip",
        );

        // The strip a collapsed build leaves behind (Rect::ZERO) → no scrollbar.
        panel.scrollbar_h_rect = Rect::ZERO;
        assert!(
            panel.scrollbar_h_layout().is_none(),
            "a zeroed strip must leave no drawable/grabbable scrollbar",
        );
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
                node_id: NodeId::PLACEHOLDER,
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
                node_id: NodeId::PLACEHOLDER,
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
                node_id: NodeId::PLACEHOLDER,
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
            is_audio: false,
            waveform: None,
            in_point_seconds: 0.0,
            warped_secs_per_beat: 0.0,
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
