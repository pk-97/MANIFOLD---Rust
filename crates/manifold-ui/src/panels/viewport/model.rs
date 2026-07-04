//! Timeline model — the addressable items the viewport paints and hit-tests.
//!
//! Lanes (`TrackInfo`), clips (`ViewportClip`), and markers (via
//! [`crate::view::UiMarker`], stored on the panel) are the items
//! the timeline addresses. A single source of each drives **both** the CPU
//! paint and the hit-test, so they cannot disagree. Track *height* lives only on
//! the `CoordinateMapper`; marker flag geometry lives only in `coordinate.rs`.
//! See `docs/TIMELINE_API_DESIGN.md` §3.2.

use super::*;

/// A clip to be rendered in the timeline viewport.
#[derive(Debug, Clone)]
pub struct ViewportClip {
    pub clip_id: ClipId,
    pub layer_index: usize,
    pub start_beat: Beats,
    pub duration_beats: Beats,
    /// `Arc<str>` (not `String`) so attaching the name to a `ClipScreenRect` each
    /// frame in `visible_clip_rects` is a refcount bump, not a heap allocation.
    pub name: std::sync::Arc<str>,
    pub color: Color32,
    pub is_muted: bool,
    pub is_locked: bool,
    pub is_generator: bool,
    /// An audio-layer clip. Renders distinctly (no video thumbnail); the hook
    /// for in-clip waveform painting. See `docs/AUDIO_LAYER_DESIGN.md`.
    pub is_audio: bool,
    /// Zoom-aware waveform renderer (MIP chain + spectral color) for the source
    /// file, shared with the audio-import lanes. `None` until the file is decoded
    /// in the background. Shared (`Arc`) so attaching it to a clip each sync is a
    /// cheap refcount bump, not a copy.
    pub waveform: Option<std::sync::Arc<crate::waveform_renderer::WaveformRenderer>>,
    /// Audio only: offset into the source file where this clip starts playing
    /// (seconds). The left edge of the waveform window. Ignored for non-audio.
    pub in_point_seconds: f32,
    /// Audio only: warped source-seconds per beat (`60 / clip_bpm` with warp on,
    /// `60 / project_bpm` with warp off). Times `duration_beats` gives the source
    /// window length, so the waveform scale is set by warp, not by trim.
    pub warped_secs_per_beat: f32,
}

/// A visible clip resolved to its on-screen rectangle and the style inputs the
/// GPU clip emitter needs (§24 5b). The viewport produces these every frame from
/// the same geometry the hit-tester uses, so the drawn body and the clickable
/// region cannot disagree. Selection / hover / marquee are resolved by the
/// caller (it owns the selection state); this carries only what the viewport
/// knows: geometry, the effective base colour, and the per-clip flags.
#[derive(Debug, Clone)]
pub struct ClipScreenRect {
    pub clip_id: ClipId,
    pub layer_index: usize,
    pub rect: Rect,
    /// Effective base colour (per-clip override resolved into `ViewportClip.color`).
    pub base_color: Color32,
    /// Display name, drawn as the clip's label strip in the overlay pass.
    /// `Arc<str>` shared from `ViewportClip` — cloned per frame as a refcount bump.
    pub name: std::sync::Arc<str>,
    pub start_beat: Beats,
    pub end_beat: Beats,
    pub is_muted: bool,
    pub is_locked: bool,
    pub is_generator: bool,
    /// Audio-layer clip — carries the waveform the GPU clip-content pass paints
    /// inside the body. Non-audio clips leave the waveform fields inert.
    pub is_audio: bool,
    /// Shared zoom-aware waveform renderer for the source file (`None` until the
    /// background decode finishes). Cloning is a refcount bump, not a copy.
    pub waveform: Option<std::sync::Arc<crate::waveform_renderer::WaveformRenderer>>,
    /// Audio only: source-file offset (seconds) where this clip starts — the left
    /// edge of the waveform window. Mirrors `ViewportClip::in_point_seconds`.
    pub in_point_seconds: f32,
    /// Audio only: warped source-seconds per beat. `× duration` gives the source
    /// window length (the waveform scale is set by warp, not trim).
    pub warped_secs_per_beat: f32,
}

// `HitRegion` and `ClipHitResult` live once in `crate::clip_hit_tester` — the
// single hit-tester both the hover and the click/drag paths use — and are
// re-exported from this module via `viewport.rs`. They were duplicated here,
// which let the two hit-test paths silently diverge (fixed- vs proportional-width
// trim handles, group-layer skip on one path only).

/// Screen-space geometry for the timeline overlays drawn on top of the clip
/// bodies + waveforms (§24 5b GPU rects, no longer baked into a bitmap). The
/// caller scissors to the tracks rect and draws these under the clip names.
/// `Copy` + allocation-free: the beat markers (variable count) are written into a
/// caller-owned scratch `Vec` instead of being boxed in here, so resolving the
/// overlays each frame allocates nothing.
#[derive(Debug, Clone, Copy, Default)]
pub struct TimelineOverlays {
    /// Marquee / region highlight: a translucent fill `(rect, colour)` over the
    /// contiguous selected beat × layer span. `None` when there is no region.
    pub region: Option<(Rect, Color32)>,
    /// Insert cursor: a thin vertical bar `(rect, colour)` on its target layer
    /// row. `None` when the cursor is inactive or has no resolved layer.
    pub cursor: Option<(Rect, Color32)>,
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
///
/// Track *height* is intentionally NOT a field here — it is owned solely by the
/// [`CoordinateMapper`] (`mapper.get_layer_height(i)`), the single Y-layout
/// authority. `TrackInfo` carries only the per-track *style/state* the renderer
/// needs. See `docs/TIMELINE_API_DESIGN.md` §3.4.
#[derive(Debug, Clone, Default)]
pub struct TrackInfo {
    pub is_muted: bool,
    pub is_group: bool,
    pub is_collapsed: bool,
    pub accent_color: Option<Color32>,
    /// For group layers: indices of child layers (used for collapsed group preview).
    /// From Unity ViewportManager.GenerateCollapsedGroupTexture.
    pub child_layer_indices: Vec<usize>,
}

/// One automation lane bucketed to its track row — mirrors `ViewportClip`'s
/// `layer_index` bucketing pattern (`docs/AUTOMATION_LANES_DESIGN.md` §7).
#[derive(Debug, Clone)]
pub struct ViewportAutomationLane {
    pub layer_index: usize,
    pub lane: UiAutomationLane,
}

/// Screen-space geometry for one automation lane strip, resolved against the
/// current Y-layout + beat→pixel mapping by
/// [`super::TimelineViewportPanel::automation_lane_screens`]. The renderer
/// (`manifold_renderer::automation_lane_draw`) draws these directly — no
/// UITree nodes, the same "GPU rects computed here, drawn there" split as
/// [`ClipScreenRect`] / [`TimelineOverlays`].
#[derive(Debug, Clone)]
pub struct AutomationLaneScreen {
    /// The strip's background band, full tracks width.
    pub strip_rect: Rect,
    pub label: String,
    /// True when this lane's param is currently latched/overridden — draws
    /// grayed instead of red (Live's affordance).
    pub overridden: bool,
    /// The sampled breakpoint line, screen-space `(x, y)` pairs in ascending
    /// x order — the caller draws consecutive segments with `draw_line`.
    pub polyline: Vec<(f32, f32)>,
    /// Breakpoint dot centers, screen-space, culled to the visible beat range.
    pub dots: Vec<(f32, f32)>,
}

// ── Marker node group for update-in-place ──────────────────────

/// Structured storage for one timeline marker's nodes.
/// Enables update-in-place by providing a 1:1 mapping between markers and their node IDs.
pub(crate) struct MarkerNodeGroup {
    pub(crate) flag_id: NodeId,
    /// Always built; hidden via set_visible when not selected.
    pub(crate) outline_id: NodeId,
    /// Always built; hidden via set_visible when the marker has no name.
    pub(crate) label_id: NodeId,
}

// ── Track background node group for update-in-place ────────────

/// Structured storage for one track's background nodes.
pub(crate) struct TrackBgGroup {
    pub(crate) bg_id: NodeId,
    pub(crate) accent_id: Option<NodeId>, // None if no accent bar
    pub(crate) separator_id: NodeId,
}

// ── Collapsed group bitmap ──────────────────────────────────────

/// CPU pixel buffer for a single collapsed group's clip preview.
pub(crate) struct CollapsedGroupBitmap {
    pub(crate) pixels: Vec<Color32>,
    pub(crate) tex_w: usize,
    pub(crate) tex_h: usize,
    pub(crate) dirty: bool,
    pub(crate) last_min_beat: f32,
    pub(crate) last_max_beat: f32,
    pub(crate) last_viewport_w: f32,
    pub(crate) last_track_h: f32,
    pub(crate) last_clip_count: usize,
}

impl CollapsedGroupBitmap {
    pub(crate) fn new() -> Self {
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
