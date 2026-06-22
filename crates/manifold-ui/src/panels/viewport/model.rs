//! Timeline model — the addressable items the viewport paints and hit-tests.
//!
//! Lanes (`TrackInfo`), clips (`ViewportClip`), and markers (via
//! `manifold_core::marker::TimelineMarker`, stored on the panel) are the items
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
    pub name: String,
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
