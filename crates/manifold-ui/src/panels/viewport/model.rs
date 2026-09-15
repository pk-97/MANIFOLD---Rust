//! Timeline model — the addressable items the viewport paints and hit-tests.
//!
//! Lanes (`TrackInfo`), clips (`ViewportClip`), and markers (via
//! [`crate::view::UiMarker`], stored on the panel) are the items
//! the timeline addresses. A single source of each drives **both** the CPU
//! paint and the hit-test, so they cannot disagree. Track *height* lives only on
//! the `CoordinateMapper`; marker flag geometry lives only in `coordinate.rs`.
//! See `docs/TIMELINE_API_DESIGN.md` section 3.2.

use super::*;
use crate::view::{UiAutomationLane, UiSegmentShape};

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
    /// Audio only: piecewise beat→file-seconds breakpoints mirroring what
    /// playback actually does (`AudioLayerPlayback::update`, which integrates
    /// the project's tempo map — NOT a flat seconds-per-beat). Each entry is
    /// `(x_frac, file_secs)`: `x_frac` beat-linear in `[0, 1]` across the clip
    /// (matches pixel x), `file_secs` the source-file position for that beat.
    /// A constant-tempo clip has exactly 2 entries (start, end); a varying
    /// tempo map adds one entry per tempo-map point strictly inside the clip.
    /// Empty for non-audio clips. See `crates/manifold-app/src/ui_bridge/
    /// state_sync.rs::audio_waveform_breakpoints` (the producer).
    pub waveform_breakpoints: Vec<(f32, f32)>,
}

/// A visible clip resolved to its on-screen rectangle and the style inputs the
/// GPU clip emitter needs (section 24 5b). The viewport produces these every frame from
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
    /// Audio only: piecewise beat→file-seconds breakpoints. Mirrors
    /// `ViewportClip::waveform_breakpoints` — see its doc comment.
    pub waveform_breakpoints: Vec<(f32, f32)>,
}

// `HitRegion` and `ClipHitResult` live once in `crate::clip_hit_tester` — the
// single hit-tester both the hover and the click/drag paths use — and are
// re-exported from this module via `viewport.rs`. They were duplicated here,
// which let the two hit-test paths silently diverge (fixed- vs proportional-width
// trim handles, group-layer skip on one path only).

/// A clip's body/trim/label geometry, resolved once from [`ClipScreenRect`] so
/// the painter and the hit-tester read the exact same rects instead of each
/// computing trim width privately (`docs/TIMELINE_INTERACTION_P1_SPEC.md` D4 —
/// this is what killed the S4 "narrow clip has no trim zone" bug and the
/// cursor/hit-test disagreement it caused). `trim_left`/`trim_right` may
/// extend OUTSIDE `body` into neighboring empty lane space — that outward
/// reach is what makes a hairline-narrow clip still grabbable.
#[derive(Debug, Clone, Copy)]
pub struct ClipZones {
    /// == `ClipScreenRect.rect`.
    pub body: Rect,
    pub trim_left: Rect,
    pub trim_right: Rect,
    /// Name strip; selection chrome insets derive from `body`, not this.
    pub label: Rect,
}

/// Zone-width core, shared by [`clip_zones`] (screen-space, for painting) and
/// `clip_hit_tester::ClipHitTester::hit_test` (local-pixel-space, for hit
/// testing) so the trim rule has exactly one implementation. Values are
/// independent of any absolute position — callers place them relative to
/// whatever origin they're working in (screen x for the painter, clip-start-
/// relative local px for the hit-tester).
///
/// Rule (D4, transcribed verbatim — do not retune here):
/// inner trim width = `min(8, body_width / 3).max(2)`; each handle also
/// extends OUTWARD by `min(4, neighbor_gap)` px into empty lane space. When
/// two clips abut (gap 0) each side's outward extension is 0, so the shared
/// boundary point falls to exactly one clip's zone — an even split with no
/// gap and no overlap.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ZoneWidths {
    /// Inner trim width, both sides.
    pub inner: f32,
    /// Outward extension into the left neighbor's gap.
    pub left_extend: f32,
    /// Outward extension into the right neighbor's gap.
    pub right_extend: f32,
}

/// `neighbor_gaps`: px of empty lane to the left/right of this clip (0.0 when
/// abutting; pass a large value — e.g. `f32::MAX` — when there is no neighbor
/// on that side, so the outward extension is unclamped by a real gap).
pub(crate) fn zone_widths(body_width: f32, neighbor_gaps: (f32, f32)) -> ZoneWidths {
    ZoneWidths {
        inner: (body_width / 3.0).clamp(2.0, 8.0),
        left_extend: neighbor_gaps.0.clamp(0.0, 4.0),
        right_extend: neighbor_gaps.1.clamp(0.0, 4.0),
    }
}

/// The single geometry source for a clip's trim handles + label strip (D4).
/// `neighbor_gaps`: px of empty lane left/right of this clip (0.0 when
/// abutting). Both `ClipHitTester` and the cursor-affordance selection must
/// read zones derived from this same rule — see `zone_widths` — never retune
/// the constants at a call site.
pub fn clip_zones(rect: &ClipScreenRect, neighbor_gaps: (f32, f32)) -> ClipZones {
    let body = rect.rect;
    let zw = zone_widths(body.width, neighbor_gaps);
    let trim_left = Rect::new(
        body.x - zw.left_extend,
        body.y,
        zw.left_extend + zw.inner,
        body.height,
    );
    let trim_right = Rect::new(
        body.x + body.width - zw.inner,
        body.y,
        zw.inner + zw.right_extend,
        body.height,
    );
    // Name strip: mirrors `manifold_renderer::clip_draw::emit_clip_names`'s
    // bottom-anchored band (that fn lives in `manifold-renderer`, which
    // depends on `manifold-ui` — not the reverse — so this reads the same
    // `color` constants rather than importing the renderer's helper).
    let strip_h = if body.height >= color::CLIP_STRIP_MIN_CLIP_HEIGHT {
        color::CLIP_STRIP_HEIGHT.min(body.height * 0.45)
    } else {
        color::FONT_LABEL as f32 + 3.0
    }
    .min(body.height.max(0.0));
    let label = Rect::new(
        body.x + color::CLIP_LABEL_PAD_X,
        body.y + body.height - strip_h,
        (body.width - color::CLIP_LABEL_PAD_X * 2.0).max(0.0),
        strip_h,
    );
    ClipZones {
        body,
        trim_left,
        trim_right,
        label,
    }
}

/// Screen-space geometry for the timeline overlays drawn on top of the clip
/// bodies + waveforms (section 24 5b GPU rects, no longer baked into a bitmap). The
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
/// needs. See `docs/TIMELINE_API_DESIGN.md` section 3.4.
#[derive(Debug, Clone, Default)]
pub struct TrackInfo {
    pub is_muted: bool,
    pub is_group: bool,
    pub is_collapsed: bool,
    /// For group layers: indices of child layers (used for collapsed group preview).
    /// From Unity ViewportManager.GenerateCollapsedGroupTexture.
    pub child_layer_indices: Vec<usize>,
}

/// One automation lane bucketed to its track row — mirrors `ViewportClip`'s
/// `layer_index` bucketing pattern (`docs/AUTOMATION_LANES_DESIGN.md` section 7).
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
/// [`ClipScreenRect`] / [`TimelineOverlays`]. `InteractionOverlay`'s
/// automation hit-testing/editing also reads this same geometry (per
/// `docs/AUTOMATION_LANES_DESIGN.md` section 7's "click on the line adds a
/// breakpoint" vocabulary) — one source for both the draw and the click, so
/// they cannot disagree, the same discipline `ClipScreenRect` already keeps
/// for clip bodies.
#[derive(Debug, Clone)]
pub struct AutomationLaneScreen {
    /// The strip's background band, full tracks width.
    pub strip_rect: Rect,
    /// Scalar horizontal mapping copied from the viewport's shared mapper.
    /// Keeping these values with the lane lets the renderer draw the timing
    /// grid directly, without allocating a second list of grid vertices or
    /// re-deriving snap positions.
    pub pixels_per_beat: f32,
    pub visible_beat_start: f32,
    pub grid_step: f32,
    pub beats_per_bar: f32,
    pub bar_skip: u32,
    pub label: String,
    /// True when this lane's param is currently latched/overridden — draws
    /// grayed instead of red (Live's affordance).
    pub overridden: bool,
    /// The sampled breakpoint line, screen-space `(x, y)` pairs in ascending
    /// x order — the caller draws consecutive segments with `draw_line`.
    pub polyline: Vec<(f32, f32)>,
    /// Breakpoint dots, screen-space, culled to the visible beat range —
    /// carries each dot's beat/value/shape (not just its pixel position) so
    /// editing can identify + reconstruct the exact `AutomationPoint` a grab
    /// or delete targets, per [`AutomationDotScreen`].
    pub dots: Vec<AutomationDotScreen>,
    /// Addressing for this lane's edit commands
    /// (`AddAutomationPointCommand`/`MoveAutomationPointCommand`/
    /// `RemoveAutomationPointCommand`) — `Effect(EffectId)` or
    /// `Generator(LayerId)`, mirroring `manifold_core::GraphTarget`.
    pub target: crate::view::UiGraphTarget,
    pub param_id: ParamId,
    /// The param's resolved range — see `UiAutomationLane::param_min/max`'s
    /// doc for the footgun this exists to close (screen Y is normalized
    /// 0..1; `AutomationPoint.value` is param range).
    pub param_min: f32,
    pub param_max: f32,
    /// Whether this param is integral — new points click-added on it default
    /// to `Hold` instead of `Linear` (`docs/AUTOMATION_LANES_DESIGN.md` section 8).
    pub whole_numbers: bool,
}

impl AutomationLaneScreen {
    /// Chrome and graph share one geometry contract with input and rendering.
    pub fn header_rect(&self) -> Rect {
        Rect::new(
            self.strip_rect.x,
            self.strip_rect.y,
            self.strip_rect.width,
            if self.strip_rect.height >= 64.0 {
                22.0
            } else {
                0.0
            },
        )
    }

    pub fn curve_rect(&self) -> Rect {
        Self::curve_rect_for(self.strip_rect)
    }

    pub fn curve_rect_for(strip: Rect) -> Rect {
        let header = if strip.height >= 64.0 { 22.0 } else { 0.0 };
        let padding = if header > 0.0 { 8.0 } else { 0.0 };
        Rect::new(
            strip.x,
            strip.y + header + padding,
            strip.width,
            (strip.height - header - padding - if header > 0.0 { 24.0 } else { 0.0 }).max(1.0),
        )
    }

    pub fn y_at_norm(&self, norm: f32) -> f32 {
        let rect = self.curve_rect();
        rect.y + rect.height * (1.0 - norm.clamp(0.0, 1.0))
    }

    pub fn value_at_y(&self, y: f32) -> f32 {
        let rect = self.curve_rect();
        let norm = (1.0 - (y - rect.y) / rect.height).clamp(0.0, 1.0);
        let value = self.param_min + norm * (self.param_max - self.param_min);
        if self.whole_numbers {
            value.round().clamp(self.param_min, self.param_max)
        } else {
            value
        }
    }

    /// Convert a beat on the shared timeline mapping to lane screen space.
    /// This is deliberately scalar: the renderer uses it while walking the
    /// bounded grid loop and the curve/point mapper uses the same origin.
    pub fn beat_to_pixel(&self, beat: f32) -> f32 {
        self.strip_rect.x + (beat - self.visible_beat_start) * self.pixels_per_beat
    }

    /// Alias for callers that describe the left edge as the mapping origin.
    pub fn visible_beat_origin(&self) -> f32 {
        self.visible_beat_start
    }

    /// Bottom resize handle: only the first 120 px of the strip is reserved.
    pub fn resize_rect(&self) -> Rect {
        Rect::new(
            self.strip_rect.x,
            self.strip_rect.y + self.strip_rect.height - 5.0,
            self.strip_rect.width.min(120.0),
            5.0,
        )
    }
}

/// One breakpoint's screen position plus the model data needed to identify
/// and edit it — the point-level counterpart to [`AutomationLaneScreen`]'s
/// lane-level target/range fields. `beat`/`value_norm`/`shape` are copied
/// verbatim from the `UiAutomationPoint` this dot was sampled from, so a
/// drag-grab or delete can resolve the exact stored `(beat, value)` tuple from
/// the full lane snapshot without re-deriving its identity from pixels.
#[derive(Debug, Clone, Copy)]
pub struct AutomationDotScreen {
    pub x: f32,
    pub y: f32,
    pub beat: Beats,
    pub value_norm: f32,
    pub shape: crate::view::UiSegmentShape,
}

/// Build the automation stroke from segment geometry rather than a fixed
/// screen-space sampler. Breakpoints are always emitted at their exact x
/// coordinate; Hold segments emit both sides of their vertical step there.
/// The returned points are screen-space and ordered by beat/x.
pub(crate) fn build_automation_polyline(
    lane: &UiAutomationLane,
    min_beat: f32,
    max_beat: f32,
    x0: f32,
    x1: f32,
    graph: Rect,
    pixels_per_beat: f32,
) -> Vec<(f32, f32)> {
    if !min_beat.is_finite() || !max_beat.is_finite() || max_beat < min_beat
        || x1 < x0 || !pixels_per_beat.is_finite() || pixels_per_beat <= 0.0
    {
        return Vec::new();
    }
    let min = f64::from(min_beat);
    let max = f64::from(max_beat);
    let screen = |beat: f64, norm: f32| (
        x0 + (beat as f32 - min_beat) * pixels_per_beat,
        graph.y + graph.height * (1.0 - norm.clamp(0.0, 1.0)),
    );
    let mut out = Vec::new();
    if lane.points.len() < 2
        || lane.points.first().is_some_and(|p| p.beat.0 > min)
        || lane.points.last().is_some_and(|p| p.beat.0 < min)
    {
        push_curve_point(&mut out, screen(min, lane.value_at_norm(Beats(min))));
    }
    // Keep one adjacent endpoint on each side; work scales with the visible
    // interval, not the length of an entire performance envelope.
    let lower = lane.points.partition_point(|p| p.beat.0 < min).saturating_sub(1);
    let upper = (lane.points.partition_point(|p| p.beat.0 <= max) + 1).min(lane.points.len());
    for pair in lane.points[lower..upper].windows(2) {
        let (a, b) = (pair[0], pair[1]);
        let (start, end) = (a.beat.0, b.beat.0);
        if end < min || start > max { continue; }
        if end == start {
            push_curve_point(&mut out, screen(start, a.value_norm));
            push_curve_point(&mut out, screen(end, b.value_norm));
            continue;
        }
        if end < start { continue; }
        let from = start.max(min);
        let to = end.min(max);
        let t0 = ((from - start) / (end - start)) as f32;
        let t1 = ((to - start) / (end - start)) as f32;
        if a.shape == UiSegmentShape::Hold {
            push_curve_point(&mut out, screen(from, a.value_norm));
            push_curve_point(&mut out, screen(to, a.value_norm));
            if to == end { push_curve_point(&mut out, screen(end, b.value_norm)); }
        } else {
            let sample = |t: f32| {
                if t == 0.0 { return screen(start, a.value_norm); }
                if t == 1.0 { return screen(end, b.value_norm); }
                screen(
                    start + f64::from(t) * (end - start),
                    a.value_norm + (b.value_norm - a.value_norm) * a.shape.sample(t),
                )
            };
            let p0 = sample(t0);
            let p1 = sample(t1);
            push_curve_point(&mut out, p0);
            if a.shape == UiSegmentShape::Linear {
                push_curve_point(&mut out, p1);
            } else {
                append_curve_segment(&mut out, &sample, t0, t1, p0, p1, 0);
            }
        }
    }
    push_curve_point(&mut out, screen(max, lane.value_at_norm(Beats(max))));
    out
}

fn push_curve_point(out: &mut Vec<(f32, f32)>, point: (f32, f32)) {
    if out.last() != Some(&point) { out.push(point); }
}

fn curve_chord_error_sq(point: (f32, f32), a: (f32, f32), b: (f32, f32)) -> f32 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len_sq = dx * dx + dy * dy;
    let t = if len_sq > 0.0 {
        (((point.0 - a.0) * dx + (point.1 - a.1) * dy) / len_sq).clamp(0.0, 1.0)
    } else { 0.0 };
    (point.0 - a.0 - t * dx).powi(2) + (point.1 - a.1 - t * dy).powi(2)
}

/// A bounded stack subdivision spends vertices where a bend needs them.
/// Quarter samples also catch bends whose midpoint nearly meets the chord.
fn append_curve_segment(
    out: &mut Vec<(f32, f32)>,
    sample: &impl Fn(f32) -> (f32, f32),
    t0: f32, t1: f32,
    p0: (f32, f32), p1: (f32, f32),
    depth: u8,
) {
    let mid = (t0 + t1) * 0.5;
    let pm = sample(mid);
    let error = [sample((t0 + mid) * 0.5), pm, sample((mid + t1) * 0.5)]
        .into_iter().map(|p| curve_chord_error_sq(p, p0, p1)).fold(0.0_f32, f32::max);
    if depth >= 14 || error <= 0.15 * 0.15 {
        push_curve_point(out, p1);
    } else {
        append_curve_segment(out, sample, t0, mid, p0, pm, depth + 1);
        append_curve_segment(out, sample, mid, t1, pm, p1, depth + 1);
    }
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

#[cfg(test)]
mod automation_lane_geometry_tests {
    use super::*;
    use crate::view::UiAutomationPoint;

    fn ui_lane(points: Vec<UiAutomationPoint>) -> UiAutomationLane {
        UiAutomationLane {
            effect_id: EffectId::new("fx"),
            param_id: ParamId::from("amount"),
            target: crate::view::UiGraphTarget::Effect(EffectId::new("fx")),
            label: "Fx: amount".into(),
            points,
            param_min: 0.0,
            param_max: 1.0,
            whole_numbers: false,
            placeholder: false,
        }
    }

    fn lane() -> AutomationLaneScreen {
        AutomationLaneScreen {
            strip_rect: Rect::new(120.0, 40.0, 800.0, 112.0),
            pixels_per_beat: 37.5,
            visible_beat_start: 3.25,
            grid_step: 0.5,
            beats_per_bar: 3.0,
            bar_skip: 1,
            label: "Fx: amount".into(),
            overridden: false,
            polyline: Vec::new(),
            dots: Vec::new(),
            target: crate::view::UiGraphTarget::Effect(EffectId::new("fx")),
            param_id: ParamId::from("amount"),
            param_min: 0.0,
            param_max: 1.0,
            whole_numbers: false,
        }
    }

    #[test]
    fn lane_grid_mapping_preserves_nonzero_scroll_and_zoom() {
        let lane = lane();
        // Beat 3.5 is the first half-beat grid line after a 3.25 beat scroll.
        assert!((lane.beat_to_pixel(3.5) - 129.375).abs() < 0.001);
        // A bar boundary remains mapped by the same scalar origin.
        assert!((lane.beat_to_pixel(6.0) - 223.125).abs() < 0.001);
    }

    #[test]
    fn lane_grid_metadata_handles_time_signature_and_empty_geometry() {
        let mut lane = lane();
        lane.beats_per_bar = 5.0;
        lane.grid_step = 1.0;
        lane.bar_skip = 4;
        lane.strip_rect.width = 0.0;
        assert!(lane.beat_to_pixel(3.25).is_finite());
        assert_eq!(lane.dots.len(), 0);
        assert!(lane.beats_per_bar >= 1.0);
        assert!(lane.bar_skip >= 1);
    }

    #[test]
    fn automation_polyline_keeps_exact_points_and_hold_step() {
        let lane = ui_lane(vec![
            UiAutomationPoint {
                beat: Beats(1.0),
                value_norm: 0.2,
                shape: UiSegmentShape::Hold,
            },
            UiAutomationPoint {
                beat: Beats(3.0),
                value_norm: 0.8,
                shape: UiSegmentShape::Linear,
            },
        ]);
        let points = build_automation_polyline(
            &lane,
            0.0,
            4.0,
            10.0,
            210.0,
            Rect::new(10.0, 0.0, 200.0, 100.0),
            50.0,
        );
        let at_break = points
            .iter()
            .enumerate()
            .filter(|(_, (x, _))| (*x - 160.0).abs() < 0.001)
            .map(|(_, (_, y))| *y)
            .collect::<Vec<_>>();
        assert_eq!(at_break.len(), 2, "Hold needs both sides of its step");
        assert!((at_break[0] - 80.0).abs() < 0.001);
        assert!((at_break[1] - 20.0).abs() < 0.001);
        assert!(points.iter().any(|(x, _)| (*x - 60.0).abs() < 0.001));
        assert!(points.iter().any(|(x, _)| (*x - 160.0).abs() < 0.001));
    }

    #[test]
    fn automation_polyline_connects_both_sides_of_same_time_points() {
        let lane = ui_lane(vec![
            UiAutomationPoint { beat: Beats(1.0), value_norm: 0.2, shape: UiSegmentShape::Linear },
            UiAutomationPoint { beat: Beats(1.0), value_norm: 0.8, shape: UiSegmentShape::Linear },
            UiAutomationPoint { beat: Beats(3.0), value_norm: 0.8, shape: UiSegmentShape::Linear },
        ]);
        for min in [0.0, 1.0] {
            let points = build_automation_polyline(&lane, min, 4.0, 0.0, 200.0,
                Rect::new(0.0, 0.0, 200.0, 100.0), 50.0);
            let x = (1.0 - min) * 50.0;
            assert!(points.windows(2).any(|p| p[0].0 == x && p[1].0 == x
                && (p[0].1 - 80.0).abs() < 0.001 && (p[1].1 - 20.0).abs() < 0.001));
            assert!(points.windows(2).all(|p| p[0].0 <= p[1].0));
        }
    }

    #[test]
    fn automation_curves_stay_within_subpixel_chord_error() {
        for bend in [-1.0, -0.2, 0.2, 1.0] {
            for width in [12.0, 1200.0] {
                let lane = ui_lane(vec![
                    UiAutomationPoint { beat: Beats(0.0), value_norm: 0.0, shape: UiSegmentShape::Curved(bend) },
                    UiAutomationPoint { beat: Beats(4.0), value_norm: 1.0, shape: UiSegmentShape::Linear },
                ]);
                let points = build_automation_polyline(&lane, 0.0, 4.0, 0.0, width,
                    Rect::new(0.0, 0.0, width, 160.0), width / 4.0);
                for step in 0..=1000 {
                    let t = step as f32 / 1000.0;
                    let sample = (width * t, 160.0 * (1.0 - lane.points[0].shape.sample(t)));
                    let error = points.windows(2).map(|p| curve_chord_error_sq(sample, p[0], p[1]))
                        .fold(f32::INFINITY, f32::min);
                    assert!(error < 0.25 * 0.25, "bend {bend}, width {width}, t {t}, error {error}");
                }
            }
        }
    }
}
