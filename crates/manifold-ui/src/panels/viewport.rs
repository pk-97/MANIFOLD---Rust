use super::{Panel, PanelAction};
use crate::bitmap_painter;
use crate::color;
use crate::coordinate_mapper::CoordinateMapper;
use crate::drag::DragController;
use crate::input::UIEvent;
use crate::layout::ScreenLayout;
use crate::node::*;
use crate::snap;
use crate::tree::UITree;
use crate::view::{UiAutomationLane, UiMarker};
use manifold_foundation::{Beats, ClipId, EffectId, LayerId, MarkerId, ParamId};

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
pub use model::{
    AutomationDotScreen, AutomationLaneScreen, ClipScreenRect, ClipZones, SelectionRegion,
    TimelineOverlays, TrackInfo, ViewportAutomationLane, ViewportClip, clip_zones,
};
use coordinate::GridSubdivision;
use model::{CollapsedGroupBitmap, MarkerNodeGroup, TrackBgGroup};
// `zone_widths` is the trim-rule core `clip_zones` wraps — `clip_hit_tester`
// (a sibling top-level module, not a `viewport` submodule) needs the exact
// same rule for its local-pixel-space hit test, so it's re-exported at
// crate visibility rather than duplicated (D4: one geometry authority).
pub(crate) use model::zone_widths;

// ── TimelineViewportPanel ───────────────────────────────────────

pub struct TimelineViewportPanel {
    // Shared coordinate mapper (owns zoom, Y-layout, grid snapping).
    // The viewport adds screen-space offset (tracks_rect.x) on top.
    mapper: CoordinateMapper,

    // Viewport-specific scroll state (in beats, not pixels)
    scroll_x_beats: Beats,
    scroll_y_px: f32,
    beats_per_bar: u32,

    // BUG-159: last horizontal scroll gesture the user drove directly (wheel,
    // trackpad pan, scrollbar drag) — playhead-follow auto-scroll
    // (`check_auto_scroll`, manifold-app's state_sync.rs) yields to a recent
    // gesture instead of fighting it. `None` = no gesture yet this session.
    last_user_scroll_x: Option<std::time::Instant>,

    // Layer IDs (kept in sync with project layers)
    pub layer_ids: Vec<LayerId>,

    // Per-track style/state (heights + Y come from `mapper`, the sole authority)
    tracks: Vec<TrackInfo>,

    // Zebra stripe parity per track, counted over VISIBLE rows only. Hidden rows
    // (collapsed group children, height 0) must not consume a stripe step or the
    // alternation breaks for every lane below them. Rebuilt in `set_tracks`.
    track_zebra_even: Vec<bool>,

    // Clip data — single storage, bucketed by layer index.
    // Access all clips via clips_by_layer.iter().flatten().
    clips_by_layer: Vec<Vec<ViewportClip>>,

    // Automation lane data (P4, `docs/AUTOMATION_LANES_DESIGN.md` §7) — single
    // storage bucketed by layer index, mirroring `clips_by_layer`. Populated
    // by `set_automation_lanes` only when automation mode is visible (the
    // translator gates it); empty otherwise, so this panel never needs to
    // know the mode flag itself.
    automation_lanes_by_layer: Vec<Vec<UiAutomationLane>>,

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
    drag: DragController<ViewportDrag>,
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

    // B13 — live position/length readout for the clip currently being
    // moved/trimmed: (position, duration, layer_index). `None` outside a
    // move/trim gesture. Set once per frame by app_render.rs from the
    // overlay's drag state (`set_drag_readout`); display-only, no styling
    // (deferred to UI_CRAFT_AND_MOTION per `TIMELINE_INTERACTION_P1_SPEC.md`
    // B13/§6.9).
    drag_readout: Option<(Beats, Beats, usize)>,
    /// Dirty-checked format cache — house pattern is `TransportDisplayCache`
    /// (`manifold-app/src/ui_bridge/state_sync.rs`): `format!` runs only
    /// when the readout's values actually change, never per frame just
    /// because a gesture is in flight.
    drag_readout_cache: DragReadoutCache,
    drag_readout_label_id: Option<NodeId>,

    /// View snapshot captured by `Z` (zoom-to-selection, B14) so `Shift+Z`
    /// can restore it. One level, not a stack — matches the doc's "zoom-back"
    /// wording, not a full zoom history. `docs/TIMELINE_INTERACTION_P1_SPEC.md`
    /// §5 P1.6.
    zoom_back: Option<(f32, Beats, f32)>,

    // ── P2 motion (`UI_CRAFT_AND_MOTION_PLAN.md` D17 "timeline marquee
    // fade") ──────────────────────────────────────────────────────────
    /// Eases the region/marquee highlight's alpha in/out. Targets 1.0
    /// whenever `selection_region` is `Some` (whether still being dragged
    /// or already a settled selection) and 0.0 once cleared — NOT tied to
    /// drag state the way `graph_canvas`'s ad-hoc marquee is: unlike that
    /// one, a timeline region is a persistent selection concept that
    /// outlives the drag, so re-targeting this on drag-end would fade a
    /// still-selected region to invisible right after you finish selecting
    /// it. Ticked in `update` (the existing per-frame `Panel::update` hook).
    region_alpha: crate::anim::AnimF32,
    /// Wall-clock timestamp `update()` last ticked `region_alpha` from —
    /// mirrors `DropdownPanel::last_tick`.
    region_alpha_last_tick: Option<std::time::Instant>,

    /// D17 "clip split flick": the two just-split clip ids separate by
    /// ~1px briefly, a one-shot hump (out then back) rather than a
    /// decaying shake. `None` when idle.
    split_flick: Option<SplitFlickState>,
    /// Wall-clock timestamp `update()` last ticked `split_flick` from.
    split_flick_last_tick: Option<std::time::Instant>,
}

/// D17 "clip split flick" state — which two clip ids are separating, and
/// how far through the one-shot hump. Purely visual (the split itself
/// already committed via `SplitClipCommand` by the time this fires).
struct SplitFlickState {
    left_id: ClipId,
    right_id: ClipId,
    flick: crate::anim::Transient,
}

/// Viewport-local drag payload (P7.6, D8/D12) — `DragController<ViewportDrag>`
/// replaces the old `ViewportDragMode` discriminant + the parallel
/// `marker_drag_id`/`marker_drag_start_beat` fields (folded into
/// `MarkerDrag`'s payload) and `scrollbar_grab_dx` (folded into
/// `ScrollbarHDrag`'s payload — it was only ever read during a scrollbar
/// drag's continuation; the plain-Click path that also computed it never
/// needed to persist it). Only tracks ruler/overview/marker/scrollbar drags
/// — all clip interaction (move, trim, region) is handled by
/// `InteractionOverlay`.
#[derive(Debug, Clone, PartialEq)]
enum ViewportDrag {
    RulerScrub,
    OverviewScrub,
    MarkerDrag { marker_id: MarkerId, start_beat: Beats },
    /// Pointer-to-thumb-left offset captured at drag start, so the thumb
    /// tracks the pointer 1:1 instead of snapping its left edge under the
    /// cursor.
    ScrollbarHDrag { grab_dx: f32 },
}

/// B13 — dirty-checked bars.beats formatter for the live drag/trim readout.
/// House pattern: `TransportDisplayCache` (`manifold-app/src/ui_bridge/state_sync.rs`) —
/// `format!` runs only when the underlying values changed since the last call,
/// never on every frame just because a gesture happens to be in flight.
#[derive(Default)]
struct DragReadoutCache {
    prev_position: Option<Beats>,
    prev_duration: Option<Beats>,
    prev_bpb: u32,
    text: String,
}

impl DragReadoutCache {
    /// Returns the formatted "bar.beat   len bar.beat" string, reformatting
    /// only when `position`, `duration`, or `beats_per_bar` differ from the
    /// last call.
    fn text(&mut self, position: Beats, duration: Beats, beats_per_bar: u32) -> &str {
        if self.prev_position != Some(position)
            || self.prev_duration != Some(duration)
            || self.prev_bpb != beats_per_bar
        {
            self.prev_position = Some(position);
            self.prev_duration = Some(duration);
            self.prev_bpb = beats_per_bar;
            let (pos_bar, pos_beat) = bars_beats(position, beats_per_bar, 1.0);
            let (dur_bar, dur_beat) = bars_beats(duration, beats_per_bar, 0.0);
            self.text = format!("{pos_bar}.{pos_beat:.2}   len {dur_bar}.{dur_beat:.2}");
        }
        &self.text
    }
}

/// Beats → (bar, beat-in-bar) at the given time signature. `origin` is `1.0`
/// for a position (musician's 1-based bar.beat) or `0.0` for a span/duration
/// (a length has no "first bar" to be 1-based about).
fn bars_beats(beat: Beats, beats_per_bar: u32, origin: f32) -> (i64, f32) {
    let bpb = beats_per_bar.max(1) as f64;
    let b = beat.0.max(0.0);
    let bar = (b / bpb).floor() as i64 + origin as i64;
    let beat_in_bar = (b % bpb) as f32 + origin;
    (bar, beat_in_bar)
}

impl TimelineViewportPanel {
    pub fn new() -> Self {
        Self {
            mapper: CoordinateMapper::new(),
            scroll_x_beats: Beats::ZERO,
            scroll_y_px: 0.0,
            beats_per_bar: 4,
            last_user_scroll_x: None,
            layer_ids: Vec::new(),
            tracks: Vec::new(),
            track_zebra_even: Vec::new(),
            clips_by_layer: Vec::new(),
            automation_lanes_by_layer: Vec::new(),
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
            drag: DragController::new(),
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
            drag_readout: None,
            drag_readout_cache: DragReadoutCache::default(),
            drag_readout_label_id: None,
            zoom_back: None,
            region_alpha: crate::anim::AnimF32::new(0.0, color::MOTION_FAST_MS),
            region_alpha_last_tick: None,
            split_flick: None,
            split_flick_last_tick: None,
        }
    }

    /// Fire the D17 "clip split flick" for a just-completed split — call
    /// right after `EditingService::split_clip_at_beat` returns a command,
    /// with the original clip's id and `SplitClipCommand::tail_clip_id()`.
    pub fn fire_split_flick(&mut self, left_id: ClipId, right_id: ClipId) {
        let mut flick = crate::anim::Transient::default();
        flick.fire(color::MOTION_MED_MS);
        self.split_flick = Some(SplitFlickState { left_id, right_id, flick });
        self.split_flick_last_tick = None;
    }

    /// Advance the split-flick hump by real elapsed wall-clock time; drops
    /// the state once finished. Called from `update()`.
    fn tick_split_flick(&mut self) {
        let Some(state) = self.split_flick.as_mut() else {
            return;
        };
        let now = std::time::Instant::now();
        let dt_ms = self
            .split_flick_last_tick
            .map(|t| (now - t).as_secs_f32() * 1000.0)
            .unwrap_or(0.0)
            .min(100.0);
        self.split_flick_last_tick = Some(now);
        if !state.flick.tick(dt_ms) {
            self.split_flick = None;
        }
    }

    /// The X offset (screen px) `clip_id` should draw at for the split
    /// flick — `+` for the left half separating leftward, `-` for the
    /// right half separating rightward, a one-shot hump via `sin(pi * t)`
    /// (out then back, not a decaying oscillation like the error shake).
    /// `0.0` when idle or `clip_id` isn't one of the two split halves.
    pub fn split_flick_offset(&self, clip_id: &ClipId) -> f32 {
        const AMPLITUDE_PX: f32 = 1.0;
        let Some(state) = &self.split_flick else {
            return 0.0;
        };
        let Some(p) = state.flick.progress() else {
            return 0.0;
        };
        let hump = (p * std::f32::consts::PI).sin() * AMPLITUDE_PX;
        if *clip_id == state.left_id {
            -hump
        } else if *clip_id == state.right_id {
            hump
        } else {
            0.0
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
        // Zebra parity is counted over visible rows only — a hidden row (height 0,
        // i.e. a collapsed group's child) keeps the same parity so the lanes below
        // it stay correctly alternating.
        self.track_zebra_even.clear();
        let mut visible_row = 0usize;
        for i in 0..self.tracks.len() {
            let height = self.mapper.get_layer_height(i);
            if height <= 0.0 {
                self.bitmap_renderers.push(None);
                // Hidden: inherit the next visible row's parity (don't advance).
                self.track_zebra_even.push(visible_row.is_multiple_of(2));
            } else {
                self.bitmap_renderers
                    .push(Some(crate::bitmap_renderer::LayerBitmapRenderer::new(
                        i,
                        self.render_scale,
                        height,
                    )));
                self.track_zebra_even.push(visible_row.is_multiple_of(2));
                visible_row += 1;
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
    ///
    /// Re-clamps the scroll position against the new `total_content_height`
    /// immediately (`docs/TIMELINE_LAYOUT_P0_SPEC.md` D3): a collapse/delete
    /// that shrinks content must move the scroll position in the same frame,
    /// not wait for the next explicit scroll event. Since the header panel
    /// reads this same `scroll_y_px` at draw time (D2), both columns move
    /// together.
    pub fn rebuild_mapper_layout(&mut self, layers: &[crate::view::UiLayer]) {
        self.mapper.rebuild_y_layout(layers);
        self.set_scroll(self.scroll_x_beats.as_f32(), self.scroll_y_px);
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

    /// Replace the automation lane data, bucketed by layer index — mirrors
    /// `set_clips`'s clear-and-refill so repeated calls don't reallocate the
    /// inner `Vec`s. Called with an empty `lanes` whenever automation mode is
    /// off (the translator gates population, not this panel) — see
    /// `docs/AUTOMATION_LANES_DESIGN.md` §7.
    pub fn set_automation_lanes(&mut self, lanes: Vec<ViewportAutomationLane>) {
        for v in &mut self.automation_lanes_by_layer {
            v.clear();
        }
        self.automation_lanes_by_layer
            .resize_with(self.tracks.len(), Vec::new);
        for entry in lanes {
            if entry.layer_index < self.automation_lanes_by_layer.len() {
                self.automation_lanes_by_layer[entry.layer_index].push(entry.lane);
            }
        }
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
                // Sub-pixel clips are clamped to a 1px hairline rather than
                // culled, so short trigger clips never vanish at far zoom.
                // Mirrors the overview strip's width clamp
                // (`viewport/render.rs:125`) and the collapsed-group
                // summary's per-clip clamp (`viewport/render.rs:298`). Only
                // clips fully outside the tracks rect are skipped; the GPU
                // scissor clamps partials at the edges.
                let w = self
                    .beat_duration_to_width(clip.duration_beats.as_f32())
                    .max(1.0);
                if x + w < tx0 || x > tx1 {
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
                    waveform_breakpoints: clip.waveform_breakpoints.clone(),
                });
            }
        }
    }

    /// D17 "timeline marquee fade": advance `region_alpha` toward 1.0/0.0 by
    /// real elapsed wall-clock time, targeted on whether a region currently
    /// exists (see `region_alpha`'s doc comment for why that's the right
    /// binding, not drag state). Called every frame from `Panel::update`.
    fn tick_region_alpha(&mut self) {
        self.region_alpha
            .set_target(if self.selection_region.is_some() { 1.0 } else { 0.0 });
        if !self.region_alpha.is_animating() {
            self.region_alpha_last_tick = None;
            return;
        }
        let now = std::time::Instant::now();
        let dt_ms = self
            .region_alpha_last_tick
            .map(|t| (now - t).as_secs_f32() * 1000.0)
            .unwrap_or(0.0)
            .min(100.0);
        self.region_alpha_last_tick = Some(now);
        self.region_alpha.tick(dt_ms);
    }

    /// Screen-space geometry for the timeline overlays that sit ON TOP of the
    /// clip bodies + waveforms: the marquee/region highlight, the insert cursor,
    /// and the beat markers. Since §24 5b these are GPU rects emitted in the
    /// overlay pass rather than baked into a per-layer bitmap.
    ///
    /// **D7 — the scissor is structural, not an opt-in caller convention.**
    /// This used to say "the caller scissors to the tracks rect" — an opt-in
    /// contract a call site could forget. It no longer is one: every lane-
    /// content draw (clip bodies, waveforms, thumbnails, this overlay, and any
    /// future drag chrome) must go through `UIRenderer::lane_content_scissor`,
    /// an RAII guard that pushes the tracks-rect clip on construction and pops
    /// it on drop, so a draw call cannot opt out — see
    /// `docs/TIMELINE_INTERACTION_P1_SPEC.md` D7. The two GPU content passes
    /// (clip waveforms, clip thumbnails) get the same non-opt-out property a
    /// different way: `tracks_rect` is a required parameter of their
    /// `render()`, not an optional one. `insert_cursor_layer` + `has_insert`
    /// come from the app (it owns the resolved cursor layer).
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
            // D17 "timeline marquee fade": scale the highlight's alpha by
            // the eased `region_alpha` instead of drawing it at full
            // strength the instant a region exists.
            let a = self.region_alpha.value().clamp(0.0, 1.0);
            let base = color::ACCENT_BLUE_SELECTION;
            let faded = color::with_alpha(base, (base.a as f32 * a) as u8);
            Some((Rect::new(x0, y0, x1 - x0, y1 - y0), faded))
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

    /// Screen-space geometry for every visible automation lane strip (P4,
    /// `docs/AUTOMATION_LANES_DESIGN.md` §7): one [`AutomationLaneScreen`] per
    /// lane, in the same "geometry here, GPU draw in manifold-renderer" split
    /// as [`Self::visible_clip_rects`] / [`Self::timeline_overlays`]. Empty
    /// whenever `set_automation_lanes` was last called with no data (i.e.
    /// automation mode is off — this panel never checks the flag itself).
    ///
    /// `latched` is `ContentState::automation_latched_params` — a lane whose
    /// `(effect_id, param_id)` appears there draws grayed instead of red.
    pub fn automation_lane_screens(
        &self,
        latched: &[(EffectId, ParamId)],
    ) -> Vec<AutomationLaneScreen> {
        let mut out = Vec::new();
        let tx0 = self.tracks_rect.x;
        let tx1 = self.tracks_rect.x_max();
        let ty0 = self.tracks_rect.y;
        let ty1 = self.tracks_rect.y + self.tracks_rect.height;
        let (min_beat, max_beat) = self.visible_beat_range();

        for (i, lanes) in self.automation_lanes_by_layer.iter().enumerate() {
            if lanes.is_empty() || self.is_group_layer(i) {
                continue;
            }
            let track_y = self.track_y(i);
            let track_h = self.track_height(i);
            if track_h <= 0.0 || track_y + track_h < ty0 || track_y > ty1 {
                continue;
            }
            // Lanes stack below the fixed base card, in the extra height
            // `CoordinateMapper::layer_height` reserved for them — the same
            // constant both sides use, so they cannot disagree.
            let base_h = color::TRACK_HEIGHT;
            for (idx, lane) in lanes.iter().enumerate() {
                let strip_y = track_y + base_h + idx as f32 * color::AUTOMATION_LANE_STRIP_HEIGHT;
                let strip_rect =
                    Rect::new(tx0, strip_y, (tx1 - tx0).max(0.0), color::AUTOMATION_LANE_STRIP_HEIGHT);
                let overridden = latched
                    .iter()
                    .any(|(eid, pid)| *eid == lane.effect_id && *pid == lane.param_id);

                // Sample the curve at a fixed screen-space step across the
                // visible range — smooth enough for a breakpoint line, cheap
                // enough per frame (mirrors the graph canvas wire's bezier
                // step count; typical scale is tens of lanes, not hundreds).
                const STEP_PX: f32 = 6.0;
                let mut polyline = Vec::new();
                let mut x = tx0;
                while x <= tx1 {
                    let beat = self.pixel_to_beat(x);
                    let norm = lane.value_at_norm(beat);
                    let y = strip_rect.y + strip_rect.height * (1.0 - norm);
                    polyline.push((x, y));
                    x += STEP_PX;
                }

                // Placeholder lanes (P5, §7 addendum) carry one synthetic
                // point at the param's current value so the flat-line
                // polyline above samples correctly, but it isn't a real
                // breakpoint yet — no dot, until the first click creates one.
                let dots = if lane.placeholder {
                    Vec::new()
                } else {
                    lane.points
                        .iter()
                        .filter(|p| {
                            let b = p.beat.as_f32();
                            b >= min_beat && b <= max_beat
                        })
                        .map(|p| {
                            let x = self.beat_to_pixel(p.beat);
                            let y = strip_rect.y + strip_rect.height * (1.0 - p.value_norm);
                            model::AutomationDotScreen {
                                x,
                                y,
                                beat: p.beat,
                                value_norm: p.value_norm,
                                shape: p.shape,
                            }
                        })
                        .collect()
                };

                out.push(AutomationLaneScreen {
                    strip_rect,
                    label: lane.label.clone(),
                    overridden,
                    polyline,
                    dots,
                    target: lane.target.clone(),
                    param_id: lane.param_id.clone(),
                    param_min: lane.param_min,
                    param_max: lane.param_max,
                    whole_numbers: lane.whole_numbers,
                });
            }
        }
        out
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
        matches!(self.drag.payload(), Some(ViewportDrag::ScrollbarHDrag { .. }))
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

    /// BUG-159: record that the USER (not playhead-follow) just drove a
    /// horizontal scroll — wheel/trackpad pan or a scrollbar drag. Call this
    /// alongside `set_scroll` from any input path the user directly controls;
    /// `check_auto_scroll` (manifold-app) yields while a gesture is recent
    /// (see [`Self::user_scroll_x_recent`]) instead of overwriting it.
    pub fn note_user_scroll_x(&mut self) {
        self.last_user_scroll_x = Some(std::time::Instant::now());
    }

    /// BUG-159: true while a user-driven horizontal scroll gesture happened
    /// within `grace` — the window during which playhead-follow yields
    /// instead of fighting the gesture. Re-engage is automatic and implicit:
    /// once `grace` elapses with no further user scroll, this returns to
    /// `false` and the very next `check_auto_scroll` call resumes following.
    pub fn user_scroll_x_recent(&self, grace: std::time::Duration) -> bool {
        self.last_user_scroll_x
            .is_some_and(|t| t.elapsed() < grace)
    }

    /// Edge zone width, in screen px, where a drag pointer triggers autoscroll (B11).
    const AUTOSCROLL_EDGE_PX: f32 = 32.0;
    /// Max scroll advance per call (one call per drag frame) at the edge itself —
    /// px on the vertical axis, and equivalent px (converted to beats via
    /// `pixels_per_beat`) on the horizontal axis. Scales down linearly to zero
    /// as the pointer moves away from the edge toward `AUTOSCROLL_EDGE_PX`.
    const AUTOSCROLL_MAX_PX_PER_FRAME: f32 = 14.0;

    /// Edge autoscroll during an in-flight move/trim/rubber-band drag (B11).
    /// Callers invoke this once per drag frame — from `on_drag`'s per-move-event
    /// path and from the stationary-pointer poll — BEFORE converting `pointer`
    /// to a beat/layer, so the same frame's gesture math already reflects the
    /// new scroll position: a parked pointer still advances the gesture as the
    /// content scrolls under it.
    ///
    /// Reuses the single P0 scroll owner (`scroll_x_beats`/`scroll_y_px`,
    /// clamped only inside `set_scroll`) — this is not a second offset, just a
    /// proximity-scaled `set_scroll` call. Zero per-frame allocations: every
    /// step below is scalar arithmetic over `Copy` types.
    pub fn autoscroll_edge(&mut self, pointer: Vec2) -> bool {
        let rect = self.tracks_rect;
        let edge = Self::AUTOSCROLL_EDGE_PX;
        let rate = Self::AUTOSCROLL_MAX_PX_PER_FRAME;

        let left_dist = pointer.x - rect.x;
        let right_dist = rect.x_max() - pointer.x;
        let dx_px = if left_dist < edge {
            -rate * ((edge - left_dist.max(0.0)) / edge).clamp(0.0, 1.0)
        } else if right_dist < edge {
            rate * ((edge - right_dist.max(0.0)) / edge).clamp(0.0, 1.0)
        } else {
            0.0
        };

        let top_dist = pointer.y - rect.y;
        let bottom_dist = rect.y_max() - pointer.y;
        let dy_px = if top_dist < edge {
            -rate * ((edge - top_dist.max(0.0)) / edge).clamp(0.0, 1.0)
        } else if bottom_dist < edge {
            rate * ((edge - bottom_dist.max(0.0)) / edge).clamp(0.0, 1.0)
        } else {
            0.0
        };

        if dx_px == 0.0 && dy_px == 0.0 {
            return false;
        }

        let ppb = self.mapper.pixels_per_beat();
        let new_x_beats = self.scroll_x_beats.as_f32() + if ppb > 0.0 { dx_px / ppb } else { 0.0 };
        let new_y_px = self.scroll_y_px + dy_px;
        self.set_scroll(new_x_beats, new_y_px)
    }

    /// B13 — set (or clear) the live position/length readout for the clip
    /// currently being moved/trimmed. Called once per frame by app_render.rs
    /// from the overlay's drag state; `None` outside a move/trim gesture
    /// (rubber-band has no single clip to report, matching the design doc).
    pub fn set_drag_readout(&mut self, readout: Option<(Beats, Beats, usize)>) {
        self.drag_readout = readout;
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
                let line_color = color::with_alpha(mc, color::MARKER_LINE_ALPHA);
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

    /// Capture the current zoom + scroll as the zoom-back target (B14 `Z`).
    /// Overwrites any prior snapshot — one level, not a stack.
    pub fn store_zoom_back(&mut self) {
        self.zoom_back = Some((self.pixels_per_beat(), self.scroll_x_beats(), self.scroll_y_px));
    }

    /// Take the stored zoom-back snapshot, if any (B14 `Shift+Z`). Consumes it —
    /// a second `Shift+Z` with no intervening `Z` is a no-op, matching "zoom-back"
    /// (restore the one prior view) rather than a navigable history.
    pub fn recall_zoom_back(&mut self) -> Option<(f32, Beats, f32)> {
        self.zoom_back.take()
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

        // B13 — live drag/trim readout (plain text, display-only)
        self.build_drag_readout(tree);

        // Playhead: unified overlay quad in app.rs (no UITree node needed)

        self.node_count = tree.count() - self.first_node;
    }

    fn update(&mut self, tree: &mut UITree) {
        self.sync_insert_cursor_ruler(tree);
        self.sync_active_track_lane(tree);
        self.tick_region_alpha();
        self.tick_split_flick();
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

    // ── B14 zoom-back (`docs/TIMELINE_INTERACTION_P1_SPEC.md` §5 P1.6) ──

    // ── P7.6 viewport-drag-fold pinning ─────────────────────────────────
    // `docs/UI_WIDGET_UNIFICATION_DESIGN.md` P7.6: prove ruler-scrub,
    // overview-scrub, marker-drag, and horizontal-scrollbar-drag survive the
    // fold of `ViewportDragMode` + `marker_drag_id`/`marker_drag_start_beat`
    // + `scrollbar_grab_dx` onto `DragController<ViewportDrag>`, driven
    // through the real `on_timeline_event` entry point.

    fn built_viewport() -> TimelineViewportPanel {
        let mut tree = UITree::new();
        let mut vp = TimelineViewportPanel::new();
        vp.set_tracks(vec![TrackInfo::default()]);
        vp.build(&mut tree, &test_layout());
        vp
    }

    fn drag_begin(origin: Vec2) -> UIEvent {
        UIEvent::DragBegin { node_id: None, pos: origin, origin, modifiers: Modifiers::NONE }
    }
    fn drag(pos: Vec2) -> UIEvent {
        UIEvent::Drag { node_id: None, pos, delta: Vec2::ZERO }
    }
    fn drag_end(pos: Vec2) -> UIEvent {
        UIEvent::DragEnd { node_id: None, pos }
    }

    #[test]
    fn ruler_scrub_begin_drag_end_emits_seek_each_step() {
        let mut vp = built_viewport();
        let r = vp.ruler_rect;
        let origin = Vec2::new(r.x + 20.0, r.y + r.height * 0.5);

        let began = vp.on_timeline_event(&drag_begin(origin));
        assert!(matches!(began.as_slice(), [PanelAction::Seek(_)]), "ruler drag-begin must Seek");

        let moved = vp.on_timeline_event(&drag(Vec2::new(r.x + 80.0, origin.y)));
        assert!(matches!(moved.as_slice(), [PanelAction::Seek(_)]), "ruler drag continuation must Seek");

        let ended = vp.on_timeline_event(&drag_end(Vec2::new(r.x + 80.0, origin.y)));
        assert!(ended.is_empty(), "ruler scrub end emits nothing further");

        // A drag AFTER end must not still be routed as a ruler scrub (the
        // controller must have gone idle).
        let stray = vp.on_timeline_event(&drag(Vec2::new(r.x + 120.0, origin.y)));
        assert!(stray.is_empty(), "no drag session should remain armed after DragEnd");
    }

    #[test]
    fn overview_scrub_begin_and_drag_emit_normalized_position() {
        let mut vp = built_viewport();
        let ov = vp.overview_rect;
        let origin = Vec2::new(ov.x + 5.0, ov.y + ov.height * 0.5);

        let began = vp.on_timeline_event(&drag_begin(origin));
        assert!(matches!(began.as_slice(), [PanelAction::OverviewScrub(_)]));

        let moved = vp.on_timeline_event(&drag(Vec2::new(ov.x + ov.width * 0.5, origin.y)));
        match moved.as_slice() {
            [PanelAction::OverviewScrub(norm)] => {
                assert!((0.4..0.6).contains(norm), "midpoint drag should read ~0.5, got {norm}");
            }
            other => panic!("expected OverviewScrub, got {other:?}"),
        }
        vp.on_timeline_event(&drag_end(origin));
    }

    #[test]
    fn marker_drag_begin_track_end_round_trips_the_grabbed_marker_id() {
        let mut vp = built_viewport();
        vp.set_markers(vec![UiMarker { id: MarkerId::new("m1"), ..UiMarker::new(Beats::from_f32(4.0)) }]);
        let flag = vp.marker_flag_rect(Beats::from_f32(4.0));
        let origin = Vec2::new(flag.x + flag.width * 0.5, flag.y + flag.height * 0.5);

        let began = vp.on_timeline_event(&drag_begin(origin));
        match began.as_slice() {
            [PanelAction::MarkerDragStarted(id)] => assert_eq!(id, "m1"),
            other => panic!("expected MarkerDragStarted, got {other:?}"),
        }

        let moved = vp.on_timeline_event(&drag(Vec2::new(origin.x + 40.0, origin.y)));
        match moved.as_slice() {
            [PanelAction::MarkerDragMoved(id, beat)] => {
                assert_eq!(id, "m1");
                assert!(*beat > 4.0, "dragging right must move the marker later");
            }
            other => panic!("expected MarkerDragMoved, got {other:?}"),
        }

        let ended = vp.on_timeline_event(&drag_end(Vec2::new(origin.x + 40.0, origin.y)));
        match ended.as_slice() {
            [PanelAction::MarkerDragEnded(id, _)] => assert_eq!(id, "m1"),
            other => panic!("expected MarkerDragEnded, got {other:?}"),
        }
    }

    #[test]
    fn scrollbar_h_drag_tracks_the_grab_offset_and_reports_dragging() {
        let mut vp = built_viewport();
        // Force real content to scroll: a clip far out past the visible
        // range (`max_content_beat`) plus a tight zoom so the timeline's
        // content is wider than the viewport — an unscrollable thumb
        // produces no drag target (`scrollbar_h_layout` returns `None`).
        vp.set_clips(vec![ViewportClip {
            clip_id: "far".into(),
            layer_index: 0,
            start_beat: Beats::from_f32(500.0),
            duration_beats: Beats::from_f32(4.0),
            name: "".into(),
            color: color::CLIP_NORMAL,
            is_muted: false,
            is_locked: false,
            is_generator: false,
            is_audio: false,
            waveform: None,
            in_point_seconds: 0.0,
            waveform_breakpoints: Vec::new(),
        }]);
        vp.set_zoom(400.0);
        let sb = vp.scrollbar_h_rect;
        assert!(sb.width > 0.0 && sb.height > 0.0, "scrollbar strip must be laid out");
        let origin = Vec2::new(sb.x + sb.width * 0.1, sb.y + sb.height * 0.5);

        assert!(!vp.scrollbar_h_dragging());
        let began = vp.on_timeline_event(&drag_begin(origin));
        assert!(!began.is_empty(), "a scrollbar drag-begin over a scrollable thumb must emit a scroll action");
        assert!(vp.scrollbar_h_dragging(), "scrollbar_h_dragging() must reflect the live session");

        let moved = vp.on_timeline_event(&drag(Vec2::new(sb.x + sb.width * 0.5, origin.y)));
        assert!(matches!(moved.as_slice(), [PanelAction::TimelineScrollbarH(_)]));

        vp.on_timeline_event(&drag_end(Vec2::new(sb.x + sb.width * 0.5, origin.y)));
        assert!(!vp.scrollbar_h_dragging(), "drag-end must clear the session");
    }

    #[test]
    fn user_scroll_x_recent_reflects_a_note_then_expires() {
        // BUG-159: no gesture yet — never recent, at any grace window.
        let mut vp = built_viewport();
        assert!(!vp.user_scroll_x_recent(std::time::Duration::from_secs(10)));

        vp.note_user_scroll_x();
        assert!(
            vp.user_scroll_x_recent(std::time::Duration::from_millis(800)),
            "a just-noted gesture must be recent under a normal grace window"
        );
        // A zero-length grace window means the gesture (however fresh) is
        // already outside it — the boundary condition the impl must honor.
        assert!(!vp.user_scroll_x_recent(std::time::Duration::from_secs(0)));
    }

    #[test]
    fn zoom_back_restores_captured_view() {
        // scroll_y left at 0.0 — B14's `zoom_to_selection` never sets a
        // non-zero y (same precedent as the existing `zoom_to_fit`), and
        // `set_scroll`'s vertical clamp depends on track-height state this
        // test doesn't configure; the round-trip under test is store/recall,
        // not the clamp.
        let mut vp = TimelineViewportPanel::new();
        vp.set_zoom(50.0);
        vp.set_scroll(12.0, 0.0);

        vp.store_zoom_back();

        // Simulate `Z` changing the view.
        vp.set_zoom(150.0);
        vp.set_scroll(40.0, 0.0);
        assert_eq!(vp.pixels_per_beat(), 150.0);

        let recalled = vp.recall_zoom_back();
        assert!(recalled.is_some());
        let (ppb, scroll_x, scroll_y) = recalled.unwrap();
        assert_eq!(ppb, 50.0);
        assert_eq!(scroll_x, Beats::from_f32(12.0));
        assert_eq!(scroll_y, 0.0);
    }

    #[test]
    fn zoom_back_is_one_level_not_a_stack() {
        let mut vp = TimelineViewportPanel::new();
        vp.store_zoom_back();
        assert!(vp.recall_zoom_back().is_some());
        // A second recall with no intervening store is a no-op.
        assert!(vp.recall_zoom_back().is_none());
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
                waveform_breakpoints: Vec::new(),
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
                waveform_breakpoints: Vec::new(),
            },
        ]
    }

    #[test]
    fn focused_lane_uses_dedicated_selection_color() {
        let mut panel = TimelineViewportPanel::new();
        panel.set_tracks(test_tracks()); // two unmuted default tracks
        // No focus → both lanes sit at their resting zebra stripe.
        assert_eq!(panel.track_bg_color(0), crate::color::TRACK_BG);
        assert_eq!(panel.track_bg_color(1), crate::color::TRACK_BG_ALT);
        // Focus lane 0 → it takes the dedicated selection colour; the other lane
        // is untouched.
        panel.set_active_track_index(Some(0));
        assert_eq!(panel.track_bg_color(0), crate::color::TRACK_BG_SELECTED);
        assert_eq!(panel.track_bg_color(1), crate::color::TRACK_BG_ALT);
        // Clearing focus returns the lane to its resting stripe.
        panel.set_active_track_index(None);
        assert_eq!(panel.track_bg_color(0), crate::color::TRACK_BG);
    }

    #[test]
    fn zebra_parity_skips_hidden_rows() {
        // A collapsed group's child renders at height 0. It must NOT consume a
        // zebra step, or every lane below it flips to the wrong shade.
        let mut panel = TimelineViewportPanel::new();
        // Row 1 is hidden (height 0) — e.g. a collapsed group's child.
        panel.mapper.set_layout(&[140.0, 0.0, 140.0, 140.0]);
        panel.set_tracks(vec![
            TrackInfo::default(),
            TrackInfo::default(),
            TrackInfo::default(),
            TrackInfo::default(),
        ]);
        // Visible rows 0,2,3 alternate as 1st/2nd/3rd visible lane — the hidden
        // row 1 inherits row 2's parity rather than advancing it.
        assert_eq!(panel.track_bg_color(0), crate::color::TRACK_BG); // 1st visible
        assert_eq!(panel.track_bg_color(2), crate::color::TRACK_BG_ALT); // 2nd visible
        assert_eq!(panel.track_bg_color(3), crate::color::TRACK_BG); // 3rd visible
        // Raw-index parity would have made row 2 == TRACK_BG (index 2 is even),
        // i.e. the same shade as its visible neighbour above — the bug.
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
            waveform_breakpoints: Vec::new(),
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
            waveform_breakpoints: Vec::new(),
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
    fn visible_clip_rects_clamps_subpixel_clips_to_hairline_but_culls_offscreen() {
        // P0.3 (`docs/TIMELINE_LAYOUT_P0_SPEC.md`): at far zoom, a clip whose
        // pixel width rounds below 1px must still draw as a 1px hairline, not
        // vanish. Only clips fully outside the tracks rect are skipped.
        let mut panel = TimelineViewportPanel::new();
        panel.tracks_rect = Rect::new(0.0, 100.0, 1000.0, 600.0);
        panel.set_zoom(1.0); // far zoom: 1px/beat
        panel.set_tracks(vec![TrackInfo::default()]);
        panel.mapper.set_layout(&[140.0]);
        panel.set_clips(vec![
            // On-screen, sub-pixel duration (0.3 beats @ 1px/beat = 0.3px).
            ViewportClip {
                clip_id: "onscreen-subpixel".into(),
                layer_index: 0,
                start_beat: Beats::from_f32(10.0),
                duration_beats: Beats(0.3),
                name: "".into(),
                color: color::CLIP_NORMAL,
                is_muted: false,
                is_locked: false,
                is_generator: false,
                is_audio: false,
                waveform: None,
                in_point_seconds: 0.0,
                waveform_breakpoints: Vec::new(),
            },
            // Fully offscreen, well past the right edge of the 1000px tracks
            // rect — must still be culled even though its clamped width
            // would be onscreen-sized.
            ViewportClip {
                clip_id: "offscreen-subpixel".into(),
                layer_index: 0,
                start_beat: Beats::from_f32(5000.0),
                duration_beats: Beats(0.3),
                name: "".into(),
                color: color::CLIP_NORMAL,
                is_muted: false,
                is_locked: false,
                is_generator: false,
                is_audio: false,
                waveform: None,
                in_point_seconds: 0.0,
                waveform_breakpoints: Vec::new(),
            },
        ]);

        let mut out = Vec::new();
        panel.visible_clip_rects(&mut out);

        assert_eq!(out.len(), 1, "only the onscreen clip should survive the cull");
        let rect = &out[0];
        assert_eq!(rect.clip_id, "onscreen-subpixel");
        assert!(
            rect.rect.width >= 1.0,
            "sub-pixel clip must clamp to a 1px hairline, got {}",
            rect.rect.width
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
            waveform_breakpoints: Vec::new(),
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

    // P0.1 gate (D3): a collapse/delete that shrinks content must move the
    // scroll position in the same frame `rebuild_mapper_layout` runs, not
    // wait for the next explicit scroll event — see
    // `docs/TIMELINE_LAYOUT_P0_SPEC.md` D3 and RC1's before-evidence
    // (`docs/evidence/timeline_p0/before/README.md`, scene 06).
    #[test]
    fn rebuild_mapper_layout_reclamps_scroll_immediately() {
        use crate::types::LayerType;
        use crate::view::UiLayer;

        let mut panel = TimelineViewportPanel::new();
        panel.tracks_rect = Rect::new(0.0, 0.0, 1000.0, 300.0);

        let make_layers = |n: usize| -> Vec<UiLayer> {
            (0..n)
                .map(|i| UiLayer {
                    layer_id: LayerId::new(format!("L{i}")),
                    parent_layer_id: None,
                    layer_type: LayerType::Video,
                    is_collapsed: false,
                    automation_lane_count: 0,
                })
                .collect()
        };

        // 6 layers * TRACK_HEIGHT(200) = 1200 content height, viewport 300 →
        // max scroll 900. Scroll to the bottom.
        panel.rebuild_mapper_layout(&make_layers(6));
        panel.set_scroll(0.0, 900.0);
        assert!(
            (panel.scroll_y_px() - 900.0).abs() < 0.01,
            "sanity: scrolled to max"
        );

        // Shrink content the way a collapse/delete would — no explicit
        // `set_scroll` call in between, matching what `sync_project_data`
        // actually does (`rebuild_mapper_layout` is the only call).
        panel.rebuild_mapper_layout(&make_layers(2));

        // 2 * 200 = 400 content height, viewport 300 → max scroll 100. The
        // stale scroll_y_px (900) must already be clamped to 100 THIS call,
        // not left stale until the next user scroll (RC1's exact mechanism).
        assert!(
            (panel.scroll_y_px() - 100.0).abs() < 0.01,
            "scroll should re-clamp to the new max immediately: got {}",
            panel.scroll_y_px()
        );
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
            waveform_breakpoints: Vec::new(),
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

    // ── P2 motion (`UI_CRAFT_AND_MOTION_PLAN.md` D17 "timeline marquee fade") ──

    #[test]
    fn region_highlight_fades_in_when_a_region_appears() {
        let mut panel = TimelineViewportPanel::new();
        panel.set_tracks(test_tracks());
        let mut markers = Vec::new();

        // No region yet — no highlight at all.
        let overlays = panel.timeline_overlays(None, false, &mut markers);
        assert!(overlays.region.is_none());

        // A region appears; `tick_region_alpha` targets 1.0. Its own first
        // call only *establishes* the wall-clock baseline (real elapsed time
        // since the previous call is unknown, so it moves nothing that
        // instant — same contract `DropdownPanel::update` documents); drive
        // the actual partial-progress assertion with an explicit `dt` on the
        // underlying tween directly, mirroring how `DropdownPanel`'s own
        // tests split `tick_enter(dt)` out from wall-clock `update` for
        // exactly this reason.
        panel.set_selection_region(Some(SelectionRegion {
            start_beat: Beats::ZERO,
            end_beat: Beats(4.0),
            start_layer: 0,
            end_layer: 1,
        }));
        panel.tick_region_alpha();
        panel.region_alpha.tick(16.0);
        let overlays = panel.timeline_overlays(None, false, &mut markers);
        let (_, c) = overlays.region.expect("region now exists");
        assert!(
            c.a > 0 && c.a < color::ACCENT_BLUE_SELECTION.a,
            "first tick fades in partway, not instantly: alpha={}",
            c.a
        );

        // Drive it to fully settled.
        panel.tick_region_alpha();
        panel.region_alpha.tick(color::MOTION_FAST_MS);
        let overlays = panel.timeline_overlays(None, false, &mut markers);
        let (_, c) = overlays.region.expect("region still exists");
        assert_eq!(c.a, color::ACCENT_BLUE_SELECTION.a, "settles at full strength");
    }

    #[test]
    fn region_highlight_survives_drag_end_unfaded() {
        // The whole point of binding `region_alpha`'s target to region
        // PRESENCE rather than drag state (unlike `graph_canvas`'s ad-hoc
        // marquee): a settled selection must NOT fade away just because a
        // drag ended. Simulates "drag ended, region persists".
        let mut panel = TimelineViewportPanel::new();
        panel.set_tracks(test_tracks());
        panel.set_selection_region(Some(SelectionRegion {
            start_beat: Beats::ZERO,
            end_beat: Beats(4.0),
            start_layer: 0,
            end_layer: 1,
        }));
        // Settle it fully first (as if the fade-in already finished).
        for _ in 0..10 {
            panel.tick_region_alpha();
            panel.region_alpha.tick(color::MOTION_FAST_MS);
        }
        let mut markers = Vec::new();
        let (_, before) = panel
            .timeline_overlays(None, false, &mut markers)
            .region
            .expect("region exists");
        assert_eq!(before.a, color::ACCENT_BLUE_SELECTION.a);

        // "Drag ends" — nothing about the region itself changes; only a
        // real `clear_region()` (a different call) should ever fade it out.
        panel.tick_region_alpha();
        let (_, after) = panel
            .timeline_overlays(None, false, &mut markers)
            .region
            .expect("still exists after the drag ends");
        assert_eq!(after.a, before.a, "stays at full strength — no fade-out on drag-end alone");
    }

    // ── P2 motion (D17 "clip split flick") ──────────────────────────

    #[test]
    fn split_flick_separates_the_two_halves_oppositely_then_settles() {
        let mut panel = TimelineViewportPanel::new();
        let left = ClipId::new("left-half");
        let right = ClipId::new("right-half");
        let other = ClipId::new("unrelated-clip");

        assert_eq!(panel.split_flick_offset(&left), 0.0, "idle before firing");

        panel.fire_split_flick(left.clone(), right.clone());
        // Mid-hump (progress != 0/1): left and right move opposite ways by
        // the same magnitude; an unrelated clip id is untouched.
        panel.split_flick.as_mut().unwrap().flick.tick(color::MOTION_MED_MS * 0.5);
        let l = panel.split_flick_offset(&left);
        let r = panel.split_flick_offset(&right);
        assert!(l < 0.0, "left half separates leftward: {l}");
        assert!(r > 0.0, "right half separates rightward: {r}");
        assert!((l + r).abs() < 1e-4, "equal and opposite: {l} vs {r}");
        assert_eq!(panel.split_flick_offset(&other), 0.0, "unrelated clip untouched");

        // Past the full duration, the hump finishes and ticking drops it.
        panel.split_flick.as_mut().unwrap().flick.tick(color::MOTION_MED_MS);
        panel.tick_split_flick();
        assert_eq!(panel.split_flick_offset(&left), 0.0, "settles back to zero");
    }
}
