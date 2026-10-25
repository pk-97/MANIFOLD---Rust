//! Single transparent overlay covering the entire tracks area.
//! Centralises all clip interaction (click, hover, drag, trim, box-select).
//!
//! Mechanical translation of Assets/Scripts/UI/Timeline/InteractionOverlay.cs.
//!
//! All interaction routing goes through this struct. The viewport panel becomes
//! purely rendering + coordinate conversion. The overlay calls through the
//! `TimelineEditingHost` trait for operations that need engine/editing access.

use ahash::AHashSet;
use manifold_foundation::{Beats, ClipId, ParamId, Seconds};
use std::collections::HashSet;
use std::time::Instant;

use crate::anim::{AnimF32, Transient};
use crate::automation_drag::{
    DragResolver, ResolveInput, apply_value, clamp_group_beat_delta, clamp_single_beat,
    room_to_unselected, shared_norm_delta,
};
use crate::automation_hit_tester::{self, AutomationFeedback, AutomationHit, AutomationOperation};
use crate::clip_hit_tester::{ClipHitResult, ClipHitTester, HitRegion};
use crate::color;
use crate::drag::DragController;
use crate::input::Modifiers;
use crate::node::Vec2;
use crate::panels::viewport::TimelineViewportPanel;
use crate::slider::BitmapSlider;
use crate::timeline_editing_host::{
    AutomationPointMove, ClipRef, TimelineCursor, TimelineEditingHost,
};
use crate::ui_state::{AutomationTimeSelection, UIState};
use crate::view::{UiAutomationPointRef, UiGraphTarget, UiSegmentShape};

// ── Constants ───────────────────────────────────────────────────
// Unity InteractionOverlay lines 78-79.

// Note: SNAP_THRESHOLD_PX lives in `snap.rs`; the live snap is
// `TimelineViewportPanel::magnetic_snap` (viewport/coordinate.rs). These overlay constants will be needed when
// overlay-level snapping is ported (Unity InteractionOverlay lines 78-79).

// ── Shift+Click region selection ─────────────────────────────────
// Port of Unity EditingService.SelectRegionTo (lines 216-262).
// Free function because it needs both UIState and host access.

/// Shift+Click region selection with correct anchor precedence.
/// Anchor priority: insert cursor > existing region > primary selected clip > fallback.
fn select_region_to(
    target_beat: Beats,
    target_layer: usize,
    ui_state: &mut UIState,
    host: &dyn TimelineEditingHost,
) {
    let layer_count = host.layer_count();
    if layer_count == 0 {
        return;
    }

    // Determine anchor — Unity priority: insert cursor > region > primary clip > fallback.
    // A cursor or region whose layers were deleted anchors nothing, never layer 0.
    let anchor: Option<(Beats, usize)> = if ui_state.has_insert_cursor() {
        // Resolve insert cursor layer_id back to an index for region computation
        ui_state
            .insert_cursor_layer_id()
            .and_then(|id| {
                (0..layer_count).find(|&i| host.layer_id_at_index(i).as_ref() == Some(id))
            })
            .map(|anchor_idx| {
                (
                    ui_state.insert_cursor_beat().unwrap_or(Beats::ZERO),
                    anchor_idx,
                )
            })
    } else if let Some(r) = ui_state.current_region() {
        r.layer_index_range(host.layers())
            .map(|(lo, _)| (r.start_beat, lo))
    } else if let Some(clip_id) = ui_state.primary_selected_clip_id().cloned() {
        host.find_clip_by_id(&clip_id)
            .map(|c| (c.start_beat, c.layer_index))
    } else {
        None
    };

    match anchor {
        Some((anchor_beat, anchor_layer)) => {
            let min_beat = anchor_beat.min(target_beat);
            let max_beat = anchor_beat.max(target_beat);
            let min_layer = anchor_layer.min(target_layer).min(layer_count - 1) as i32;
            let max_layer = anchor_layer.max(target_layer).min(layer_count - 1) as i32;
            ui_state.set_region(min_beat, max_beat, min_layer, max_layer, host.layers());
        }
        None => {
            // No anchor — set insert cursor at target (Unity line 247-248)
            let layer_id = host.layer_id_at_index(target_layer).unwrap_or_default();
            ui_state.set_insert_cursor(target_beat, layer_id);
        }
    }
}

// ── Shift+Click clip-range selection (D2) ──────────────────────────
// Shift-click on a CLIP is a
// contiguous whole-clip range selection, NOT a time-range region — the
// `select_region_to` call this arm used to make (same as the empty-lane
// shift path above) was S1/S3's root. This is the live-gesture twin of
// `ui_bridge::select_clip_range_to_with_project` (manifold-app), which does
// the identical thing against a `&Project` for the dispatch/test-harness
// surface; this one reads through `TimelineEditingHost` for the real
// `on_pointer_click` path. Both must move together — see the phase notes.

/// Shift+Click clip-range selection: extend from the current `Clips`
/// selection's anchor to `target_clip_id`, selecting every WHOLE clip on the
/// **anchor's** layer whose start beat falls between the anchor and the
/// target, inclusive. A gap between clips simply isn't a clip — nothing
/// synthesizes a region there. No live anchor (fresh selection, or the
/// anchor clip vanished) falls back to a plain single-clip select.
fn select_clip_range_to(
    target_clip_id: &str,
    ui_state: &mut UIState,
    host: &dyn TimelineEditingHost,
) {
    let Some(target) = host.find_clip_by_id(target_clip_id) else {
        return; // clip vanished under us
    };

    let anchor_id = ui_state.clip_selection_anchor();
    let anchor = anchor_id.as_ref().and_then(|id| host.find_clip_by_id(id));

    let Some(anchor) = anchor else {
        // No live anchor to extend from — behaves like a plain click on the target.
        ui_state.select_clip(ClipId::new(target_clip_id), target.layer_id);
        return;
    };

    let min_beat = anchor.start_beat.min(target.start_beat);
    let max_beat = anchor.start_beat.max(target.start_beat);

    let ids: HashSet<ClipId> = host
        .clips_on_layer(anchor.layer_index)
        .into_iter()
        .filter(|c| c.start_beat >= min_beat && c.start_beat <= max_beat)
        .map(|c| c.clip_id)
        .collect();

    ui_state.set_clip_range(
        ids,
        anchor_id.expect("anchor Some implies anchor_id Some"),
        ClipId::new(target_clip_id),
        target.layer_id,
    );
}

// ── DragMode ────────────────────────────────────────────────────
// Unity InteractionOverlay line 37. Public discriminant, kept for external
// consumers (D11) — the real internal truth is `InteractionOverlay::drag`
// (`DragController<TimelineDrag>`); `drag_mode()` derives this via
// `TimelineDrag::kind()`. `TimelineDrag` is private to this module.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DragMode {
    None,
    Move,
    TrimLeft,
    TrimRight,
    RegionSelect,
    /// Dragging an existing automation breakpoint. State lives in
    /// `TimelineDrag::AutomationPoint`'s payload (P7.3).
    AutomationPoint,
    /// Alt-dragging a segment into a curve bend (P4 Unit B, section 7's
    /// "modifier-drag a segment"). State in `TimelineDrag::AutomationSegmentBend`.
    AutomationSegmentBend,
    /// Plain (no-Alt) vertical drag of a segment — both endpoints move by the
    /// same value delta (P4 Unit B, section 7's "drag a segment"). State in
    /// `TimelineDrag::AutomationSegmentDrag`.
    AutomationSegmentDrag,
    /// Rubber-band selecting automation breakpoints (P4 Unit B, section 7's
    /// "Marquee-select multiple dots"). No payload — the press corner is the
    /// controller session's own `start` (P7.3, D9-style).
    AutomationMarquee,
    /// Dragging a marquee-selected GROUP of breakpoints together (grabbed one
    /// of the selected dots while the marquee set has 2+ members). State in
    /// `TimelineDrag::AutomationGroupMove`.
    AutomationGroupMove,
    /// Pencil/draw mode stroke (P4 Unit B, section 7's "Draw mode") — active
    /// whenever `UIState::automation_draw_mode` is on and the press lands in
    /// a lane strip, overriding dot/segment/marquee routing entirely. State
    /// in `TimelineDrag::AutomationDraw`.
    AutomationDraw,
    AutomationLaneResize,
}

// ── AutomationDragState ─────────────────────────────────────────
// Unit A: grabbed-dot geometry for
// a `DragMode::AutomationPoint` drag. `last_beat`/`last_value` track where
// the point currently sits (recomputed fresh from screen each frame, not
// incrementally) so `TimelineEditingHost::set_automation_point_preview`'s
// beat+raw-value lookup always finds it — the same "recompute from origin, not
// incrementally" discipline `handle_move_drag` uses for clips.

#[derive(Debug, Clone)]
struct AutomationDragState {
    target: UiGraphTarget,
    param_id: ParamId,
    /// The point's state at grab time, PARAM RANGE value — the explicit
    /// reverse `commit_automation_point_move` needs (the
    /// `EditParamMappingCommand::new_with_reverse` drag-commit precedent).
    original: (Beats, f32, UiSegmentShape),
    /// Where the preview last wrote the point — the `from_beat` the NEXT
    /// preview call must search by.
    last_beat: Beats,
    last_value: f32,
    /// The drag's one resolver; a tail drag's is beat-locked.
    resolver: DragResolver,
    /// Neighbour beats at press time: the point may not cross them.
    prev_beat: Beats,
    next_beat: Option<Beats>,
    original_points: Vec<(Beats, f32, UiSegmentShape)>,
    working: Vec<(Beats, f32, UiSegmentShape)>,
}

// ── AutomationSegmentBendState / AutomationSegmentDragState ────────
// Unit B: grabbed-segment geometry
// for the two segment gestures. Both re-derive their live value from the
// ORIGINAL grab geometry each frame (never incrementally), matching every
// other drag state in this file.

/// Alt-drag curve bend: only the LEAVING point's `shape` changes (beat/value
/// untouched), so this carries just enough to preview + commit that one
/// field. `grab_y` is the screen Y at press time; `bend` is re-derived from
/// the vertical delta since grab, not accumulated.
#[derive(Debug, Clone)]
struct AutomationSegmentBendState {
    target: UiGraphTarget,
    param_id: ParamId,
    left_beat: Beats,
    left_value: f32,
    original_shape: UiSegmentShape,
    snapshot_index: usize,
    last_pointer_y: f32,
    accumulated_delta_px: f32,
    direction_sign: f32,
    original_bend: f32,
    last_bend: f32,
    working: Vec<(Beats, f32, UiSegmentShape)>,
}

/// Plain vertical segment drag: both endpoints move by the same normalized
/// value delta. Carries both points' original (param-range) values and
/// shapes — shape is unchanged by this gesture but threaded through so the
/// commit's `MoveAutomationPointCommand`s preserve it exactly.
#[derive(Debug, Clone)]
struct AutomationSegmentDragState {
    target: UiGraphTarget,
    param_id: ParamId,
    left_beat: Beats,
    left_original_value: f32,
    left_shape: UiSegmentShape,
    right_beat: Beats,
    right_original_value: f32,
    right_shape: UiSegmentShape,
    /// Beat-locked: a segment moves in value only.
    resolver: DragResolver,
    last_left_value: f32,
    last_right_value: f32,
}

// ── AutomationGroupDragState / AutomationDrawState ──────────────────────
// Unit B: group move of the
// marquee-selected set, and pencil/draw mode. The marquee's own state
// (P7.3, D9-style) is just the drag controller session's `start` — no
// separate `AutomationMarqueeState` struct.

/// Marquee press corner in content space, plus the selection a Shift-drag
/// adds to (empty for a plain drag, which replaces the selection).
#[derive(Debug, Clone)]
struct AutomationMarqueeAnchor {
    beat: Beats,
    content_y: f32,
    base: Vec<UiAutomationPointRef>,
}

/// One marquee-selected point's captured geometry for a group move. Each
/// point keeps its OWN param range (points can span multiple lanes/params at
/// once) — only the normalized delta is shared across the group.
#[derive(Debug, Clone)]
struct AutomationGroupPointState {
    target: UiGraphTarget,
    param_id: ParamId,
    lane_index: usize,
    original_beat: Beats,
    last_beat: Beats,
    original_value: f32,
    shape: UiSegmentShape,
    param_min: f32,
    param_max: f32,
    whole_numbers: bool,
    last_value: f32,
}

/// Reusable scratch lane for a grouped drag. The original points remain in
/// `automation_lane_snapshots` so Escape and the eventual commit share the
/// same full-lane capture; this buffer is rebuilt in place for each frame.
#[derive(Debug, Clone)]
struct AutomationGroupLaneState {
    target: UiGraphTarget,
    param_id: ParamId,
    snapshot_index: usize,
    working: Vec<(Beats, f32, UiSegmentShape)>,
}

/// Dragging the whole marquee-selected group. `grab_target`/`grab_param_id`
/// identify the GRABBED lane, re-resolved fresh each frame for its
/// `strip_rect` (handles mid-drag scroll) to compute the shared normalized
/// delta applied to every point in `points`.
#[derive(Debug, Clone)]
struct AutomationGroupDragState {
    grab_target: UiGraphTarget,
    grab_param_id: ParamId,
    grabbed_original_beat: Beats,
    grabbed_original_value: f32,
    resolver: DragResolver,
    /// Earliest selected beat, and the room to the nearest unselected point on
    /// each side across every lane, both captured at press.
    min_beat: f64,
    room_left: f64,
    room_right: f64,
    points: Vec<AutomationGroupPointState>,
    lanes: Vec<AutomationGroupLaneState>,
}

/// One in-progress pencil/draw stroke. `old_points` is the FULL (unfiltered
/// by visible range) pre-stroke point list — `None` if the stroke is
/// creating the lane — captured via `TimelineEditingHost::
/// automation_lane_points` at grab time, since `AutomationLaneScreen::dots`
/// is culled to the visible beat range and would silently drop off-screen
/// points. `working` is the list being built as the stroke proceeds; it
/// starts as a clone of `old_points` (or empty).
#[derive(Debug, Clone)]
struct AutomationDrawState {
    target: UiGraphTarget,
    param_id: ParamId,
    /// Value/beat denormalization always re-resolves the lane's CURRENT
    /// `param_min`/`param_max` fresh each frame in `write_automation_draw_step`
    /// (handles the (practically static, but consistent with every other
    /// handler's discipline) case of the range changing mid-stroke) — not
    /// cached here.
    new_point_shape: UiSegmentShape,
    old_points: Option<Vec<(Beats, f32, UiSegmentShape)>>,
    working: Vec<(Beats, f32, UiSegmentShape)>,
    sweep_scratch: Vec<(Beats, f32, UiSegmentShape)>,
    last_sample: Option<(Beats, f32)>,
}

#[derive(Debug, Clone)]
struct AutomationPressDraft {
    target: UiGraphTarget,
    param_id: ParamId,
    old_points: Option<Vec<(Beats, f32, UiSegmentShape)>>,
    beat: Beats,
    value: f32,
    shape: UiSegmentShape,
    segment: bool,
    hit: AutomationHit,
    param_min: f32,
    param_max: f32,
}

#[derive(Debug, Clone)]
struct AutomationLaneResize {
    target: UiGraphTarget,
    param_id: ParamId,
    start_y: f32,
    start_height: f32,
}

#[derive(Debug, Clone)]
struct AutomationLaneGestureSnapshot {
    target: UiGraphTarget,
    param_id: ParamId,
    points: Option<Vec<(Beats, f32, UiSegmentShape)>>,
}

/// Insert-or-overwrite `(beat, value, shape)` into a sorted-by-beat working
/// point list — the pencil's per-grid-step write. Matches an existing beat
/// exactly (grid-snapped beats are stable across frames within the same
/// grid cell) or inserts at the sorted position.
fn apply_draw_point(
    points: &mut Vec<(Beats, f32, UiSegmentShape)>,
    beat: Beats,
    value: f32,
    shape: UiSegmentShape,
) {
    match points.iter().position(|p| p.0.0 == beat.0) {
        Some(idx) => points[idx] = (beat, value, shape),
        None => {
            let pos = points
                .iter()
                .position(|p| p.0.0 > beat.0)
                .unwrap_or(points.len());
            points.insert(pos, (beat, value, shape));
        }
    }
}

fn shape_with_bend(shape: UiSegmentShape, bend: f32) -> UiSegmentShape {
    match shape {
        UiSegmentShape::Curved(_) => UiSegmentShape::Curved(bend),
        UiSegmentShape::CurvedRange { start, end, .. } => {
            UiSegmentShape::CurvedRange { bend, start, end }
        }
        UiSegmentShape::Linear | UiSegmentShape::Hold => UiSegmentShape::Curved(bend),
    }
}

fn shape_bend(shape: UiSegmentShape) -> f32 {
    match shape {
        UiSegmentShape::Curved(bend) | UiSegmentShape::CurvedRange { bend, .. } => bend,
        UiSegmentShape::Linear | UiSegmentShape::Hold => 0.0,
    }
}

/// Resolve the value and interpolation fraction at a beat inside a visible
/// segment. The viewport includes one endpoint on either side of the visible
/// interval, so this remains correct while the lane is scrolled.
fn segment_value_at_beat(
    lane: &crate::panels::viewport::AutomationLaneScreen,
    beat: Beats,
) -> Option<(f32, f32, Beats)> {
    let right_index = lane.dots.iter().position(|dot| dot.beat > beat)?;
    let left_index = right_index.checked_sub(1)?;
    let left = lane.dots[left_index];
    let right = lane.dots[right_index];
    let span = right.beat.0 - left.beat.0;
    if span <= 0.0 {
        return None;
    }
    let t = ((beat.0 - left.beat.0) / span) as f32;
    let left_value = lane.param_min + left.value_norm * (lane.param_max - lane.param_min);
    let right_value = lane.param_min + right.value_norm * (lane.param_max - lane.param_min);
    Some((
        left_value + (right_value - left_value) * left.shape.sample(t),
        t,
        left.beat,
    ))
}

/// The value of the lane's drawn line at `beat`: the segment value between two
/// points, or the end point's value on a flat tail. The third item is the
/// left point of the segment, `None` on a tail. `None` for a lane with no
/// points; a placeholder's value comes from `placeholder_value`.
fn line_value_at_beat(
    lane: &crate::panels::viewport::AutomationLaneScreen,
    beat: Beats,
) -> Option<(f32, f32, Option<Beats>)> {
    if let Some((value, t, left)) = segment_value_at_beat(lane, beat) {
        return Some((value, t, Some(left)));
    }
    let (first, last) = (lane.dots.first()?, lane.dots.last()?);
    let end = if beat <= first.beat {
        first
    } else if beat >= last.beat {
        last
    } else {
        return None;
    };
    Some((
        lane.param_min + end.value_norm * (lane.param_max - lane.param_min),
        0.0,
        None,
    ))
}

/// Split a preview lane at `beat`, preserving the curve on both outer tails.
/// The committed segment path performs the same operation in the content
/// host; this keeps the press preview visually identical to the release.
fn insert_preview_point(
    points: &mut Vec<(Beats, f32, UiSegmentShape)>,
    beat: Beats,
    value: f32,
    shape: UiSegmentShape,
    segment_left_beat: Option<Beats>,
    segment_t: f32,
) {
    if segment_left_beat.is_some() && points.iter().any(|point| point.0 == beat) {
        return;
    }
    if let Some(left_beat) = segment_left_beat
        && let Some(index) = points.iter().position(|point| point.0 == left_beat)
    {
        let original = points[index].2;
        points[index].2 = original.subrange(0.0, segment_t);
        let new_shape = original.subrange(segment_t, 1.0);
        let right_insert = index + 1;
        if points
            .get(right_insert)
            .is_some_and(|point| point.0 == beat)
        {
            points[right_insert] = (beat, value, new_shape);
        } else {
            points.insert(right_insert, (beat, value, new_shape));
        }
        return;
    }
    apply_draw_point(points, beat, value, shape);
}

fn apply_draw_sweep(
    points: &mut Vec<(Beats, f32, UiSegmentShape)>,
    from: Option<(Beats, f32)>,
    to: (Beats, f32),
    shape: UiSegmentShape,
    grid_step: f32,
    bypass_snap: bool,
) {
    let Some((from_beat, from_value)) = from else {
        apply_draw_point(points, to.0, to.1, shape);
        return;
    };
    let lo = from_beat.0.min(to.0.0);
    let hi = from_beat.0.max(to.0.0);
    points.retain(|point| point.0.0 < lo || point.0.0 > hi);
    if bypass_snap || !grid_step.is_finite() || grid_step <= f32::EPSILON {
        apply_draw_point(points, from_beat, from_value, shape);
        apply_draw_point(points, to.0, to.1, shape);
        return;
    }
    let direction = if to.0.0 >= from_beat.0 { 1.0 } else { -1.0 };
    let step = f64::from(grid_step.abs());
    let mut beat = from_beat.0;
    loop {
        let t = if (to.0.0 - from_beat.0).abs() <= f64::EPSILON {
            1.0
        } else {
            ((beat - from_beat.0) / (to.0.0 - from_beat.0)).clamp(0.0, 1.0) as f32
        };
        apply_draw_point(
            points,
            Beats(beat),
            from_value + (to.1 - from_value) * t,
            shape,
        );
        if (beat - to.0.0).abs() <= step * 0.5 {
            break;
        }
        let next = beat + direction * step;
        if next == beat || (direction > 0.0 && next > to.0.0) || (direction < 0.0 && next < to.0.0)
        {
            beat = to.0.0;
        } else {
            beat = next;
        }
    }
}

fn draw_value_at(points: &[(Beats, f32, UiSegmentShape)], beat: Beats) -> Option<f32> {
    let right_index = points.iter().position(|point| point.0 > beat);
    let Some(right_index) = right_index else {
        return points.last().map(|point| point.1);
    };
    if right_index == 0 {
        return points.first().map(|point| point.1);
    }
    let left = points[right_index - 1];
    let right = points[right_index];
    let span = right.0.0 - left.0.0;
    if span <= 0.0 {
        return Some(left.1);
    }
    let t = ((beat.0 - left.0.0) / span) as f32;
    Some(left.1 + (right.1 - left.1) * left.2.sample(t))
}

/// Rebuild a pencil interval from the stroke's original lane. The two
/// representable boundary points preserve the sampled curve and clipped
/// outgoing shapes on both tails, matching the content-side punch operation.
fn rebuild_draw_interval(
    original: &[(Beats, f32, UiSegmentShape)],
    working: &mut Vec<(Beats, f32, UiSegmentShape)>,
    recorded: &[(Beats, f32, UiSegmentShape)],
) {
    let (Some(first), Some(last)) = (recorded.first(), recorded.last()) else {
        return;
    };
    if original.is_empty() {
        working.clear();
        working.extend_from_slice(recorded);
        return;
    }
    let before = Beats(first.0.0.next_down());
    let after = Beats(last.0.0.next_up());
    working.clear();
    working.extend(original.iter().copied().filter(|point| point.0 < before));
    if first.0 > Beats::ZERO {
        if let Some(left) = working.last_mut()
            && let Some(right) = original.iter().find(|point| point.0 > left.0)
        {
            let t = ((before.0 - left.0.0) / (right.0.0 - left.0.0)).clamp(0.0, 1.0) as f32;
            left.2 = left.2.subrange(0.0, t);
        }
        if let Some(value) = draw_value_at(original, before) {
            working.push((before, value, UiSegmentShape::Hold));
        }
    }
    working.extend(recorded.iter().copied());
    let right_shape = original
        .iter()
        .enumerate()
        .rfind(|(_, point)| point.0 <= after)
        .and_then(|(index, left)| {
            original.get(index + 1).map(|right| {
                let t = ((after.0 - left.0.0) / (right.0.0 - left.0.0)).clamp(0.0, 1.0) as f32;
                left.2.subrange(t, 1.0)
            })
        })
        .unwrap_or(UiSegmentShape::Hold);
    if let Some(value) = draw_value_at(original, after) {
        working.push((after, value, right_shape));
    }
    working.extend(original.iter().copied().filter(|point| point.0 > after));
}

#[cfg(test)]
mod automation_gesture_math_tests;

fn insert_moved_point(
    points: &mut Vec<(Beats, f32, UiSegmentShape)>,
    from_beat: Beats,
    from_value: f32,
    beat: Beats,
    value: f32,
    shape: UiSegmentShape,
) {
    let source_rank = points
        .iter()
        .filter(|p| p.0 == from_beat)
        .position(|p| p.1 == from_value)
        .unwrap_or(0);
    insert_moved_point_with_rank(
        points,
        from_beat,
        beat,
        value,
        shape,
        source_rank,
        from_value,
    );
}

fn insert_moved_point_with_rank(
    points: &mut Vec<(Beats, f32, UiSegmentShape)>,
    from_beat: Beats,
    beat: Beats,
    value: f32,
    shape: UiSegmentShape,
    source_rank: usize,
    from_value: f32,
) {
    // Only the grabbed point leaves; a neighbour that shares the destination
    // beat and value stays (a drop never deletes anything).
    if let Some(source) = points
        .iter()
        .position(|p| p.0 == from_beat && p.1 == from_value)
    {
        points.remove(source);
    }
    let first_equal = points.iter().position(|p| p.0 == beat);
    let index = match first_equal {
        Some(first) if beat.0 > from_beat.0 => first,
        Some(first) if beat.0 < from_beat.0 => points[first..]
            .iter()
            .position(|p| p.0 != beat)
            .map_or(points.len(), |offset| first + offset),
        Some(first) => {
            let count = points[first..].iter().take_while(|p| p.0 == beat).count();
            first + source_rank.min(count)
        }
        None => points
            .iter()
            .position(|p| p.0.0 > beat.0)
            .unwrap_or(points.len()),
    };
    points.insert(index, (beat, value, shape));
}

// ── DragSnapshot ────────────────────────────────────────────────
// Unity InteractionOverlay lines 49-54.

#[derive(Debug, Clone)]
pub struct DragSnapshot {
    pub clip_id: ClipId,
    pub start_beat: Beats,
    pub layer_index: usize,
}

// ── TrimOriginal ────────────────────────────────────────────────
// Per-clip pre-trim geometry captured at grab time. A trim drag fans the
// grabbed clip's edge delta over one of these per selected clip, then
// records each into a single undo batch on drag end.

#[derive(Debug, Clone)]
struct TrimOriginal {
    clip_id: ClipId,
    start_beat: Beats,
    duration_beats: Beats,
    in_point: Seconds,
    is_generator: bool,
    is_looping: bool,
}

// ── TrimDrag / RegionDrag ─────────────────────────────────
// The grabbed clip's own pre-trim geometry (for the shared edge-delta snap
// context) plus every selected clip's `TrimOriginal` the delta fans over —
// the fold target for the grabbed-clip-id field, the three grabbed-clip
// geometry fields, and the fan-over Vec.
#[derive(Debug, Clone)]
struct TrimDrag {
    clip_id: ClipId,
    original_start_beat: Beats,
    original_duration_beats: Beats,
    originals: Vec<TrimOriginal>,
}

/// Rubber-band region-select's grab geometry — the fold target for the two
/// parallel grab-beat/grab-layer fields.
#[derive(Debug, Clone, Copy)]
struct RegionDrag {
    start_beat: Beats,
    start_layer: usize,
}

/// The fold target for eleven loose fields: the anchor clip id (was `Option<ClipId>`
/// — P7.5's entry-proof established every path that arms a move drag also
/// sets an anchor in the same synchronous call, so it's non-optional here by
/// construction), the start-layer index, the per-clip snapshot Vec + id set,
/// the three selection-extent fields, the cross-layer-blocked flag, the
/// opt-drag-duplicate flag, and the anchor's start-beat/offset-beats pair.
/// The `AnimF32`/`Transient` visual-tween fields on `InteractionOverlay`
/// (`lift_anim`, `ghost_alpha`, `settle_dx`, `drag_visual_clip_ids`,
/// `landing_flash*`, `error_shake`, `was_layer_blocked`) stay OUTSIDE this
/// payload by design — they deliberately keep easing after release, so their
/// lifetime outlaps the drag.
#[derive(Debug, Clone)]
struct MoveDrag {
    anchor_clip_id: ClipId,
    start_layer_index: usize,
    snapshots: Vec<DragSnapshot>,
    snapshot_clip_ids: HashSet<ClipId>,
    selection_min_start_beat: Beats,
    selection_min_layer: usize,
    selection_max_layer: usize,
    layer_blocked: bool,
    /// Alt/Opt held at grab time → on release, leave a copy of each moved
    /// clip at its original position (opt-drag duplicate).
    duplicate_on_release: bool,
    /// Anchor clip's start beat at grab time.
    start_beat: Beats,
    /// Offset from the anchor clip's start to the mouse beat at grab time.
    offset_beats: Beats,
}

// ── TimelineDrag (P7.3/P7.4/P7.5, D8/D11) ────────────────────────────────
// The single lifecycle-owner payload for `InteractionOverlay`'s drag
// controller. Every gesture — move, trim, region-select, and the six
// automation variants — carries its state on this payload now; P7's fold
// is complete.

#[derive(Debug, Clone)]
enum TimelineDrag {
    Move(MoveDrag),
    TrimLeft(TrimDrag),
    TrimRight(TrimDrag),
    RegionSelect(RegionDrag),
    AutomationPoint(AutomationDragState),
    AutomationSegmentBend(AutomationSegmentBendState),
    AutomationSegmentDrag(AutomationSegmentDragState),
    /// Rubber-band marquee. The press corner lives in content space so edge
    /// autoscroll keeps it pinned to the beat it was pressed on.
    AutomationMarquee(AutomationMarqueeAnchor),
    AutomationGroupMove(AutomationGroupDragState),
    AutomationDraw(AutomationDrawState),
    AutomationLaneResize(AutomationLaneResize),
}

impl TimelineDrag {
    /// 1:1 projection onto the public discriminant every external consumer
    /// (auto-scroll polling, drag readout, snap, input routing) still reads.
    fn kind(&self) -> DragMode {
        match self {
            TimelineDrag::Move(_) => DragMode::Move,
            TimelineDrag::TrimLeft(_) => DragMode::TrimLeft,
            TimelineDrag::TrimRight(_) => DragMode::TrimRight,
            TimelineDrag::RegionSelect(_) => DragMode::RegionSelect,
            TimelineDrag::AutomationPoint(_) => DragMode::AutomationPoint,
            TimelineDrag::AutomationSegmentBend(_) => DragMode::AutomationSegmentBend,
            TimelineDrag::AutomationSegmentDrag(_) => DragMode::AutomationSegmentDrag,
            TimelineDrag::AutomationMarquee(_) => DragMode::AutomationMarquee,
            TimelineDrag::AutomationGroupMove(_) => DragMode::AutomationGroupMove,
            TimelineDrag::AutomationDraw(_) => DragMode::AutomationDraw,
            TimelineDrag::AutomationLaneResize(_) => DragMode::AutomationLaneResize,
        }
    }
}

// ── InteractionOverlay ──────────────────────────────────────────
// Unity InteractionOverlay lines 17-73.

pub struct InteractionOverlay {
    // Dependencies
    clip_vertical_padding: f32,

    // Drag state (Unity lines 37-73) — EXCLUSIVELY owned here
    // P7.3/P7.4/P7.5 (D8/D11): every gesture's state — move, trim, region,
    // and the six automation variants — folds onto this one controller.
    drag: DragController<TimelineDrag>,

    // Current modifier state — set by app before each event.
    // Unity reads Keyboard.current inline; Rust stores latest modifiers here.
    modifiers: Modifiers,

    // ── P2 motion ──────────────
    // Purely visual drag feedback — the model already moves/snaps instantly
    // (D15: data first, visual follows); these only change what the render
    // loop draws for the dragged clip set. Shared scalars, not per-clip:
    // every clip in a move-drag starts/ends its lift, ghost, and settle at
    // the SAME moment (drag begin / `finalize_move_snap`), so one `AnimF32`
    // per effect — applied uniformly to every id in `drag_visual_clip_ids`
    // — is correct, not an approximation.
    /// Grab lift (D17): targets 1.0 while `drag_mode == Move`, 0.0
    /// otherwise. The render loop reads [`Self::lift_amount`] for a 1-2px
    /// rise on every dragged clip.
    lift_anim: AnimF32,
    /// Duplicate-drag ghost (D17): targets 0.5 while alt-dragging
    /// (`drag_mode == Move && duplicate_on_release`), 1.0 otherwise — eases
    /// back up ("solidifies") once the drag ends and the real duplicate
    /// commits in `on_end_drag`.
    ghost_alpha: AnimF32,
    /// Grid-settle offset in PIXELS (D15): seeded in `finalize_move_snap`
    /// with the just-applied snap correction (screen-space) and eased to
    /// 0 — the model is already at its final snapped position; this only
    /// eases the DRAWN rect there from where the release-frame visual sat.
    settle_dx: AnimF32,
    /// The clip ids [`Self::lift_anim`]/[`Self::ghost_alpha`]/
    /// [`Self::settle_dx`] apply to. During a live move-drag this mirrors
    /// the move payload's own snapshot-clip-id set; `on_end_drag` snapshots
    /// it here BEFORE
    /// clearing the real drag state, so the tweens have something to keep
    /// easing against post-release — the same "keep drawing past the state
    /// event" idea the exit-state pattern uses for deletion, applied here to
    /// a visual settle instead. Cleared once every tween above has settled
    /// (see `tick`).
    drag_visual_clip_ids: Vec<ClipId>,
    /// Landing-line flash (D15): fires once in `finalize_move_snap` when the
    /// snap correction actually moved something. Geometry for the render
    /// loop's vertical line: the snapped beat + the dragged selection's
    /// layer span.
    landing_flash: Transient,
    landing_flash_beat: Beats,
    landing_flash_layers: (usize, usize),
    /// Error shake (D17): fires once on the RISING edge of the move
    /// payload's layer-blocked flag (a cross-layer move rejected mid-drag)
    /// — a 3px/240ms horizontal shake applied to every dragged clip.
    /// `was_layer_blocked` detects the edge so a held-blocked drag doesn't
    /// re-fire every frame.
    error_shake: Transient,
    was_layer_blocked: bool,
    automation_lane_snapshots: Vec<AutomationLaneGestureSnapshot>,
    automation_press: Option<AutomationPressDraft>,
    forced_automation_hit: Option<AutomationHit>,
    /// Set when a press draft was cancelled or committed: the gesture is
    /// finished, so a Click routed after it must not insert again.
    /// Cleared by the next pointer-down.
    suppress_next_automation_click: bool,
    /// Where and when the last left click deleted an automation dot. The
    /// second press and clicks of a double-click on that spot land on the
    /// strip the dot exposed and must not re-add a point. Held until the
    /// double-click window passes or the double-click's own event arrives.
    last_dot_delete: Option<(Vec2, Instant)>,
    /// The point the last plain click made. A click on it inside the
    /// double-click window selects instead of deleting, so a double-click on
    /// empty space ends with one point.
    last_point_insert: Option<RecentInsert>,
    /// Pointer, scroll, zoom and modifiers the last automation drag frame
    /// was applied with. An identical frame re-derives the same preview, so
    /// the per-frame poll skips it. Cleared when the gesture ends.
    last_automation_drag_frame: Option<AutomationDragFrameKey>,
}

/// A breakpoint a click just inserted, kept for the double-click window.
#[derive(Debug, Clone)]
struct RecentInsert {
    target: UiGraphTarget,
    param_id: ParamId,
    beat: Beats,
    when: Instant,
}

/// Everything an automation drag frame is derived from besides the gesture's
/// own state.
#[derive(Clone, Copy, PartialEq)]
struct AutomationDragFrameKey {
    pos: Vec2,
    scroll_x: Beats,
    scroll_y: f32,
    pixels_per_beat: f32,
    modifiers: Modifiers,
}

impl InteractionOverlay {
    pub fn new(clip_vertical_padding: f32) -> Self {
        Self {
            clip_vertical_padding,
            drag: DragController::new(),
            modifiers: Modifiers::NONE,
            lift_anim: AnimF32::new(0.0, color::MOTION_MED_MS),
            ghost_alpha: AnimF32::new(1.0, color::MOTION_MED_MS),
            settle_dx: AnimF32::new(0.0, color::MOTION_MED_MS),
            drag_visual_clip_ids: Vec::with_capacity(8),
            landing_flash: Transient::default(),
            landing_flash_beat: Beats::ZERO,
            landing_flash_layers: (0, 0),
            error_shake: Transient::default(),
            was_layer_blocked: false,
            automation_lane_snapshots: Vec::with_capacity(4),
            automation_press: None,
            forced_automation_hit: None,
            suppress_next_automation_click: false,
            last_dot_delete: None,
            last_point_insert: None,
            last_automation_drag_frame: None,
        }
    }

    /// True while a lane point is visible as a provisional press preview. The
    /// app uses this to avoid replacing the draft with a stale snapshot before
    /// the matching pointer-up/click pair has been consumed.
    pub fn has_pending_automation_press(&self) -> bool {
        self.automation_press.is_some()
    }

    /// True for any automation gesture, including a lane resize or marquee.
    pub fn is_automation_drag(&self) -> bool {
        matches!(
            self.drag_mode(),
            DragMode::AutomationPoint
                | DragMode::AutomationSegmentBend
                | DragMode::AutomationSegmentDrag
                | DragMode::AutomationMarquee
                | DragMode::AutomationGroupMove
                | DragMode::AutomationDraw
                | DragMode::AutomationLaneResize
        )
    }

    fn capture_automation_lane_snapshot(
        &mut self,
        host: &dyn TimelineEditingHost,
        target: UiGraphTarget,
        param_id: ParamId,
    ) -> usize {
        if let Some(index) = self
            .automation_lane_snapshots
            .iter()
            .position(|s| s.target == target && s.param_id == param_id)
        {
            return index;
        }
        let points = host.automation_lane_points(&target, &param_id);
        self.automation_lane_snapshots
            .push(AutomationLaneGestureSnapshot {
                target,
                param_id,
                points,
            });
        self.automation_lane_snapshots.len() - 1
    }

    fn restore_pending_automation_press(&mut self, host: &mut dyn TimelineEditingHost) {
        let Some(draft) = self.automation_press.take() else {
            return;
        };
        host.restore_automation_lane_preview(
            &draft.target,
            &draft.param_id,
            draft.old_points.as_deref(),
        );
        host.clear_automation_previews();
    }

    fn commit_pending_automation_press(
        &mut self,
        host: &mut dyn TimelineEditingHost,
        ui_state: &mut UIState,
    ) {
        let Some(draft) = self.automation_press.take() else {
            return;
        };
        ui_state.selected_automation_points.clear();
        ui_state.automation_time_selection = None;
        ui_state.automation_insert_beat = Some(draft.beat);
        ui_state.selected_automation_point = None;
        // The live preview already contains the candidate. Restore the
        // pre-press lane before issuing the authoritative insert so the
        // command captures the real old state and creates exactly one undo.
        host.restore_automation_lane_preview(
            &draft.target,
            &draft.param_id,
            draft.old_points.as_deref(),
        );
        host.clear_automation_previews();
        // Select the (beat, value) the host made, never the UI's sample.
        let made = if draft.segment {
            host.insert_automation_point_on_curve(
                &draft.target,
                &draft.param_id,
                draft.beat,
                draft.shape,
            )
        } else {
            host.add_automation_point(
                &draft.target,
                &draft.param_id,
                draft.beat,
                draft.value,
                draft.shape,
            );
            Some(draft.value)
        };
        if let Some(value) = made {
            ui_state.selected_automation_point = Some(UiAutomationPointRef {
                target: draft.target.clone(),
                param_id: draft.param_id.clone(),
                beat: draft.beat,
                value_norm: ((value - draft.param_min)
                    / (draft.param_max - draft.param_min).max(f32::EPSILON))
                .clamp(0.0, 1.0),
            });
            self.last_point_insert = Some(RecentInsert {
                target: draft.target,
                param_id: draft.param_id,
                beat: draft.beat,
                when: Instant::now(),
            });
        }
    }

    /// True when the point at `beat` on this lane was inserted by a click
    /// inside the double-click window.
    fn is_recent_insert(&self, target: &UiGraphTarget, param_id: &ParamId, beat: Beats) -> bool {
        self.last_point_insert.as_ref().is_some_and(|r| {
            r.target == *target
                && r.param_id == *param_id
                && r.beat == beat
                && r.when.elapsed().as_secs_f32() < color::DOUBLE_CLICK_TIME_SEC
        })
    }

    /// True when `pos` is inside the double-click time and radius of the dot
    /// the last click deleted.
    fn within_dot_delete_window(&self, pos: Vec2) -> bool {
        self.last_dot_delete.is_some_and(|(at, when)| {
            when.elapsed().as_secs_f32() < color::DOUBLE_CLICK_TIME_SEC
                && at.distance(pos) < color::DOUBLE_CLICK_RADIUS_PX
        })
    }

    /// Arm a point candidate on a bare strip or curve segment. This mutates
    /// only the host's live preview; the undoable insert happens at release.
    pub fn on_pointer_down(
        &mut self,
        pos: Vec2,
        host: &mut dyn TimelineEditingHost,
        ui_state: &mut UIState,
        viewport: &TimelineViewportPanel,
    ) {
        self.suppress_next_automation_click = false;
        self.restore_pending_automation_press(host);
        // The second press of a double-click that just deleted a dot lands on
        // the strip the dot exposed; arming a draft there would re-add it.
        // The guard stays armed so the routed Click/DoubleClick are swallowed.
        if self.within_dot_delete_window(pos) {
            return;
        }
        let lanes = viewport.automation_lane_screens();
        if automation_hit_tester::resolve_feedback(
            pos,
            lanes,
            self.modifiers.alt,
            ui_state.automation_draw_mode,
        )
        .is_some_and(|(_, op, _)| {
            matches!(
                op,
                AutomationOperation::Title
                    | AutomationOperation::Resize
                    | AutomationOperation::Blocked
            )
        }) {
            return;
        }
        let Some(hit) = automation_hit_tester::hit_test_automation(pos, lanes) else {
            return;
        };
        if self.modifiers.alt || self.modifiers.shift {
            return;
        }
        // On the curve: between two points or on a tail past the end points.
        // A placeholder tail has no point to follow, so it takes the lane's
        // current value like a bare strip does.
        let (lane, on_curve) = match hit {
            AutomationHit::Strip { lane_index } => (&lanes[lane_index], false),
            AutomationHit::Segment { lane_index, .. } => (&lanes[lane_index], true),
            AutomationHit::Tail {
                lane_index,
                dot_index,
            } => (&lanes[lane_index], dot_index.is_some()),
            AutomationHit::Dot { .. } => return,
        };
        if !lane.param_min.is_finite()
            || !lane.param_max.is_finite()
            || lane.param_max <= lane.param_min
        {
            return;
        }
        let raw_beat = viewport.pixel_to_beat(pos.x);
        let beat = if self.modifiers.command {
            raw_beat
        } else {
            viewport.snap_to_grid(raw_beat)
        }
        .max(Beats::ZERO);
        // A line click that snaps onto an existing point means that point:
        // the curve insert would refuse it, so a draft would only flash and vanish.
        if on_curve && let Some(dot) = lane.dots.iter().find(|dot| dot.beat == beat).copied() {
            self.select_automation_dot(lane, dot, ui_state);
            return;
        }
        let (value, segment_t, resolved_segment_left_beat) = on_curve
            .then(|| line_value_at_beat(lane, beat))
            .flatten()
            .unwrap_or_else(|| {
                (
                    lane.placeholder_value
                        .unwrap_or_else(|| lane.value_at_y(pos.y)),
                    0.0,
                    None,
                )
            });
        let shape = if lane.whole_numbers {
            UiSegmentShape::Hold
        } else {
            UiSegmentShape::Linear
        };
        let old_points = host.automation_lane_points(&lane.target, &lane.param_id);
        let mut working = old_points.clone().unwrap_or_default();
        insert_preview_point(
            &mut working,
            beat,
            value,
            shape,
            resolved_segment_left_beat,
            segment_t,
        );
        host.set_automation_lane_preview(&lane.target, &lane.param_id, &working);
        ui_state.automation_paste_context = Some((lane.target.clone(), lane.param_id.clone()));
        self.automation_press = Some(AutomationPressDraft {
            target: lane.target.clone(),
            param_id: lane.param_id.clone(),
            old_points,
            beat,
            value,
            shape,
            segment: on_curve,
            hit,
            param_min: lane.param_min,
            param_max: lane.param_max,
        });
    }

    /// Commit the candidate armed by [`Self::on_pointer_down`]. Release is
    /// the commit signal: a drag or cancel has already consumed the draft,
    /// and the routed Click can be lost when the tree rebuilds mid-press.
    pub fn on_pointer_up(&mut self, host: &mut dyn TimelineEditingHost, ui_state: &mut UIState) {
        if self.automation_press.is_some() {
            self.commit_pending_automation_press(host, ui_state);
            self.suppress_next_automation_click = true;
        }
    }

    // ── P2 motion ──────────────

    /// Per-frame tween tick for the overlay's drag-visual pieces (grab lift,
    /// duplicate ghost, grid settle, landing-line flash, error shake). Call
    /// once per frame from the app's frame loop — mirrors
    /// `GraphCanvas::tick`. Returns `true` while anything is still
    /// animating, so the caller can keep the timeline dirty/repainting.
    pub fn tick(&mut self, dt_ms: f32) -> bool {
        let mut any = false;

        let dragging_move = matches!(self.drag.payload(), Some(TimelineDrag::Move(_)));
        let duplicate_on_release =
            matches!(self.drag.payload(), Some(TimelineDrag::Move(m)) if m.duplicate_on_release);
        if let Some(TimelineDrag::Move(m)) = self.drag.payload() {
            self.drag_visual_clip_ids.clear();
            self.drag_visual_clip_ids
                .extend(m.snapshot_clip_ids.iter().cloned());
        }

        self.lift_anim
            .set_target(if dragging_move { 1.0 } else { 0.0 });
        any |= self.lift_anim.tick(dt_ms);

        self.ghost_alpha
            .set_target(if dragging_move && duplicate_on_release {
                0.5
            } else {
                1.0
            });
        any |= self.ghost_alpha.tick(dt_ms);

        any |= self.settle_dx.tick(dt_ms);
        any |= self.landing_flash.tick(dt_ms);
        any |= self.error_shake.tick(dt_ms);

        // Drop the settling clip-id memory once every visual has caught up
        // and no new drag is in flight — otherwise a rapid re-drag right
        // after release would still see stale ids from the previous
        // gesture (harmless — they'd just get overwritten above — but
        // there's no reason to hold them once nothing reads them).
        if !dragging_move
            && !self.lift_anim.is_animating()
            && !self.ghost_alpha.is_animating()
            && !self.settle_dx.is_animating()
        {
            self.drag_visual_clip_ids.clear();
        }
        any
    }

    /// Whether `clip_id` is currently a target of the drag-visual tweens
    /// (grab lift / duplicate ghost / grid settle) — either live-dragged
    /// right now, or still easing out post-release. The render loop checks
    /// this per visible clip before applying [`Self::lift_amount`] /
    /// [`Self::ghost_alpha`] / [`Self::settle_dx_px`].
    pub fn is_drag_visual_target(&self, clip_id: &ClipId) -> bool {
        self.drag_visual_clip_ids.contains(clip_id)
    }

    /// 0..1 grab-lift progress — the render loop derives a 1-2px rise from
    /// this for every [`Self::is_drag_visual_target`] clip.
    pub fn lift_amount(&self) -> f32 {
        self.lift_anim.value().clamp(0.0, 1.0)
    }

    /// 0..1 alpha for a duplicate-drag ghost (1.0 = fully solid — the
    /// common case outside an alt-drag, so callers can multiply
    /// unconditionally).
    pub fn ghost_alpha(&self) -> f32 {
        self.ghost_alpha.value().clamp(0.0, 1.0)
    }

    /// Current grid-settle X offset in screen pixels (D15) — added to the
    /// dragged clip's already-final (snapped) drawn rect, decaying to 0.
    pub fn settle_dx_px(&self) -> f32 {
        self.settle_dx.value()
    }

    /// Current error-shake X offset in screen pixels (D17) — a decaying
    /// sine over the shake's 240ms, `None` when idle.
    pub fn error_shake_offset_px(&self) -> f32 {
        match self.error_shake.progress() {
            Some(p) => {
                let decay = 1.0 - p;
                (p * std::f32::consts::PI * 5.0).sin() * 3.0 * decay
            }
            None => 0.0,
        }
    }

    /// `Some(progress)` while the landing-line flash (D15) is active, plus
    /// its screen geometry: the snapped beat and the dragged selection's
    /// `(min_layer, max_layer)` span.
    pub fn landing_flash(&self) -> Option<(f32, Beats, usize, usize)> {
        self.landing_flash.progress().map(|p| {
            (
                p,
                self.landing_flash_beat,
                self.landing_flash_layers.0,
                self.landing_flash_layers.1,
            )
        })
    }

    /// True while any drag is in progress. Unity: IsDragging property.
    pub fn is_dragging(&self) -> bool {
        self.drag.is_active()
    }

    /// Current drag mode (read-only, for external queries like auto-scroll).
    /// Derived from the controller's payload (D11) — idle maps to `None`.
    pub fn drag_mode(&self) -> DragMode {
        self.drag
            .payload()
            .map(TimelineDrag::kind)
            .unwrap_or(DragMode::None)
    }

    /// B13 — the clip whose position/length the live readout should report,
    /// or `None` outside a move/trim gesture. Rubber-band (`RegionSelect`)
    /// has no single clip to report and is deliberately `None` here —
    /// `ClipId` wraps `Arc<str>`, so this clone is a refcount bump, not an
    /// allocation.
    pub fn drag_readout_clip_id(&self) -> Option<ClipId> {
        match self.drag.payload() {
            Some(TimelineDrag::Move(m)) => Some(m.anchor_clip_id.clone()),
            Some(TimelineDrag::TrimLeft(t)) | Some(TimelineDrag::TrimRight(t)) => {
                Some(t.clip_id.clone())
            }
            _ => None,
        }
    }

    /// Update the stored modifier state. Call from app before dispatching events.
    /// Unity reads Keyboard.current inline; Rust stores the latest state here.
    pub fn set_modifiers(&mut self, modifiers: Modifiers) {
        self.modifiers = modifiers;
    }

    /// Resolve feedback from the same geometry and captured payload used by edits.
    pub fn refresh_automation_feedback(
        &self,
        pos: Vec2,
        lanes: &[crate::panels::viewport::AutomationLaneScreen],
        ui_state: &mut UIState,
        viewport: &TimelineViewportPanel,
    ) {
        if !self.drag.is_active() && !viewport.tracks_rect().contains(pos) {
            ui_state.automation_feedback = None;
            return;
        }
        let snap = |x| {
            let raw = viewport.pixel_to_beat(x);
            (if self.modifiers.command {
                raw
            } else {
                viewport.snap_to_grid(raw)
            })
            .max(Beats::ZERO)
        };
        let resolved = if let Some(session) = self.drag.session() {
            // Capture identity and values from the gesture, never from a new hover hit.
            let locked = match &session.payload {
                TimelineDrag::AutomationPoint(s) => Some((
                    &s.target,
                    &s.param_id,
                    AutomationOperation::Point,
                    Some(s.last_beat),
                    None,
                    Some(s.last_value),
                    "Move point · Release to commit · Esc cancels",
                )),
                TimelineDrag::AutomationGroupMove(s) => s
                    .points
                    .iter()
                    .find(|p| {
                        p.target == s.grab_target
                            && p.param_id == s.grab_param_id
                            && p.original_beat == s.grabbed_original_beat
                            && p.original_value == s.grabbed_original_value
                    })
                    .map(|p| {
                        (
                            &s.grab_target,
                            &s.grab_param_id,
                            AutomationOperation::Point,
                            Some(p.last_beat),
                            None,
                            Some(p.last_value),
                            "Move selected points · Release to commit · Esc cancels",
                        )
                    }),
                TimelineDrag::AutomationSegmentBend(s) => Some((
                    &s.target,
                    &s.param_id,
                    AutomationOperation::Bend,
                    Some(s.left_beat),
                    None,
                    Some(s.left_value),
                    "Bend curve · Release to commit · Esc cancels",
                )),
                TimelineDrag::AutomationSegmentDrag(s) => Some((
                    &s.target,
                    &s.param_id,
                    AutomationOperation::Segment,
                    Some(s.left_beat),
                    Some(s.right_beat),
                    Some(s.last_left_value),
                    "Move segment · Release to commit · Esc cancels",
                )),
                TimelineDrag::AutomationDraw(s) => Some((
                    &s.target,
                    &s.param_id,
                    AutomationOperation::Draw,
                    None,
                    None,
                    None,
                    "Draw points · Release to commit · Esc cancels",
                )),
                TimelineDrag::AutomationLaneResize(s) => Some((
                    &s.target,
                    &s.param_id,
                    AutomationOperation::Resize,
                    None,
                    None,
                    None,
                    "Resize lane · Release to finish",
                )),
                TimelineDrag::AutomationMarquee(anchor) => lanes
                    .iter()
                    .find(|l| l.strip_rect.contains(Self::marquee_start(anchor, viewport)))
                    .map(|l| {
                        (
                            &l.target,
                            &l.param_id,
                            AutomationOperation::Marquee,
                            None,
                            None,
                            None,
                            "Select points · Release to finish",
                        )
                    }),
                _ => None,
            };
            locked.and_then(
                |(target, param_id, operation, point_beat, end, value, hint)| {
                    let lane = lanes
                        .iter()
                        .find(|l| l.target == *target && l.param_id == *param_id)?;
                    let end = end.or_else(|| {
                        if operation == AutomationOperation::Bend {
                            lane.dots
                                .iter()
                                .find(|d| Some(d.beat) > point_beat)
                                .map(|d| d.beat)
                        } else {
                            None
                        }
                    });
                    Some((
                        lane,
                        operation,
                        point_beat,
                        end,
                        point_beat.unwrap_or_else(|| snap(pos.x)),
                        value.unwrap_or_else(|| lane.value_at_y(pos.y)),
                        true,
                        hint,
                    ))
                },
            )
        } else {
            automation_hit_tester::resolve_feedback(
                pos,
                lanes,
                self.modifiers.alt,
                ui_state.automation_draw_mode,
            )
            .map(|(index, operation, point_index)| {
                let lane = &lanes[index];
                let point = point_index.map(|i| lane.dots[i]);
                // A tail is the flat line past the end points (or a
                // placeholder's): it has no right endpoint.
                let tail = operation == AutomationOperation::Segment
                    && matches!(
                        automation_hit_tester::hit_test_automation(pos, std::slice::from_ref(lane)),
                        Some(AutomationHit::Tail { .. })
                    );
                let end = point_index
                    .and_then(|i| lane.dots.get(i + 1))
                    .map(|p| p.beat)
                    .filter(|_| {
                        !tail
                            && matches!(
                                operation,
                                AutomationOperation::Segment | AutomationOperation::Bend
                            )
                    });
                let hint = match operation {
                    AutomationOperation::Point => {
                        "drag: move · click: delete · shift-click: select"
                    }
                    AutomationOperation::Segment if tail && point.is_none() => "Click: add point",
                    AutomationOperation::Segment if tail => {
                        "Drag end point up or down · Click: insert"
                    }
                    AutomationOperation::Segment if lane.whole_numbers => {
                        "Drag segment vertically · Stepped values · Click: insert"
                    }
                    AutomationOperation::Segment => {
                        "Drag segment vertically · Alt: bend · Click: insert"
                    }
                    AutomationOperation::Bend => "Alt-drag to bend curve · Click: insert",
                    AutomationOperation::Insert => {
                        "Click: add point · Drag: select · Shift-click: deselect"
                    }
                    AutomationOperation::Draw => "Drag to draw points · Cmd: unsnap",
                    AutomationOperation::Resize => "Drag to resize lane",
                    AutomationOperation::Title => "Lane title · Right-click: lane options",
                    AutomationOperation::Blocked => "Cannot edit: parameter has no editable range",
                    AutomationOperation::Marquee => "Drag to select points",
                };
                let (beat, value) = if operation == AutomationOperation::Point {
                    let p = point.expect("point resolver supplies dot");
                    (
                        p.beat,
                        lane.param_min + p.value_norm * (lane.param_max - lane.param_min),
                    )
                } else if matches!(
                    operation,
                    AutomationOperation::Segment | AutomationOperation::Bend
                ) {
                    // The ghost shows the value a click here will insert.
                    let beat = snap(pos.x);
                    let value = line_value_at_beat(lane, beat)
                        .map(|(value, _, _)| value)
                        .or(lane.placeholder_value)
                        .unwrap_or_else(|| lane.value_at_y(pos.y));
                    (beat, value)
                } else {
                    (snap(pos.x), lane.value_at_y(pos.y))
                };
                (
                    lane,
                    operation,
                    point.map(|p| p.beat),
                    end,
                    beat,
                    value,
                    false,
                    hint,
                )
            })
        };
        ui_state.automation_feedback = resolved.map(
            |(lane, operation, point_beat, segment_end_beat, beat, value, active, hint)| {
                let norm = if lane.param_max > lane.param_min {
                    (value - lane.param_min) / (lane.param_max - lane.param_min)
                } else {
                    0.0
                };
                AutomationFeedback {
                    target: lane.target.clone(),
                    param_id: lane.param_id.clone(),
                    operation,
                    point_beat,
                    segment_end_beat,
                    beat,
                    value,
                    active,
                    hint,
                    position: Vec2::new(viewport.beat_to_pixel(beat), lane.y_at_norm(norm)),
                }
            },
        );
    }

    /// Build the `TrimDrag` payload for a trim begin (P7.4): the grabbed
    /// clip's own pre-trim geometry (the snap-context half, formerly
    /// `begin_trim`) plus every selected clip's pre-trim geometry (the
    /// fan-over half, formerly `capture_trim_selection`) — locked clips
    /// skipped. The `on_begin_drag` select guard ensures the grabbed clip is
    /// in the selection before this runs.
    fn build_trim_drag(
        clip_id: ClipId,
        clip: &ClipRef,
        ui_state: &UIState,
        host: &dyn TimelineEditingHost,
    ) -> TrimDrag {
        let mut originals = Vec::new();
        for id in ui_state.get_selected_clip_ids() {
            if let Some(c) = host.find_clip_by_id(&id) {
                if c.is_locked {
                    continue;
                }
                originals.push(TrimOriginal {
                    clip_id: id.clone(),
                    start_beat: c.start_beat,
                    duration_beats: c.duration_beats,
                    in_point: c.in_point,
                    is_generator: c.is_generator,
                    is_looping: c.is_looping,
                });
            }
        }
        TrimDrag {
            clip_id,
            original_start_beat: clip.start_beat,
            original_duration_beats: clip.duration_beats,
            originals,
        }
    }

    // ────────────────────────────────────────────────────────────
    // DRAG POLLING
    // Unity InteractionOverlay.PollMoveDrag (lines 116-124), extended (B11) to
    // trim and region-select — edge autoscroll during move/trim/rubber-band
    // must keep advancing when the pointer is parked at the edge, not just
    // when a mouse-move event arrives. Called from app.rs frame loop every
    // frame while a drag of one of these kinds is in flight.
    // ────────────────────────────────────────────────────────────

    pub fn poll_drag(
        &mut self,
        mouse_screen_pos: Vec2,
        host: &mut dyn TimelineEditingHost,
        ui_state: &mut UIState,
        viewport: &mut TimelineViewportPanel,
    ) {
        match self.drag_mode() {
            // P7.5 entry-proof: every path that arms `TimelineDrag::Move`
            // also sets the anchor clip in the same synchronous call
            // (`begin_move_drag`), so the anchor is non-optional by
            // construction — this guard's old `.is_some()` condition was
            // proven vacuous and is gone (see the landing commit).
            DragMode::Move => {
                self.handle_move_drag(mouse_screen_pos, host, ui_state, viewport);
            }
            DragMode::TrimLeft => {
                self.handle_trim_left_drag(mouse_screen_pos, host, viewport);
            }
            DragMode::TrimRight => {
                self.handle_trim_right_drag(mouse_screen_pos, host, viewport);
            }
            DragMode::RegionSelect => {
                self.update_region_drag(mouse_screen_pos, ui_state, viewport, host);
            }
            // Automation gestures re-derive from the pointer every call, so
            // the per-frame poll is the same step a mouse move takes.
            mode if Self::autoscrolls_at_edge(mode) => {
                self.on_drag(mouse_screen_pos, host, ui_state, viewport);
            }
            _ => {}
        }
    }

    /// Whether the running drag has moved (or at this pointer will move) along
    /// the beat axis. Point and group drags do not move in time before the drag
    /// threshold, or at all when beat-locked, and must not scroll the view then.
    fn drag_moves_in_time(&self, mode: DragMode, pos: Vec2) -> bool {
        match (mode, self.drag.payload()) {
            (DragMode::AutomationPoint, Some(TimelineDrag::AutomationPoint(d))) => {
                d.resolver.moves_in_time(pos)
            }
            (DragMode::AutomationGroupMove, Some(TimelineDrag::AutomationGroupMove(s))) => {
                s.resolver.moves_in_time(pos)
            }
            _ => true,
        }
    }

    /// Automation gestures that follow the pointer across the timeline and so
    /// ride the same edge autoscroll as clip drags. Bend and lane resize do
    /// not move along the beat axis.
    pub fn autoscrolls_at_edge(mode: DragMode) -> bool {
        matches!(
            mode,
            DragMode::AutomationPoint
                | DragMode::AutomationMarquee
                | DragMode::AutomationGroupMove
                | DragMode::AutomationDraw
        )
    }

    // ────────────────────────────────────────────────────────────
    // POINTER EVENTS
    // ────────────────────────────────────────────────────────────

    /// Port of Unity InteractionOverlay.OnPointerClick (lines 130-217).
    pub fn on_pointer_click(
        &mut self,
        pos: Vec2,
        shift: bool,
        ctrl: bool,
        click_count: u32,
        is_right_button: bool,
        host: &mut dyn TimelineEditingHost,
        ui_state: &mut UIState,
        viewport: &TimelineViewportPanel,
    ) {
        if self.suppress_next_automation_click && !is_right_button {
            return;
        }
        if !is_right_button {
            // Alt-click is the straightening gesture. Handling it for both a
            // single and double click keeps platforms that emit a generic
            // Click before DoubleClick from inserting a transient point.
            if self.modifiers.alt && self.reset_automation_segment_bend(pos, host, viewport) {
                self.restore_pending_automation_press(host);
                return;
            }
            // The pending press owns this gesture; release commits it.
            if self.automation_press.is_some() {
                return;
            }
        }
        // Unit A: a click inside an
        // automation lane strip is handled entirely here — click-on-line adds
        // a breakpoint and click-on-dot selects it. It never falls through
        // to clip/region logic below.
        //
        // a right-click on a lane (strip, segment, or dot) opens the
        // lane's context menu (Clear Automation / Remove Lane) instead of
        // falling through to clip/track right-click handling — resolved
        // BEFORE the left-click automation handler below so a right-click
        // never mutates a point.
        if is_right_button {
            let lanes = viewport.automation_lane_screens();
            if let Some(hit) = automation_hit_tester::hit_test_automation(pos, lanes) {
                let lane = &lanes[hit.lane_index()];
                let same_lane = |p: &UiAutomationPointRef| {
                    p.target == lane.target && p.param_id == lane.param_id
                };
                let is_selected_lane = ui_state.selected_automation_points.iter().any(same_lane)
                    || ui_state
                        .selected_automation_point
                        .as_ref()
                        .is_some_and(same_lane)
                    || ui_state
                        .automation_time_selection
                        .as_ref()
                        .is_some_and(|range| {
                            range.lanes.iter().any(|(target, param)| {
                                *target == lane.target && *param == lane.param_id
                            })
                        });
                if !is_selected_lane {
                    ui_state.clear_automation_selection();
                }
                if let AutomationHit::Dot { dot_index, .. } = hit {
                    let point = UiAutomationPointRef {
                        target: lane.target.clone(),
                        param_id: lane.param_id.clone(),
                        beat: lane.dots[dot_index].beat,
                        value_norm: lane.dots[dot_index].value_norm,
                    };
                    if !ui_state.automation_point_selected(
                        &point.target,
                        &point.param_id,
                        point.beat,
                        point.value_norm,
                    ) {
                        ui_state.clear_automation_selection();
                        ui_state.selected_automation_point = Some(point);
                    }
                }
                ui_state.automation_paste_context =
                    Some((lane.target.clone(), lane.param_id.clone()));
                host.on_automation_lane_right_click(&lane.target, &lane.param_id, pos);
                return;
            }
        }

        if !is_right_button
            && self.handle_automation_click(pos, click_count, host, ui_state, viewport)
        {
            return;
        }

        ui_state.clear_automation_selection();

        let hit = self.hit_test_at(pos, viewport);

        if hit.is_none() {
            // ── NO HIT: empty area clicked ──
            // Unity line 147: clear region
            ui_state.clear_region();

            let layer_index = viewport.layer_at_y(pos.y);

            // Unity: InputHandler.HandleEmptyAreaRightClick → ShowLayerContextMenu
            if is_right_button {
                if let Some(layer) = layer_index {
                    let beat = viewport.pixel_to_beat(pos.x);
                    host.on_track_right_click(beat, layer, pos);
                }
                return;
            }

            // Unity lines 152-162: double-click on empty area → create clip
            if click_count >= 2
                && let Some(layer) = layer_index
            {
                let beat = viewport.floor_to_grid(viewport.pixel_to_beat(pos.x));
                if let Some(clip_id) =
                    host.create_clip_at_position(beat, layer, viewport.clip_creation_step())
                {
                    let lid = host.layer_id_at_index(layer).unwrap_or_default();
                    ui_state.select_clip(clip_id.clone(), lid);
                    host.on_clip_selected(&clip_id);
                }
                return;
            }

            // Unity lines 165-188: single click on empty area
            if let Some(layer) = layer_index {
                let beat = viewport.pixel_to_beat(pos.x);
                let snapped = viewport.snap_to_grid(beat);

                if shift {
                    // Unity line 180: Shift+Click → extend region
                    select_region_to(snapped, layer, ui_state, host);
                } else {
                    // Unity line 184: bare click → set insert cursor
                    let lid = host.layer_id_at_index(layer).unwrap_or_default();
                    ui_state.set_insert_cursor(snapped, lid);
                }

                // Unity line 187: always inspect layer on empty click
                host.inspect_layer(layer);
            }
            return;
        }

        let hit = hit.unwrap();

        // Unity line 195: locked clips — ignore
        if self.clip_is_locked(&hit.clip_id, viewport) {
            return;
        }

        // Unity lines 198-204: right-click → context menu
        if is_right_button {
            if !ui_state.is_selected(&hit.clip_id) {
                let lid = host.layer_id_at_index(hit.layer_index).unwrap_or_default();
                ui_state.select_clip(hit.clip_id.clone(), lid);
            }
            host.on_clip_right_click(&hit.clip_id, pos);
            return;
        }

        // Unity lines 206-214: selection modifiers
        if shift {
            // D2: shift-click on a CLIP is a clip-range selection (contiguous
            // whole clips on the anchor's layer), not a region — the empty-area
            // shift path above still calls `select_region_to`; only this
            // clip-hit arm changes (S1/S3's root).
            select_clip_range_to(&hit.clip_id, ui_state, host);
        } else if ctrl {
            // Unity lines 208-212: Ctrl → toggle multi-select. D1: no longer
            // synthesises a region from the clip set — a multi-clip selection
            // is a pure `Clips` selection (the redundant region band is gone;
            // begins the S1 fix). `toggle_clip_selection` owns the whole update.
            let lid = host.layer_id_at_index(hit.layer_index).unwrap_or_default();
            ui_state.toggle_clip_selection(hit.clip_id.clone(), lid);
        } else {
            // Unity line 214: bare click → select single
            let lid = host.layer_id_at_index(hit.layer_index).unwrap_or_default();
            ui_state.select_clip(hit.clip_id.clone(), lid);
        }

        // Unity line 216: always notify host
        host.on_clip_selected(&hit.clip_id);
    }

    // ────────────────────────────────────────────────────────────
    // AUTOMATION LANE EDITING
    // ────────────────────────────────────────────────────────────

    /// Handle a click/double-click that lands inside an automation lane
    /// strip. Returns `true` when the click was handled (caller must
    /// return without falling through to clip/region logic).
    ///
    /// - Plain click on an existing dot → delete it (Live). Shift toggles its
    ///   selection; Alt/Cmd or pencil mode only select it.
    /// - The second click of a double-click that just deleted a dot is
    ///   swallowed, so it cannot land on the bare strip and re-add a point.
    /// - Click (or double-click) on bare strip → add a breakpoint at the
    ///   clicked beat/value (grid-snapped unless Cmd is held; `Hold` shape
    ///   for whole-numbers params, `Linear` otherwise — section 8).
    fn handle_automation_click(
        &mut self,
        pos: Vec2,
        click_count: u32,
        host: &mut dyn TimelineEditingHost,
        ui_state: &mut UIState,
        viewport: &TimelineViewportPanel,
    ) -> bool {
        // The platform sends a plain Click and then the DoubleClick for the
        // second press, so both are swallowed; the DoubleClick ends the guard.
        let after_delete = self.within_dot_delete_window(pos);
        if !after_delete || click_count >= 2 {
            self.last_dot_delete = None;
        }
        let lanes = viewport.automation_lane_screens();
        if automation_hit_tester::resolve_feedback(
            pos,
            lanes,
            self.modifiers.alt,
            ui_state.automation_draw_mode,
        )
        .is_some_and(|(_, op, _)| {
            matches!(
                op,
                AutomationOperation::Title
                    | AutomationOperation::Resize
                    | AutomationOperation::Blocked
            )
        }) {
            return true;
        }
        let Some(hit) = automation_hit_tester::hit_test_automation(pos, lanes) else {
            return false;
        };
        if after_delete {
            return true;
        }
        match hit {
            AutomationHit::Dot {
                lane_index,
                dot_index,
            } => {
                let lane = &lanes[lane_index];
                let dot = lane.dots[dot_index];
                let plain = !self.modifiers.shift
                    && !self.modifiers.alt
                    && !self.modifiers.command
                    && !ui_state.automation_draw_mode;
                if plain && self.is_recent_insert(&lane.target, &lane.param_id, dot.beat) {
                    // The second click of a double-click on empty space lands
                    // on the point the first one made.
                    self.select_automation_dot(lane, dot, ui_state);
                } else if plain {
                    host.remove_automation_point(
                        &lane.target,
                        &lane.param_id,
                        dot.beat,
                        dot.value_norm,
                    );
                    let point = UiAutomationPointRef {
                        target: lane.target.clone(),
                        param_id: lane.param_id.clone(),
                        beat: dot.beat,
                        value_norm: dot.value_norm,
                    };
                    if ui_state.selected_automation_point.as_ref() == Some(&point) {
                        ui_state.selected_automation_point = None;
                    }
                    ui_state.selected_automation_points.retain(|p| p != &point);
                    ui_state.automation_time_selection = None;
                    ui_state.automation_insert_beat = Some(dot.beat);
                    ui_state.automation_paste_context =
                        Some((lane.target.clone(), lane.param_id.clone()));
                    self.last_dot_delete = Some((pos, Instant::now()));
                } else {
                    self.select_automation_dot(lane, dot, ui_state);
                }
            }
            // A plain CLICK (no drag) on a segment inserts a new breakpoint
            // there, same as clicking bare strip — Ableton's "click the line
            // to add a point" behavior. `Segment` only changes DRAG-begin
            // routing (`begin_automation_drag`), not click routing.
            AutomationHit::Strip { .. }
            | AutomationHit::Segment { .. }
            | AutomationHit::Tail { .. } => {
                // On the curve: between two points or on a tail past the end
                // points. A placeholder's line has no point, so it inserts the
                // lane's current value through the plain add.
                let is_segment = match hit {
                    AutomationHit::Segment { .. } => true,
                    AutomationHit::Tail { dot_index, .. } => dot_index.is_some(),
                    _ => false,
                };
                let lane = &lanes[hit.lane_index()];
                ui_state.clear_automation_selection();
                ui_state.automation_paste_context =
                    Some((lane.target.clone(), lane.param_id.clone()));
                // Shift-click is explicit deselection; a bare drag remains marquee.
                if self.modifiers.shift {
                    return true;
                }
                let raw_beat = viewport.pixel_to_beat(pos.x);
                let beat = if self.modifiers.command {
                    raw_beat
                } else {
                    viewport.snap_to_grid(raw_beat)
                }
                .max(Beats::ZERO);
                if is_segment
                    && let Some(dot) = lane.dots.iter().find(|dot| dot.beat == beat).copied()
                {
                    self.select_automation_dot(lane, dot, ui_state);
                    return true;
                }
                let value = if is_segment {
                    line_value_at_beat(lane, beat)
                        .map(|(value, _, _)| value)
                        .unwrap_or_else(|| lane.value_at_y(pos.y))
                } else {
                    lane.placeholder_value
                        .unwrap_or_else(|| lane.value_at_y(pos.y))
                };
                let shape = if lane.whole_numbers {
                    UiSegmentShape::Hold
                } else {
                    UiSegmentShape::Linear
                };
                ui_state.automation_time_selection = None;
                ui_state.automation_insert_beat = Some(beat);
                if is_segment {
                    if let Some(curve_value) = host.insert_automation_point_on_curve(
                        &lane.target,
                        &lane.param_id,
                        beat,
                        shape,
                    ) {
                        // The host is authoritative about the sampled value;
                        // keep the selection aligned with the committed point.
                        let value = curve_value;
                        ui_state.selected_automation_point = Some(UiAutomationPointRef {
                            target: lane.target.clone(),
                            param_id: lane.param_id.clone(),
                            beat,
                            value_norm: (value - lane.param_min)
                                / (lane.param_max - lane.param_min),
                        });
                        self.last_point_insert = Some(RecentInsert {
                            target: lane.target.clone(),
                            param_id: lane.param_id.clone(),
                            beat,
                            when: Instant::now(),
                        });
                    }
                    return true;
                }
                host.add_automation_point(&lane.target, &lane.param_id, beat, value, shape);
                ui_state.selected_automation_point = Some(UiAutomationPointRef {
                    target: lane.target.clone(),
                    param_id: lane.param_id.clone(),
                    beat,
                    value_norm: (value - lane.param_min) / (lane.param_max - lane.param_min),
                });
                self.last_point_insert = Some(RecentInsert {
                    target: lane.target.clone(),
                    param_id: lane.param_id.clone(),
                    beat,
                    when: Instant::now(),
                });
            }
        }
        true
    }

    /// Select an existing breakpoint the way a click on its dot does: Shift
    /// toggles it in the multi-selection, a plain click makes it the only one.
    fn select_automation_dot(
        &self,
        lane: &crate::panels::viewport::AutomationLaneScreen,
        dot: crate::panels::viewport::AutomationDotScreen,
        ui_state: &mut UIState,
    ) {
        let point = UiAutomationPointRef {
            target: lane.target.clone(),
            param_id: lane.param_id.clone(),
            beat: dot.beat,
            value_norm: dot.value_norm,
        };
        if self.modifiers.shift {
            if let Some(single) = ui_state.selected_automation_point.take()
                && !ui_state.selected_automation_points.contains(&single)
            {
                ui_state.selected_automation_points.push(single);
            }
            if let Some(index) = ui_state
                .selected_automation_points
                .iter()
                .position(|p| p == &point)
            {
                ui_state.selected_automation_points.remove(index);
            } else {
                ui_state.selected_automation_points.push(point);
            }
        } else {
            ui_state.selected_automation_points.clear();
            ui_state.selected_automation_point = Some(point);
        }
        ui_state.automation_time_selection = None;
        ui_state.automation_insert_beat = Some(dot.beat);
        ui_state.automation_paste_context = Some((lane.target.clone(), lane.param_id.clone()));
    }

    /// Hit-test `press_pos` against automation lane strips for a drag begin.
    /// Returns `true` when an existing dot was grabbed (caller must return
    /// without falling through to clip drag logic). A drag press on bare
    /// strip area is NOT handled here — section 7's "click on line adds a point" is
    /// a click action (`handle_automation_click`), not a drag-begin; the
    /// `DragBegin` event only fires for what the platform layer already
    /// distinguishes as a drag gesture, so a plain click on the strip is
    /// routed through `on_pointer_click` instead.
    fn begin_automation_drag(
        &mut self,
        press_pos: Vec2,
        host: &mut dyn TimelineEditingHost,
        ui_state: &mut UIState,
        viewport: &TimelineViewportPanel,
    ) -> bool {
        let mut lanes = std::borrow::Cow::Borrowed(viewport.automation_lane_screens());
        if automation_hit_tester::resolve_feedback(
            press_pos,
            &lanes,
            self.modifiers.alt,
            ui_state.automation_draw_mode,
        )
        .is_some_and(|(_, op, _)| {
            matches!(
                op,
                AutomationOperation::Title
                    | AutomationOperation::Resize
                    | AutomationOperation::Blocked
            )
        }) {
            return true;
        }
        let forced_hit = self.forced_automation_hit.take();
        if let Some(hit) = forced_hit
            && let Some(lane) = lanes.to_mut().get_mut(hit.lane_index())
        {
            // A frame may have projected the draft. Restore dot geometry
            // before capturing the original segment endpoints.
            let original = host.automation_lane_points(&lane.target, &lane.param_id);
            lane.dots.retain_mut(|dot| {
                let point = original
                    .as_deref()
                    .unwrap_or_default()
                    .iter()
                    .find(|point| {
                        point.0 == dot.beat
                            && BitmapSlider::value_to_normalized(
                                point.1,
                                lane.param_min,
                                lane.param_max,
                            ) == dot.value_norm
                    });
                if let Some(point) = point {
                    dot.shape = point.2;
                }
                point.is_some()
            });
        }
        let hit =
            forced_hit.or_else(|| automation_hit_tester::hit_test_automation(press_pos, &lanes));
        let Some(hit) = hit else {
            return false;
        };

        if !matches!(
            hit,
            AutomationHit::Strip { .. }
                | AutomationHit::Tail {
                    dot_index: None,
                    ..
                }
        ) || ui_state.automation_draw_mode
        {
            ui_state.automation_time_selection = None;
            let beat = viewport.pixel_to_beat(press_pos.x);
            ui_state.automation_insert_beat = Some(
                if self.modifiers.command {
                    beat
                } else {
                    viewport.snap_to_grid(beat)
                }
                .max(Beats::ZERO),
            );
        }

        // Pencil/draw mode overrides ALL other drag routing while active —
        // Ableton's pencil draws regardless of whether the press happened to
        // land on an existing dot/segment (P4 Unit B, section 7's "Draw mode").
        if ui_state.automation_draw_mode {
            self.begin_automation_draw(hit.lane_index(), press_pos, &lanes, host, viewport);
            return true;
        }

        match hit {
            // A tail press drags the end point alone, its beat locked.
            AutomationHit::Dot {
                lane_index,
                dot_index,
            }
            | AutomationHit::Tail {
                lane_index,
                dot_index: Some(dot_index),
            } => {
                let beat_locked = matches!(hit, AutomationHit::Tail { .. });
                let lane = &lanes[lane_index];
                let dot = lane.dots[dot_index];
                let point_ref = UiAutomationPointRef {
                    target: lane.target.clone(),
                    param_id: lane.param_id.clone(),
                    beat: dot.beat,
                    value_norm: dot.value_norm,
                };
                // Grabbing a dot that's part of an active multi-selection (2+
                // members) moves the WHOLE group; otherwise this is a plain
                // single-point drag and any stale marquee selection is
                // cleared (mirrors clip selection's "bare click selects just
                // this one").
                if !beat_locked
                    && ui_state.selected_automation_points.len() > 1
                    && ui_state.selected_automation_points.contains(&point_ref)
                {
                    self.begin_automation_group_drag(press_pos, &lanes, ui_state, host, viewport);
                    return true;
                }
                ui_state.selected_automation_points.clear();

                let Some(original_points) =
                    host.automation_lane_points(&lane.target, &lane.param_id)
                else {
                    return true;
                };
                let Some(&original) = original_points.iter().find(|point| {
                    point.0 == dot.beat
                        && BitmapSlider::value_to_normalized(
                            point.1,
                            lane.param_min,
                            lane.param_max,
                        ) == dot.value_norm
                }) else {
                    return true;
                };
                let value = original.1;
                let source_index = original_points
                    .iter()
                    .position(|p| p.0 == original.0 && p.1 == original.1)
                    .unwrap_or(0);
                let prev_beat = source_index
                    .checked_sub(1)
                    .map_or(Beats::ZERO, |i| original_points[i].0);
                let next_beat = original_points.get(source_index + 1).map(|p| p.0);
                let state = AutomationDragState {
                    target: lane.target.clone(),
                    param_id: lane.param_id.clone(),
                    original,
                    last_beat: dot.beat,
                    last_value: value,
                    resolver: DragResolver::new(
                        press_pos,
                        viewport.pixel_to_beat(press_pos.x),
                        beat_locked,
                    ),
                    prev_beat,
                    next_beat,
                    working: original_points.clone(),
                    original_points,
                };
                self.capture_automation_lane_snapshot(
                    host,
                    lane.target.clone(),
                    lane.param_id.clone(),
                );
                ui_state.selected_automation_point = Some(point_ref);
                self.drag
                    .start(TimelineDrag::AutomationPoint(state), press_pos);
                true
            }
            AutomationHit::Segment {
                lane_index,
                left_dot_index,
            } => {
                self.begin_automation_segment_drag(
                    lane_index,
                    left_dot_index,
                    press_pos,
                    &lanes,
                    host,
                );
                true
            }
            // A placeholder's line has nothing to drag: the press is a marquee.
            AutomationHit::Strip { lane_index }
            | AutomationHit::Tail {
                lane_index,
                dot_index: None,
            } => {
                // Shift adds to what is selected now; a plain press replaces it.
                let mut base = Vec::new();
                if self.modifiers.shift {
                    base.extend(ui_state.selected_automation_points.iter().cloned());
                    if let Some(single) = ui_state.selected_automation_point.clone()
                        && !base.contains(&single)
                    {
                        base.push(single);
                    }
                }
                ui_state.clear_automation_selection();
                let lane = &lanes[lane_index];
                ui_state.automation_paste_context =
                    Some((lane.target.clone(), lane.param_id.clone()));
                ui_state
                    .selected_automation_points
                    .extend(base.iter().cloned());
                let anchor = AutomationMarqueeAnchor {
                    beat: viewport.pixel_to_beat(press_pos.x),
                    content_y: press_pos.y + viewport.scroll_y_px(),
                    base,
                };
                self.drag
                    .start(TimelineDrag::AutomationMarquee(anchor), press_pos);
                self.update_automation_time_selection(press_pos, press_pos, ui_state, viewport);
                host.redraw();
                true
            }
        }
    }

    fn reset_automation_segment_bend(
        &mut self,
        pos: Vec2,
        host: &mut dyn TimelineEditingHost,
        viewport: &TimelineViewportPanel,
    ) -> bool {
        let lanes = viewport.automation_lane_screens();
        let Some(AutomationHit::Segment {
            lane_index,
            left_dot_index,
        }) = automation_hit_tester::hit_test_automation(pos, lanes)
        else {
            return false;
        };
        let lane = &lanes[lane_index];
        let left = lane.dots[left_dot_index];
        let Some(old_points) = host.automation_lane_points(&lane.target, &lane.param_id) else {
            return true;
        };
        let left_value = lane.param_min + left.value_norm * (lane.param_max - lane.param_min);
        let Some(point_index) = old_points
            .iter()
            .position(|point| point.0 == left.beat && (point.1 - left_value).abs() <= f32::EPSILON)
        else {
            return true;
        };
        let old_shape = old_points[point_index].2;
        if lane.whole_numbers
            || !matches!(
                old_shape,
                UiSegmentShape::Curved(_) | UiSegmentShape::CurvedRange { .. }
            )
        {
            return true;
        }
        let new_shape = shape_with_bend(old_shape, 0.0);
        if new_shape == old_shape {
            return true;
        }
        let mut working = old_points.clone();
        working[point_index].2 = new_shape;
        host.set_automation_lane_preview(&lane.target, &lane.param_id, &working);
        host.clear_automation_previews();
        host.commit_automation_point_move(
            &lane.target,
            &lane.param_id,
            (left.beat, left_value, old_shape),
            (left.beat, left_value, new_shape),
        );
        true
    }

    /// Begin a marquee-group drag: capture every currently multi-selected
    /// point's original (beat, value, shape) plus its own lane's param
    /// range — points can span multiple lanes/params at once, so only the
    /// NORMALIZED delta is shared, not the raw param-range delta.
    fn begin_automation_group_drag(
        &mut self,
        press_pos: Vec2,
        lanes: &[crate::panels::viewport::AutomationLaneScreen],
        ui_state: &UIState,
        host: &mut dyn TimelineEditingHost,
        viewport: &TimelineViewportPanel,
    ) {
        let Some(AutomationHit::Dot {
            lane_index: grab_lane_index,
            dot_index: grab_dot_index,
        }) = automation_hit_tester::hit_test_automation(press_pos, lanes)
        else {
            return;
        };
        let grab_lane = &lanes[grab_lane_index];
        let mut points = Vec::with_capacity(ui_state.selected_automation_points.len());
        let mut group_lanes: Vec<AutomationGroupLaneState> = Vec::new();
        for r in &ui_state.selected_automation_points {
            // The selected set can contain points outside the current visible
            // beat range AND lanes whose track has scrolled off screen — the
            // screen list is culled, so fall back to the viewport's unculled
            // lane model for the value range. Dropping the point here would
            // silently exclude it from the move AND from the rebuilt
            // selection at drag end. The full host snapshot supplies the
            // authoritative point data either way.
            let (param_min, param_max, whole_numbers) = match lanes
                .iter()
                .find(|l| l.target == r.target && l.param_id == r.param_id)
            {
                Some(lane) => (lane.param_min, lane.param_max, lane.whole_numbers),
                None => match viewport.automation_lane_meta(&r.target, &r.param_id) {
                    Some(meta) => meta,
                    // Lane genuinely gone (deleted since selection) —
                    // skipping is correct, not a silent drop.
                    None => continue,
                },
            };
            let snapshot_index =
                self.capture_automation_lane_snapshot(host, r.target.clone(), r.param_id.clone());
            let Some(original_points) = self.automation_lane_snapshots[snapshot_index]
                .points
                .as_ref()
            else {
                continue;
            };
            let Some(&(original_beat, value, shape)) = original_points.iter().find(|point| {
                point.0 == r.beat
                    && BitmapSlider::value_to_normalized(point.1, param_min, param_max)
                        == r.value_norm
            }) else {
                continue;
            };
            let lane_slot = if let Some(index) = group_lanes
                .iter()
                .position(|l| l.snapshot_index == snapshot_index)
            {
                index
            } else {
                group_lanes.push(AutomationGroupLaneState {
                    target: r.target.clone(),
                    param_id: r.param_id.clone(),
                    snapshot_index,
                    working: Vec::with_capacity(original_points.len()),
                });
                group_lanes.len() - 1
            };
            points.push(AutomationGroupPointState {
                target: r.target.clone(),
                param_id: r.param_id.clone(),
                lane_index: lane_slot,
                original_beat,
                last_beat: original_beat,
                original_value: value,
                shape,
                param_min,
                param_max,
                whole_numbers,
                last_value: value,
            });
        }
        if points.is_empty() {
            self.automation_lane_snapshots.clear();
            return;
        }
        let grabbed_dot_beat = lanes[grab_lane_index].dots[grab_dot_index].beat;
        let Some((grabbed_original_beat, grabbed_original_value)) = points
            .iter()
            .find(|point| {
                point.target == grab_lane.target
                    && point.param_id == grab_lane.param_id
                    && point.original_beat == grabbed_dot_beat
                    && BitmapSlider::value_to_normalized(
                        point.original_value,
                        grab_lane.param_min,
                        grab_lane.param_max,
                    ) == lanes[grab_lane_index].dots[grab_dot_index].value_norm
            })
            .map(|point| (point.original_beat, point.original_value))
        else {
            self.automation_lane_snapshots.clear();
            return;
        };
        // The group stops at beat zero and at the nearest unselected point on
        // any lane, as the arrow-key nudge does (`room_to_unselected`).
        let (mut room_left, mut room_right) = (f64::INFINITY, f64::INFINITY);
        for p in &points {
            let Some(lane_points) = self.automation_lane_snapshots
                [group_lanes[p.lane_index].snapshot_index]
                .points
                .as_ref()
            else {
                continue;
            };
            let Some(index) = lane_points
                .iter()
                .position(|q| q.0 == p.original_beat && q.1 == p.original_value)
            else {
                continue;
            };
            let (left, right) = room_to_unselected(
                index,
                p.original_beat.0,
                lane_points.iter().enumerate().map(|(j, q)| {
                    let selected = points.iter().any(|s| {
                        s.lane_index == p.lane_index
                            && s.original_beat == q.0
                            && s.original_value == q.1
                    });
                    (j, q.0.0, selected)
                }),
            );
            room_left = room_left.min(left);
            room_right = room_right.min(right);
        }
        let min_beat = points
            .iter()
            .map(|p| p.original_beat.0)
            .fold(f64::INFINITY, f64::min);
        let state = AutomationGroupDragState {
            grab_target: grab_lane.target.clone(),
            grab_param_id: grab_lane.param_id.clone(),
            grabbed_original_beat,
            grabbed_original_value,
            resolver: DragResolver::new(press_pos, viewport.pixel_to_beat(press_pos.x), false),
            min_beat,
            room_left,
            room_right,
            points,
            lanes: group_lanes,
        };
        self.drag
            .start(TimelineDrag::AutomationGroupMove(state), press_pos);
    }

    /// Begin a pencil/draw stroke on `lane_index`'s lane: reads the FULL
    /// pre-stroke point list via `host.automation_lane_points` (NOT
    /// `AutomationLaneScreen::dots`, which is culled to the visible beat
    /// range — using it here would silently drop off-screen points from the
    /// eventual install), seeds `working` from it, then writes the first
    /// grid step under the press.
    fn begin_automation_draw(
        &mut self,
        lane_index: usize,
        press_pos: Vec2,
        lanes: &[crate::panels::viewport::AutomationLaneScreen],
        host: &mut dyn TimelineEditingHost,
        viewport: &TimelineViewportPanel,
    ) {
        let lane = &lanes[lane_index];
        let old_points = host.automation_lane_points(&lane.target, &lane.param_id);
        self.capture_automation_lane_snapshot(host, lane.target.clone(), lane.param_id.clone());
        let working = old_points.clone().unwrap_or_default();
        let state = AutomationDrawState {
            target: lane.target.clone(),
            param_id: lane.param_id.clone(),
            new_point_shape: if lane.whole_numbers {
                UiSegmentShape::Hold
            } else {
                UiSegmentShape::Linear
            },
            old_points,
            working,
            sweep_scratch: Vec::with_capacity(32),
            last_sample: None,
        };
        self.drag
            .start(TimelineDrag::AutomationDraw(state), press_pos);
        // Falls through to the shared per-frame writer so the press itself
        // draws a point immediately, not just subsequent movement.
        self.write_automation_draw_step(press_pos, host, viewport);
    }

    /// Write (insert-or-overwrite) the grid-snapped point under `pos` into
    /// the in-progress draw stroke, then push the whole working list to the
    /// live preview. Re-resolves the stroke's lane fresh each call (handles
    /// mid-stroke scroll), matching every other drag handler's discipline.
    fn write_automation_draw_step(
        &mut self,
        pos: Vec2,
        host: &mut dyn TimelineEditingHost,
        viewport: &TimelineViewportPanel,
    ) {
        let Some(TimelineDrag::AutomationDraw(state)) = self.drag.payload() else {
            return;
        };
        let (target, param_id) = (state.target.clone(), state.param_id.clone());
        let lanes = viewport.automation_lane_screens();
        let Some(lane) = lanes
            .iter()
            .find(|l| l.target == target && l.param_id == param_id)
        else {
            return;
        };

        let beat = if self.modifiers.command {
            viewport.pixel_to_beat(pos.x)
        } else {
            viewport.snap_to_grid(viewport.pixel_to_beat(pos.x))
        }
        .max(Beats::ZERO);
        let value = lane.value_at_y(pos.y);

        let command_bypass = self.modifiers.command;
        let Some(TimelineDrag::AutomationDraw(state)) = self.drag.payload_mut() else {
            return;
        };
        let current = (beat, value);
        if state.last_sample == Some(current) {
            return;
        }
        apply_draw_sweep(
            &mut state.sweep_scratch,
            state.last_sample,
            current,
            state.new_point_shape,
            lane.grid_step,
            command_bypass,
        );
        state.last_sample = Some(current);
        rebuild_draw_interval(
            state.old_points.as_deref().unwrap_or(&[]),
            &mut state.working,
            &state.sweep_scratch,
        );
        host.set_automation_lane_preview(&target, &param_id, &state.working);
    }

    /// Commit a finished draw stroke as one undo entry — no-op if the
    /// working list ended up identical to the pre-stroke set.
    fn commit_automation_draw(
        &mut self,
        state: AutomationDrawState,
        host: &mut dyn TimelineEditingHost,
    ) {
        let unchanged = state.old_points.as_deref() == Some(state.working.as_slice())
            || (state.old_points.is_none() && state.working.is_empty());
        if !unchanged {
            host.commit_automation_draw_stroke(
                &state.target,
                &state.param_id,
                state.working,
                state.old_points,
            );
        }
    }

    /// Grab a segment for either an Alt-drag curve bend or a plain vertical
    /// drag (P4 Unit B, section 7). Curve-bend is gated off for `whole_numbers`
    /// lanes (section 8: enum/int params author with `Hold`, so bending one would
    /// silently change its runtime sampling from a step to a curve) — Alt on
    /// such a lane falls back to the vertical-drag gesture instead of a no-op.
    fn begin_automation_segment_drag(
        &mut self,
        lane_index: usize,
        left_dot_index: usize,
        press_pos: Vec2,
        lanes: &[crate::panels::viewport::AutomationLaneScreen],
        host: &mut dyn TimelineEditingHost,
    ) {
        let lane = &lanes[lane_index];
        let snapshot_index =
            self.capture_automation_lane_snapshot(host, lane.target.clone(), lane.param_id.clone());
        let left = lane.dots[left_dot_index];
        let right = lane.dots[left_dot_index + 1];
        let Some(snapshot) = self.automation_lane_snapshots[snapshot_index]
            .points
            .as_ref()
        else {
            return;
        };
        let Some(left_value) = snapshot
            .iter()
            .find(|point| {
                point.0 == left.beat
                    && BitmapSlider::value_to_normalized(point.1, lane.param_min, lane.param_max)
                        == left.value_norm
            })
            .map(|point| point.1)
        else {
            return;
        };
        let Some(right_value) = snapshot
            .iter()
            .find(|point| {
                point.0 == right.beat
                    && BitmapSlider::value_to_normalized(point.1, lane.param_min, lane.param_max)
                        == right.value_norm
            })
            .map(|point| point.1)
        else {
            return;
        };

        if self.modifiers.alt && !lane.whole_numbers {
            let original_bend = shape_bend(left.shape);
            let state = AutomationSegmentBendState {
                target: lane.target.clone(),
                param_id: lane.param_id.clone(),
                left_beat: left.beat,
                left_value,
                original_shape: left.shape,
                snapshot_index,
                last_pointer_y: press_pos.y,
                accumulated_delta_px: 0.0,
                direction_sign: (right_value - left_value).signum(),
                original_bend,
                last_bend: original_bend,
                working: snapshot.clone(),
            };
            self.drag
                .start(TimelineDrag::AutomationSegmentBend(state), press_pos);
        } else {
            let state = AutomationSegmentDragState {
                target: lane.target.clone(),
                param_id: lane.param_id.clone(),
                left_beat: left.beat,
                left_original_value: left_value,
                left_shape: left.shape,
                right_beat: right.beat,
                right_original_value: right_value,
                right_shape: right.shape,
                resolver: DragResolver::new(press_pos, Beats::ZERO, true),
                last_left_value: left_value,
                last_right_value: right_value,
            };
            self.drag
                .start(TimelineDrag::AutomationSegmentDrag(state), press_pos);
        }
    }

    /// Live-preview an in-progress automation point drag (`DragMode::
    /// AutomationPoint`). Value changes are relative to the grab position, so
    /// the point does not jump to the pointer.
    fn handle_automation_drag(
        &mut self,
        pos: Vec2,
        host: &mut dyn TimelineEditingHost,
        ui_state: &mut UIState,
        viewport: &TimelineViewportPanel,
    ) {
        let Some(TimelineDrag::AutomationPoint(d)) = self.drag.payload() else {
            return;
        };
        let (target, param_id) = (d.target.clone(), d.param_id.clone());
        let lanes = viewport.automation_lane_screens();
        let Some(lane) = lanes
            .iter()
            .find(|l| l.target == target && l.param_id == param_id)
        else {
            return;
        };

        let input = ResolveInput {
            pos,
            pointer_beat: viewport.pixel_to_beat(pos.x),
            grid_step: viewport.grid_step(),
            command: self.modifiers.command,
            shift: self.modifiers.shift,
            curve_height: lane.curve_rect().height,
        };
        let Some(TimelineDrag::AutomationPoint(drag)) = self.drag.payload_mut() else {
            return;
        };
        let resolved = drag.resolver.resolve(&input);
        let to_beat = clamp_single_beat(
            drag.original.0 + Beats(resolved.beat_delta),
            drag.prev_beat,
            drag.next_beat,
        );
        let to_value = apply_value(
            drag.original.1,
            resolved.norm_delta,
            lane.param_min,
            lane.param_max,
            lane.whole_numbers,
        );
        drag.working.clone_from(&drag.original_points);
        if to_beat != drag.original.0 || to_value != drag.original.1 {
            insert_moved_point(
                &mut drag.working,
                drag.original.0,
                drag.original.1,
                to_beat,
                to_value,
                drag.original.2,
            );
        }
        host.set_automation_lane_preview(&target, &param_id, &drag.working);
        drag.last_beat = to_beat;
        drag.last_value = to_value;
        if let Some(selected) = ui_state.selected_automation_point.as_mut() {
            selected.beat = to_beat;
            selected.value_norm = ((to_value - lane.param_min)
                / (lane.param_max - lane.param_min).max(f32::EPSILON))
            .clamp(0.0, 1.0);
        }
    }

    /// Commit a finished automation point drag as one undo entry. No-op if
    /// the point never actually moved.
    fn commit_automation_drag(
        &mut self,
        drag: AutomationDragState,
        host: &mut dyn TimelineEditingHost,
    ) {
        let new_point = (drag.last_beat, drag.last_value, drag.original.2);
        let moved = (new_point.0.0 - drag.original.0.0).abs() >= 0.0001
            || (new_point.1 - drag.original.1).abs() > f32::EPSILON;
        if moved {
            host.commit_automation_point_move(
                &drag.target,
                &drag.param_id,
                drag.original,
                new_point,
            );
        }
    }

    /// Pixel range of vertical drag mapped to the full `-1..1` bend swing —
    /// an interior tuning constant (Alt-drag curve bend, P4 Unit B).
    const SEGMENT_BEND_PX_RANGE: f32 = 80.0;

    /// Live-preview an in-progress Alt-drag curve bend. Re-derives `bend`
    /// fresh from the vertical delta since grab each frame (never
    /// incrementally), same discipline as every other drag handler here.
    /// Dragging UP (screen Y decreasing) bends toward the higher-value side of
    /// the segment, so rising and falling segments feel identical on screen.
    fn handle_automation_segment_bend_drag(
        &mut self,
        pos: Vec2,
        host: &mut dyn TimelineEditingHost,
    ) {
        let Some(TimelineDrag::AutomationSegmentBend(state)) = self.drag.payload() else {
            return;
        };
        let target = state.target.clone();
        let param_id = state.param_id.clone();
        let left_beat = state.left_beat;
        let left_value = state.left_value;
        let original_shape = state.original_shape;
        let snapshot_index = state.snapshot_index;
        let delta_px = state.last_pointer_y - pos.y;
        let scale = if self.modifiers.shift { 0.25 } else { 1.0 };
        let accumulated_delta_px = state.accumulated_delta_px + delta_px * scale;
        let direction = if state.direction_sign.abs() <= f32::EPSILON {
            0.0
        } else {
            -state.direction_sign
        };
        let bend = (state.original_bend
            + direction * (accumulated_delta_px / Self::SEGMENT_BEND_PX_RANGE))
            .clamp(-1.0, 1.0);

        // The bend-only host API predates CurvedRange. Rebuild the complete
        // captured lane so a clipped curve keeps its original subrange.
        if let Some(snapshot) = self
            .automation_lane_snapshots
            .get(snapshot_index)
            .and_then(|snapshot| snapshot.points.as_ref())
            && let Some(TimelineDrag::AutomationSegmentBend(state)) = self.drag.payload_mut()
        {
            state.working.clone_from(snapshot);
            if let Some(point) = state
                .working
                .iter_mut()
                .find(|point| point.0 == left_beat && (point.1 - left_value).abs() <= f32::EPSILON)
            {
                point.2 = if bend == state.original_bend {
                    original_shape
                } else {
                    shape_with_bend(original_shape, bend)
                };
            }
            host.set_automation_lane_preview(&target, &param_id, &state.working);
        }
        if let Some(TimelineDrag::AutomationSegmentBend(state)) = self.drag.payload_mut() {
            state.last_pointer_y = pos.y;
            state.accumulated_delta_px = accumulated_delta_px;
            state.last_bend = bend;
        }
    }

    /// Commit a finished curve-bend drag as one undo entry — reuses
    /// `commit_automation_point_move` directly: beat and value are
    /// untouched by this gesture, only `shape` differs between old and new.
    fn commit_automation_segment_bend(
        &mut self,
        state: AutomationSegmentBendState,
        host: &mut dyn TimelineEditingHost,
    ) {
        if state.last_bend == state.original_bend {
            return;
        }
        let new_shape = shape_with_bend(state.original_shape, state.last_bend);
        if new_shape != state.original_shape {
            let old = (state.left_beat, state.left_value, state.original_shape);
            let new = (state.left_beat, state.left_value, new_shape);
            host.commit_automation_point_move(&state.target, &state.param_id, old, new);
        }
    }

    /// Live-preview an in-progress vertical segment drag. Both endpoints'
    /// values come from the resolver's travel since the grab; a shared clamp
    /// keeps the value delta identical at both ends when either reaches its
    /// parameter boundary.
    fn handle_automation_segment_vertical_drag(
        &mut self,
        pos: Vec2,
        host: &mut dyn TimelineEditingHost,
        viewport: &TimelineViewportPanel,
    ) {
        let Some(TimelineDrag::AutomationSegmentDrag(state)) = self.drag.payload() else {
            return;
        };
        let (target, param_id) = (state.target.clone(), state.param_id.clone());
        let lanes = viewport.automation_lane_screens();
        let Some(lane) = lanes
            .iter()
            .find(|l| l.target == target && l.param_id == param_id)
        else {
            return;
        };
        let input = ResolveInput {
            pos,
            pointer_beat: viewport.pixel_to_beat(pos.x),
            grid_step: viewport.grid_step(),
            command: self.modifiers.command,
            shift: self.modifiers.shift,
            curve_height: lane.curve_rect().height,
        };
        let Some(TimelineDrag::AutomationSegmentDrag(state)) = self.drag.payload_mut() else {
            return;
        };
        let resolved = state.resolver.resolve(&input);
        let state = state.clone();
        // A shared clamp keeps the value delta identical at both ends when
        // either endpoint reaches its parameter boundary.
        let (min, max) = (lane.param_min, lane.param_max);
        let delta_norm = shared_norm_delta(
            [state.left_original_value, state.right_original_value]
                .into_iter()
                .map(|v| (v, min, max)),
            resolved.norm_delta,
        );
        let left_value = apply_value(
            state.left_original_value,
            delta_norm,
            min,
            max,
            lane.whole_numbers,
        );
        let right_value = apply_value(
            state.right_original_value,
            delta_norm,
            min,
            max,
            lane.whole_numbers,
        );

        host.set_automation_segment_drag_preview(
            &state.target,
            &state.param_id,
            state.left_beat,
            state.last_left_value,
            left_value,
            state.right_beat,
            state.last_right_value,
            right_value,
        );
        if let Some(TimelineDrag::AutomationSegmentDrag(s)) = self.drag.payload_mut() {
            s.last_left_value = left_value;
            s.last_right_value = right_value;
        }
    }

    /// Commit a finished vertical segment drag as ONE undo entry covering
    /// both endpoints (`host.commit_automation_segment_drag` batches them via
    /// `ContentCommand::ExecuteBatch`/`CompositeCommand`).
    fn commit_automation_segment_value_drag(
        &mut self,
        state: AutomationSegmentDragState,
        host: &mut dyn TimelineEditingHost,
    ) {
        let moved = (state.last_left_value - state.left_original_value).abs() > f32::EPSILON
            || (state.last_right_value - state.right_original_value).abs() > f32::EPSILON;
        if moved {
            host.commit_automation_segment_drag(
                &state.target,
                &state.param_id,
                (
                    state.left_beat,
                    state.left_original_value,
                    state.last_left_value,
                    state.left_shape,
                ),
                (
                    state.right_beat,
                    state.right_original_value,
                    state.last_right_value,
                    state.right_shape,
                ),
            );
        }
    }

    fn automation_time_bounds(
        a: Vec2,
        b: Vec2,
        viewport: &TimelineViewportPanel,
        command: bool,
    ) -> (Beats, Beats) {
        let snap = |x: f32| {
            let raw = viewport.pixel_to_beat(x).max(Beats::ZERO);
            if command {
                raw
            } else {
                viewport.snap_to_grid(raw)
            }
            .max(Beats::ZERO)
        };
        let first = snap(a.x);
        let second = snap(b.x);
        (first.min(second), first.max(second))
    }

    fn update_automation_time_selection(
        &self,
        start: Vec2,
        current: Vec2,
        ui_state: &mut UIState,
        viewport: &TimelineViewportPanel,
    ) {
        let (start_beat, end_beat) =
            Self::automation_time_bounds(start, current, viewport, self.modifiers.command);
        let top = start.y.min(current.y);
        let bottom = start.y.max(current.y);
        let selection =
            ui_state
                .automation_time_selection
                .get_or_insert_with(|| AutomationTimeSelection {
                    start: start_beat,
                    end: end_beat,
                    lanes: Vec::with_capacity(4),
                });
        selection.start = start_beat;
        selection.end = end_beat;
        selection.lanes.clear();
        for lane in viewport.automation_lane_screens() {
            if lane.strip_rect.y <= bottom && lane.strip_rect.y_max() >= top {
                selection
                    .lanes
                    .push((lane.target.clone(), lane.param_id.clone()));
            }
        }
    }

    /// Screen position of the marquee's press corner under the current scroll.
    /// The corner is anchored in content space (beat, content-y), not screen
    /// space, so edge autoscroll extends the rectangle instead of dragging
    /// its start along with the view.
    fn marquee_start(anchor: &AutomationMarqueeAnchor, viewport: &TimelineViewportPanel) -> Vec2 {
        Vec2::new(
            viewport.beat_to_pixel(anchor.beat),
            anchor.content_y - viewport.scroll_y_px(),
        )
    }

    /// Live-update the automation time selection every frame from the press
    /// corner to the current position. Every point (visible or scrolled off)
    /// in the intersected lanes and beat interval is selected, regardless of
    /// its vertical value; the range itself remains active when no points
    /// fall inside it. A Shift-drag keeps the selection held at press time.
    fn handle_automation_marquee_drag(
        &mut self,
        pos: Vec2,
        host: &dyn TimelineEditingHost,
        ui_state: &mut UIState,
        viewport: &TimelineViewportPanel,
    ) {
        let Some(session) = self.drag.session() else {
            return;
        };
        let TimelineDrag::AutomationMarquee(anchor) = &session.payload else {
            return;
        };
        let start = Self::marquee_start(anchor, viewport);
        self.update_automation_time_selection(start, pos, ui_state, viewport);
        let lanes = viewport.automation_lane_screens();
        let selection = ui_state.automation_time_selection.take();
        ui_state.selected_automation_points.clear();
        ui_state
            .selected_automation_points
            .extend(anchor.base.iter().cloned());
        if let Some(selection) = selection {
            for lane in lanes {
                if !selection
                    .lanes
                    .iter()
                    .any(|key| key.0 == lane.target && key.1 == lane.param_id)
                {
                    continue;
                }
                // Identity within a lane is (beat, normalized value); a set
                // keeps the dedupe linear in the number of lane points.
                let mut seen: AHashSet<(u64, u32)> = ui_state
                    .selected_automation_points
                    .iter()
                    .filter(|p| p.target == lane.target && p.param_id == lane.param_id)
                    .map(|p| (p.beat.0.to_bits(), p.value_norm.to_bits()))
                    .collect();
                let mut add = |beat: Beats, value_norm: f32| {
                    if beat < selection.start || beat > selection.end {
                        return;
                    }
                    if seen.insert((beat.0.to_bits(), value_norm.to_bits())) {
                        ui_state
                            .selected_automation_points
                            .push(UiAutomationPointRef {
                                target: lane.target.clone(),
                                param_id: lane.param_id.clone(),
                                beat,
                                value_norm,
                            });
                    }
                };
                match host.automation_lane_points(&lane.target, &lane.param_id) {
                    Some(points) => {
                        for point in points {
                            add(
                                point.0,
                                BitmapSlider::value_to_normalized(
                                    point.1,
                                    lane.param_min,
                                    lane.param_max,
                                ),
                            );
                        }
                    }
                    None => {
                        for dot in &lane.dots {
                            add(dot.beat, dot.value_norm);
                        }
                    }
                }
            }
            ui_state.automation_time_selection = Some(selection);
        }
    }

    /// Live-preview an in-progress marquee GROUP drag. One snapped beat delta
    /// and one normalized value delta are derived from the grabbed point and
    /// applied to every selected point. Each affected lane is rebuilt from
    /// its full grab snapshot so crossed points and destination collisions
    /// cannot corrupt the ordering.
    fn handle_automation_group_drag(
        &mut self,
        pos: Vec2,
        host: &mut dyn TimelineEditingHost,
        ui_state: &mut UIState,
        viewport: &TimelineViewportPanel,
    ) {
        let Some(TimelineDrag::AutomationGroupMove(s)) = self.drag.payload() else {
            return;
        };
        let (grab_target, grab_param_id) = (s.grab_target.clone(), s.grab_param_id.clone());
        let lanes = viewport.automation_lane_screens();
        let Some(grab_lane) = lanes
            .iter()
            .find(|l| l.target == grab_target && l.param_id == grab_param_id)
        else {
            return;
        };
        let input = ResolveInput {
            pos,
            pointer_beat: viewport.pixel_to_beat(pos.x),
            grid_step: viewport.grid_step(),
            command: self.modifiers.command,
            shift: self.modifiers.shift,
            curve_height: grab_lane.curve_rect().height,
        };
        let Some(TimelineDrag::AutomationGroupMove(state)) = self.drag.payload_mut() else {
            return;
        };
        let resolved = state.resolver.resolve(&input);
        // One shared delta preserves all inter-point spacing; it stops at beat
        // zero and at the nearest unselected point, the arrow-key nudge's clamp.
        let beat_delta = Beats(clamp_group_beat_delta(
            resolved.beat_delta,
            state.min_beat,
            state.room_left,
            state.room_right,
        ));
        // Clamp the shared normalized delta against the whole selection, so
        // one endpoint reaching a bound does not squash the selected shape.
        let delta_norm = shared_norm_delta(
            state
                .points
                .iter()
                .map(|p| (p.original_value, p.param_min, p.param_max)),
            resolved.norm_delta,
        );
        let unchanged = beat_delta.0 == 0.0 && delta_norm == 0.0;

        for point in &mut state.points {
            point.last_beat = point.original_beat + beat_delta;
            point.last_value = apply_value(
                point.original_value,
                delta_norm,
                point.param_min,
                point.param_max,
                point.whole_numbers,
            );
        }
        let snapshots = &self.automation_lane_snapshots;
        for (lane_index, lane) in state.lanes.iter_mut().enumerate() {
            let Some(original_points) = snapshots[lane.snapshot_index].points.as_ref() else {
                lane.working.clear();
                continue;
            };
            lane.working.clone_from(original_points);

            // A no-op frame must preserve the full snapshot byte-for-byte;
            // rebuilding through insert-or-overwrite would collapse legacy
            // duplicate beats that the user has not moved.
            if unchanged {
                host.set_automation_lane_preview(&lane.target, &lane.param_id, &lane.working);
                continue;
            }

            // Transform selected source tuples in their original positions.
            // Unselected points are never removed: a drop onto an exact twin
            // keeps both, as the commit does (`for_group`).
            // Stable ordering preserves equal-beat order: a point moved right
            // stays before a stationary endpoint, and a point moved left
            // stays after it.
            for point in lane.working.iter_mut() {
                if let Some(selected) = state.points.iter().find(|selected| {
                    selected.lane_index == lane_index
                        && point.0 == selected.original_beat
                        && point.1 == selected.original_value
                }) {
                    *point = (selected.last_beat, selected.last_value, selected.shape);
                }
            }
            // Keep equal-beat order without allocating stable-sort scratch
            // on every pointer frame. Most frames retain the original order.
            for index in 1..lane.working.len() {
                if lane.working[index - 1].0 > lane.working[index].0 {
                    let beat = lane.working[index].0;
                    let destination =
                        lane.working[..index].partition_point(|point| point.0 <= beat);
                    lane.working[destination..=index].rotate_right(1);
                }
            }
            host.set_automation_lane_preview(&lane.target, &lane.param_id, &lane.working);
        }
        ui_state.selected_automation_points.clear();
        ui_state
            .selected_automation_points
            .extend(state.points.iter().map(|point| {
                UiAutomationPointRef {
                    target: point.target.clone(),
                    param_id: point.param_id.clone(),
                    beat: point.last_beat,
                    value_norm: ((point.last_value - point.param_min)
                        / (point.param_max - point.param_min).max(f32::EPSILON))
                    .clamp(0.0, 1.0),
                }
            }));
    }

    /// Commit a finished marquee group drag as ONE undo entry covering every
    /// point (`host.commit_automation_group_move` batches them via
    /// `ContentCommand::ExecuteBatch`/`CompositeCommand`). No-op if nothing
    /// actually moved.
    fn commit_automation_group_drag(
        &mut self,
        state: AutomationGroupDragState,
        host: &mut dyn TimelineEditingHost,
    ) {
        let moved = state.points.iter().any(|p| {
            p.last_beat != p.original_beat || (p.last_value - p.original_value).abs() > f32::EPSILON
        });
        if moved {
            let moves = state
                .points
                .into_iter()
                .map(|p| AutomationPointMove {
                    target: p.target,
                    param_id: p.param_id,
                    old: (p.original_beat, p.original_value, p.shape),
                    new: (p.last_beat, p.last_value, p.shape),
                })
                .collect();
            host.commit_automation_group_move(moves);
        }
    }

    /// Port of Unity InteractionOverlay.OnPointerMove (lines 219-257).
    pub fn on_pointer_move(
        &mut self,
        pos: Vec2,
        host: &mut dyn TimelineEditingHost,
        ui_state: &mut UIState,
        viewport: &TimelineViewportPanel,
    ) {
        // Unity lines 222-223: track cursor position for paste target
        ui_state.set_pointer_position(
            viewport.pixel_to_beat(pos.x).as_f32(),
            viewport
                .layer_at_y(pos.y)
                .and_then(|idx| host.layer_id_at_index(idx)),
        );

        // Unity lines 225-245: hover detection
        let hit = self.hit_test_at(pos, viewport);
        let lanes = viewport.automation_lane_screens();
        // Feedback is re-resolved and painted every frame (frame/present.rs);
        // a move only needs that frame to happen, never a rebuild.
        if ui_state.automation_feedback.is_some() || viewport.tracks_rect().contains(pos) {
            host.redraw();
        }
        let new_hover_id = hit.as_ref().map(|h| h.clip_id.clone());

        // The hovered clip is drawn by the immediate clip pass; the grid
        // bitmaps do not depend on it.
        if new_hover_id != ui_state.hovered_clip_id {
            ui_state.hovered_clip_id = new_hover_id;
            host.redraw();
        }

        // Unity lines 248-256: cursor feedback (only when not dragging)
        if !self.drag.is_active() {
            if automation_hit_tester::resize_grip_lane(pos, lanes).is_some() {
                host.set_cursor(TimelineCursor::ResizeVertical);
                return;
            }
            if let Some(ref hit) = hit {
                match hit.region {
                    HitRegion::TrimLeft | HitRegion::TrimRight => {
                        host.set_cursor(TimelineCursor::ResizeHorizontal);
                    }
                    HitRegion::Body => {
                        host.set_cursor(TimelineCursor::Move);
                    }
                }
            } else {
                host.set_cursor(TimelineCursor::Default);
            }
        }
    }

    // ────────────────────────────────────────────────────────────
    // DRAG EVENTS
    // ────────────────────────────────────────────────────────────

    /// Port of Unity InteractionOverlay.OnBeginDrag (lines 278-332).
    /// `press_pos` is the position where the mouse was PRESSED, not current.
    pub fn on_begin_drag(
        &mut self,
        press_pos: Vec2,
        host: &mut dyn TimelineEditingHost,
        ui_state: &mut UIState,
        viewport: &TimelineViewportPanel,
    ) {
        // A provisional click candidate must not survive promotion to a drag:
        // restore the pre-press lane before marquee, segment, or pencil
        // routing captures its own gesture snapshot.
        self.last_automation_drag_frame = None;
        let pending_hit = self.automation_press.as_ref().map(|draft| draft.hit);
        self.restore_pending_automation_press(host);
        self.forced_automation_hit = pending_hit;
        if crate::input::input_trace_enabled() {
            eprintln!(
                "[input-trace] overlay: begin_drag ({:.0},{:.0}) prior mode={:?}",
                press_pos.x,
                press_pos.y,
                self.drag_mode()
            );
        }
        // `layer_blocked` no longer needs a pre-emptive reset here — it's
        // freshly initialized `false` inside `MoveDrag` at construction now
        // (P7.5). `was_layer_blocked` stays a loose field (its lifetime
        // needs to persist across the whole gesture for the rising-edge
        // shake detector in `handle_move_drag`).
        self.was_layer_blocked = false;

        let press_lanes = viewport.automation_lane_screens();
        if let Some(lane) =
            automation_hit_tester::resize_grip_lane(press_pos, press_lanes).map(|i| &press_lanes[i])
        {
            let height = lane.strip_rect.height;
            self.drag.start(
                TimelineDrag::AutomationLaneResize(AutomationLaneResize {
                    target: lane.target.clone(),
                    param_id: lane.param_id.clone(),
                    start_y: press_pos.y,
                    start_height: height,
                }),
                press_pos,
            );
            host.set_cursor(TimelineCursor::ResizeVertical);
            return;
        }

        // P4 Unit A: grabbing an existing automation dot starts a point
        // drag instead of clip/region logic — see `begin_automation_drag`'s
        // doc for why bare-strip clicks aren't handled here.
        if self.begin_automation_drag(press_pos, host, ui_state, viewport) {
            return;
        }

        // Unity line 284: hit-test at PRESS position
        let hit = self.hit_test_at(press_pos, viewport);

        if hit.is_none() {
            // Unity lines 288-291: empty area drag → region selection
            // Unity reads Keyboard.current for ctrl/cmd — we use stored modifiers
            let ctrl = self.modifiers.ctrl || self.modifiers.command;
            let region = Self::begin_region_drag(press_pos, ctrl, ui_state, viewport);
            self.drag
                .start(TimelineDrag::RegionSelect(region), press_pos);
            return;
        }

        let hit = hit.unwrap();

        // Unity line 295: locked clips — ignore
        if self.clip_is_locked(&hit.clip_id, viewport) {
            return;
        }

        let beat = viewport.pixel_to_beat(press_pos.x);

        let hit_layer_id = host.layer_id_at_index(hit.layer_index).unwrap_or_default();

        match hit.region {
            // Unity lines 299-309: trim left
            HitRegion::TrimLeft => {
                if !ui_state.is_selected(&hit.clip_id) {
                    ui_state.select_clip(hit.clip_id.clone(), hit_layer_id.clone());
                    host.on_clip_selected(&hit.clip_id);
                }
                if let Some(clip) = host.find_clip_by_id(&hit.clip_id) {
                    let trim = Self::build_trim_drag(hit.clip_id.clone(), &clip, ui_state, host);
                    self.drag.start(TimelineDrag::TrimLeft(trim), press_pos);
                }
            }
            // Unity lines 311-320: trim right
            HitRegion::TrimRight => {
                if !ui_state.is_selected(&hit.clip_id) {
                    ui_state.select_clip(hit.clip_id.clone(), hit_layer_id);
                    host.on_clip_selected(&hit.clip_id);
                }
                if let Some(clip) = host.find_clip_by_id(&hit.clip_id) {
                    let trim = Self::build_trim_drag(hit.clip_id.clone(), &clip, ui_state, host);
                    self.drag.start(TimelineDrag::TrimRight(trim), press_pos);
                }
            }
            // Unity lines 322-324: body → move drag
            HitRegion::Body => {
                // Alt/Opt-drag duplicates: remembered now, acted on at release.
                let duplicate_on_release = self.modifiers.alt;
                self.begin_move_drag(
                    &hit.clip_id,
                    hit.layer_index,
                    beat,
                    press_pos,
                    duplicate_on_release,
                    host,
                    ui_state,
                    viewport,
                );
            }
        }

        // Unity lines 328-331: reinforce cursor
        match self.drag_mode() {
            DragMode::TrimLeft | DragMode::TrimRight => {
                host.set_cursor(TimelineCursor::ResizeHorizontal);
            }
            DragMode::Move => {
                host.set_cursor(TimelineCursor::Move);
            }
            _ => {}
        }
    }

    /// Port of Unity InteractionOverlay.OnDrag (lines 334-353).
    pub fn on_drag(
        &mut self,
        pos: Vec2,
        host: &mut dyn TimelineEditingHost,
        ui_state: &mut UIState,
        viewport: &mut TimelineViewportPanel,
    ) {
        self.drag.track(pos);
        // Scroll before the handler converts pixels to beats, so a parked
        // pointer keeps advancing the gesture as the content moves under it.
        // Horizontal only: the pointer's height sets the value being edited,
        // so a lane near the top or bottom edge must not scroll away mid-drag.
        // Point and group drags stay put until the pointer has moved in time,
        // so a vertical-only value drag near the edge never scrolls.
        let mode = self.drag_mode();
        if Self::autoscrolls_at_edge(mode) && self.drag_moves_in_time(mode, pos) {
            let rect = viewport.tracks_rect();
            viewport.autoscroll_edge(Vec2::new(pos.x, rect.y + rect.height * 0.5));
        }
        if matches!(
            mode,
            DragMode::AutomationPoint
                | DragMode::AutomationSegmentDrag
                | DragMode::AutomationMarquee
                | DragMode::AutomationGroupMove
        ) {
            let key = AutomationDragFrameKey {
                pos,
                scroll_x: viewport.scroll_x_beats(),
                scroll_y: viewport.scroll_y_px(),
                pixels_per_beat: viewport.pixels_per_beat(),
                modifiers: self.modifiers,
            };
            if self.last_automation_drag_frame == Some(key) {
                return;
            }
            self.last_automation_drag_frame = Some(key);
        }
        match self.drag_mode() {
            DragMode::Move => {
                self.handle_move_drag(pos, host, ui_state, viewport);
            }
            DragMode::TrimLeft => {
                self.handle_trim_left_drag(pos, host, viewport);
            }
            DragMode::TrimRight => {
                self.handle_trim_right_drag(pos, host, viewport);
            }
            DragMode::RegionSelect => {
                self.update_region_drag(pos, ui_state, viewport, host);
            }
            DragMode::AutomationPoint => {
                self.handle_automation_drag(pos, host, ui_state, viewport);
            }
            DragMode::AutomationSegmentBend => {
                self.handle_automation_segment_bend_drag(pos, host);
            }
            DragMode::AutomationSegmentDrag => {
                self.handle_automation_segment_vertical_drag(pos, host, viewport);
            }
            DragMode::AutomationMarquee => {
                self.handle_automation_marquee_drag(pos, host, ui_state, viewport);
                host.redraw();
            }
            DragMode::AutomationGroupMove => {
                self.handle_automation_group_drag(pos, host, ui_state, viewport);
            }
            DragMode::AutomationDraw => {
                self.write_automation_draw_step(pos, host, viewport);
            }
            DragMode::AutomationLaneResize => {
                if let Some(TimelineDrag::AutomationLaneResize(state)) = self.drag.payload() {
                    let height = (state.start_height + pos.y - state.start_y).clamp(
                        crate::color::AUTOMATION_LANE_MIN_HEIGHT,
                        crate::color::AUTOMATION_LANE_MAX_HEIGHT,
                    );
                    ui_state.set_automation_lane_height(
                        state.target.clone(),
                        state.param_id.clone(),
                        height,
                    );
                }
            }
            DragMode::None => {}
        }
    }

    /// Port of Unity InteractionOverlay.OnEndDrag (lines 356-446).
    ///
    /// Takes no `UIState`: end-of-drag commits the engine batch and clears the
    /// overlay's own transient state — selection is untouched here.
    pub fn on_end_drag(&mut self, host: &mut dyn TimelineEditingHost) {
        if crate::input::input_trace_enabled() {
            eprintln!(
                "[input-trace] overlay: end_drag entered (mode={:?})",
                self.drag_mode()
            );
        }

        // P7.3 (D8/D11): one `release()` call is the single consuming read of
        // the drag session — every automation gesture commits directly here
        // (already applied live by its own preview calls, so it needs none
        // of the command-batch/enforce-non-overlap machinery a move/trim
        // does) and returns; `Move`/`TrimLeft`/`TrimRight` fall through to
        // the existing batch-commit logic below via the two flags captured
        // here (their own state still lives in the loose fields until
        // P7.4/P7.5 fold it onto the payload).
        let mut move_state: Option<MoveDrag> = None;
        let mut trim_state: Option<TrimDrag> = None;
        match self.drag.release() {
            // Unity lines 358-363: region select → finalize
            Some(TimelineDrag::RegionSelect(_)) => {
                host.redraw();
                return;
            }
            // P4 Unit A: automation point drag → commit one undo entry.
            Some(TimelineDrag::AutomationPoint(state)) => {
                host.clear_automation_previews();
                self.commit_automation_drag(state, host);
                self.reset_drag_state(host);
                return;
            }
            // P4 Unit B: segment gestures commit the same way — already
            // applied live, just register the undo entry (single command for
            // a bend, batched pair for a vertical drag).
            Some(TimelineDrag::AutomationSegmentBend(state)) => {
                host.clear_automation_previews();
                self.commit_automation_segment_bend(state, host);
                self.reset_drag_state(host);
                return;
            }
            Some(TimelineDrag::AutomationSegmentDrag(state)) => {
                host.clear_automation_previews();
                self.commit_automation_segment_value_drag(state, host);
                self.reset_drag_state(host);
                return;
            }
            Some(TimelineDrag::AutomationLaneResize(_)) => {
                self.reset_drag_state(host);
                return;
            }
            // P4 Unit B: marquee selection isn't an edit — just stop
            // tracking and redraw (the selected set itself stays in
            // `UIState`, already written live during `on_drag`).
            Some(TimelineDrag::AutomationMarquee(_)) => {
                host.redraw();
                self.reset_drag_state(host);
                return;
            }
            // P4 Unit B: group move / draw stroke commit the same way as the
            // single-point/segment gestures above.
            Some(TimelineDrag::AutomationGroupMove(state)) => {
                host.clear_automation_previews();
                self.commit_automation_group_drag(state, host);
                self.reset_drag_state(host);
                return;
            }
            Some(TimelineDrag::AutomationDraw(state)) => {
                host.clear_automation_previews();
                self.commit_automation_draw(state, host);
                self.reset_drag_state(host);
                return;
            }
            Some(TimelineDrag::Move(m)) => {
                move_state = Some(m);
            }
            Some(TimelineDrag::TrimLeft(t)) | Some(TimelineDrag::TrimRight(t)) => {
                trim_state = Some(t);
            }
            None => {}
        }

        host.begin_command_batch();

        // D15 landing-line flash geometry — leftmost landed beat + layer span
        // of the clips that actually moved, accumulated in the record loop.
        let mut landed: Option<(Beats, usize, usize)> = None;

        if let Some(mv) = &move_state {
            // Unity lines 370-386: record commands. No finalize-snap step —
            // `handle_move_drag` already snapped+clamped this position on the
            // last frame (D5); there is nothing left to reconcile here.
            for snapshot in &mv.snapshots {
                if let Some(clip) = host.find_clip_by_id(&snapshot.clip_id) {
                    let start_changed =
                        (clip.start_beat - snapshot.start_beat).abs() >= Beats(0.0001);
                    let layer_changed = clip.layer_index != snapshot.layer_index;
                    if start_changed || layer_changed {
                        landed = Some(match landed {
                            None => (clip.start_beat, clip.layer_index, clip.layer_index),
                            Some((b, lo, hi)) => (
                                if clip.start_beat < b {
                                    clip.start_beat
                                } else {
                                    b
                                },
                                lo.min(clip.layer_index),
                                hi.max(clip.layer_index),
                            ),
                        });
                        host.record_move(
                            &snapshot.clip_id,
                            snapshot.start_beat,
                            clip.start_beat,
                            snapshot.layer_index,
                            clip.layer_index,
                        );
                        // Opt/Alt-drag: drop a copy back at the original position
                        // so the moved clip reads as the duplicate. Added to the
                        // same batch → one undo entry with the move.
                        if mv.duplicate_on_release {
                            host.duplicate_clip_to(
                                &snapshot.clip_id,
                                snapshot.start_beat,
                                snapshot.layer_index,
                            );
                        }
                    }
                }
            }

            // Unity lines 407-416: enforce non-overlap on all dragged clips
            for snapshot in &mv.snapshots {
                host.enforce_non_overlap(&snapshot.clip_id, &mv.snapshot_clip_ids);
            }
        } else if let Some(trim) = &trim_state {
            // Unity lines 390-401: record a trim command for every selected clip
            // that actually changed, each with its own pre/post geometry.
            for orig in &trim.originals {
                if let Some(clip) = host.find_clip_by_id(&orig.clip_id) {
                    let start_changed = (clip.start_beat - orig.start_beat).abs() >= Beats(0.0001);
                    let dur_changed =
                        (clip.duration_beats - orig.duration_beats).abs() >= Beats(0.0001);
                    if start_changed || dur_changed {
                        host.record_trim(
                            &orig.clip_id,
                            orig.start_beat,
                            clip.start_beat,
                            orig.duration_beats,
                            clip.duration_beats,
                            orig.in_point,
                            clip.in_point,
                        );
                    }
                }
            }

            // Unity lines 417-421: enforce non-overlap on every trimmed clip,
            // ignoring the trimmed set itself so co-selected clips don't shove
            // each other (mirrors the move path's snapshot-clip-id set).
            let trimmed_ids: HashSet<ClipId> =
                trim.originals.iter().map(|o| o.clip_id.clone()).collect();
            for id in &trimmed_ids {
                host.enforce_non_overlap(id, &trimmed_ids);
            }
        }

        // Unity lines 436-441: commit as composite command
        let desc = match &move_state {
            Some(mv) if mv.duplicate_on_release => "Duplicate clips",
            Some(_) => "Move clips",
            None => "Trim clips",
        };
        host.commit_command_batch(desc);

        // landing-line flash — re-hooked at drag-end. P1.4's
        // continuous snap deleted the discrete `finalize_move_snap` trigger
        // (see the dormancy note at the drawer, `app_render.rs`); the drag-END
        // commit is the new discrete moment. Fires once, only when a move
        // actually landed somewhere new (a click-without-move stays dark).
        // Move only — a trim reshapes in place, there is no "landing".
        // Feel sign-off owed to Peter.
        if let Some((beat, lo, hi)) = landed {
            self.landing_flash_beat = beat;
            self.landing_flash_layers = (lo, hi);
            self.landing_flash.fire(color::MOTION_MED_MS);
        }

        // Unity lines 423-427/444-445: clear drag state
        self.reset_drag_state(host);
    }

    /// Escape cancels an in-flight move or trim gesture (D5, B8): the model
    /// is restored to the pre-gesture snapshot through the SAME begin/commit
    /// batch pair `on_end_drag` uses to land a real drag. Nothing is
    /// `record_*`'d into the batch, so `commit_command_batch` sees an empty
    /// command list and returns without pushing an undo entry or reaching the
    /// content thread (`AppEditingHost::commit_command_batch`'s
    /// `if commands.is_empty() { return; }`). This is "restore and close
    /// batch" — never "commit then undo": the latter would create a real
    /// undo entry only to erase it, which is observable (an extra undo-stack
    /// slot, a spurious `ContentCommand`) in a way a true cancel is not.
    ///
    /// Region-select remains untouched; automation gestures restore their
    /// captured lane snapshots and clear their temporary previews.
    pub fn cancel_drag(&mut self, host: &mut dyn TimelineEditingHost) {
        if self.automation_press.is_some() {
            self.restore_pending_automation_press(host);
            self.suppress_next_automation_click = true;
        }
        if matches!(
            self.drag_mode(),
            DragMode::AutomationPoint
                | DragMode::AutomationSegmentBend
                | DragMode::AutomationSegmentDrag
                | DragMode::AutomationGroupMove
                | DragMode::AutomationDraw
                | DragMode::AutomationMarquee
        ) {
            for snapshot in &self.automation_lane_snapshots {
                host.restore_automation_lane_preview(
                    &snapshot.target,
                    &snapshot.param_id,
                    snapshot.points.as_deref(),
                );
            }
            host.clear_automation_previews();
            host.redraw();
            self.reset_drag_state(host);
            return;
        }
        match self.drag.payload() {
            Some(TimelineDrag::Move(m)) => {
                for snapshot in &m.snapshots {
                    host.set_clip_start_beat(&snapshot.clip_id, snapshot.start_beat);
                    if let Some(clip) = host.find_clip_by_id(&snapshot.clip_id)
                        && clip.layer_index != snapshot.layer_index
                    {
                        host.move_clip_to_layer(&snapshot.clip_id, snapshot.layer_index);
                    }
                }
            }
            Some(TimelineDrag::TrimLeft(t)) | Some(TimelineDrag::TrimRight(t)) => {
                for orig in &t.originals {
                    host.set_clip_trim(
                        &orig.clip_id,
                        orig.start_beat,
                        orig.duration_beats,
                        orig.in_point,
                    );
                }
            }
            _ => return,
        }
        // Restore-and-close: open/close the SAME batch pair `on_end_drag`
        // commits, but record nothing into it — the commit is a genuine
        // no-op (see doc comment above), not a commit-then-undo.
        host.begin_command_batch();
        host.commit_command_batch("cancelled");
        host.redraw();
        self.reset_drag_state(host);
    }

    /// The drag-state clear shared by a normal `on_end_drag` commit and an
    /// Escape `cancel_drag` — both end the gesture the same way once the
    /// model is settled (committed or restored).
    fn reset_drag_state(&mut self, host: &mut dyn TimelineEditingHost) {
        // Idempotent: `on_end_drag`'s path already consumed the session via
        // `release()`; `cancel_drag`'s path has not, so this is where it
        // drops (no commit signal, per `cancel`'s contract).
        self.drag.cancel();
        self.automation_lane_snapshots.clear();
        self.automation_press = None;
        self.forced_automation_hit = None;
        self.last_automation_drag_frame = None;
        host.set_cursor(TimelineCursor::Default);
    }

    // ────────────────────────────────────────────────────────────
    // DRAG HANDLERS
    // ────────────────────────────────────────────────────────────

    /// Port of Unity InteractionOverlay.HandleMoveDrag (lines 463-537).
    fn handle_move_drag(
        &mut self,
        screen_pos: Vec2,
        host: &mut dyn TimelineEditingHost,
        ui_state: &mut UIState,
        viewport: &mut TimelineViewportPanel,
    ) {
        // P7.5: `layer_blocked`'s intermediate writes below need `&mut`
        // access to the payload interleaved with `host`/`viewport` calls
        // (separate objects, no conflict) and other `self` fields
        // (`was_layer_blocked`, `error_shake` — disjoint fields, no
        // conflict). `snapshots`'s lazy re-capture (mirrors the pre-fold
        // "if empty, capture" discipline) runs first, via a payload_mut
        // write, before anything below reads it.
        if !matches!(self.drag.payload(), Some(TimelineDrag::Move(_))) {
            return;
        }
        let snapshots_empty =
            matches!(self.drag.payload(), Some(TimelineDrag::Move(m)) if m.snapshots.is_empty());
        if snapshots_empty {
            let anchor = match self.drag.payload() {
                Some(TimelineDrag::Move(m)) => m.anchor_clip_id.clone(),
                _ => return,
            };
            let (snapshots, snapshot_clip_ids, min_start_beat, min_layer, max_layer) =
                Self::capture_drag_selection(ui_state, host, &anchor);
            if let Some(TimelineDrag::Move(m)) = self.drag.payload_mut() {
                m.snapshots = snapshots;
                m.snapshot_clip_ids = snapshot_clip_ids;
                m.selection_min_start_beat = min_start_beat;
                m.selection_min_layer = min_layer;
                m.selection_max_layer = max_layer;
            }
        }

        // Unity line 470: auto-scroll (B11) — the P0 scroll owner advances
        // BEFORE the beat conversion below, so this frame's gesture math
        // already reflects the new scroll position: a parked pointer still
        // advances the gesture as the content scrolls under it.
        viewport.autoscroll_edge(screen_pos);
        let mouse_beat = viewport.pixel_to_beat(screen_pos.x);

        // Unity lines 474-500: cross-layer delta
        let target_layer = viewport.layer_at_y(screen_pos.y);
        let mut layer_delta: i32 = 0;
        let total_layers = host.layer_count();
        let mut layer_blocked = false;

        if let Some(target) = target_layer
            && total_layers > 0
            && let Some(TimelineDrag::Move(m)) = self.drag.payload()
        {
            layer_delta = target as i32 - m.start_layer_index as i32;
            let min_delta = -(m.selection_min_layer as i32);
            let max_delta = (total_layers as i32 - 1) - m.selection_max_layer as i32;
            layer_delta = layer_delta.clamp(min_delta, max_delta);

            // Unity lines 488-498: type compatibility check
            if layer_delta != 0 {
                for snapshot in &m.snapshots {
                    let dest = snapshot.layer_index as i32 + layer_delta;
                    if dest < 0 || dest >= total_layers as i32 {
                        layer_delta = 0;
                        layer_blocked = true;
                        break;
                    }
                    if !host.layer_accepts_clips_from(dest as usize, snapshot.layer_index) {
                        layer_delta = 0;
                        layer_blocked = true;
                        break;
                    }
                }
            }
        }
        if let Some(TimelineDrag::Move(m)) = self.drag.payload_mut() {
            m.layer_blocked = layer_blocked;
        }

        // Unity lines 503-506: cursor feedback
        if layer_blocked {
            host.set_cursor(TimelineCursor::Blocked);
        } else {
            host.set_cursor(TimelineCursor::Move);
        }

        // D17 error shake: fire once on the RISING edge of `layer_blocked`
        // (a held-blocked drag must not re-fire every frame).
        if layer_blocked && !self.was_layer_blocked {
            self.error_shake.fire(240.0);
        }
        self.was_layer_blocked = layer_blocked;

        let Some(TimelineDrag::Move(m)) = self.drag.payload() else {
            return;
        };

        // Unity lines 508-520: apply cross-layer moves
        if layer_delta != 0 {
            for snapshot in &m.snapshots {
                let target_layer = (snapshot.layer_index as i32 + layer_delta) as usize;
                if let Some(clip) = host.find_clip_by_id(&snapshot.clip_id)
                    && target_layer != clip.layer_index
                {
                    host.move_clip_to_layer(&snapshot.clip_id, target_layer);
                }
            }
        }

        // Unity lines 522-534: magnetic snap + beat delta — shared with
        // (formerly) `finalize_move_snap`, see `move_snap_delta` (D5). The
        // clip written here IS the committed result: nothing left to reconcile
        // at release.
        let anchor_start_beat = mouse_beat - m.offset_beats;
        let beat_delta = self.move_snap_delta(anchor_start_beat, viewport);

        let Some(TimelineDrag::Move(m)) = self.drag.payload() else {
            return;
        };

        // Apply beat delta to all clips (direct mutation during drag — committed in OnEndDrag)
        // Unity line 533: movingClip.StartBeat = Max(0, snapshot.StartBeat + beatDelta)
        for snapshot in &m.snapshots {
            let new_start = (snapshot.start_beat + beat_delta).max(Beats::ZERO);
            host.set_clip_start_beat(&snapshot.clip_id, new_start);
        }
    }

    /// D5 — the shared snap+clamp math for a move drag. Given a candidate
    /// anchor beat, magnetic-snaps it against the grid and neighboring clip
    /// edges on the gesture's start layer (excluding the dragged clips
    /// themselves), then clamps the resulting delta so the group's leftmost
    /// clip cannot cross beat 0. `handle_move_drag` calls this every frame —
    /// the on-screen position IS the landed position, snap included.
    ///
    /// B12: Cmd held mid-drag bypasses snap entirely (raw position) — checked
    /// here, at the ONE call site of the shared `magnetic_snap`, not by
    /// forking a second snap implementation. The floor clamp below is a
    /// separate invariant (D5) and still applies even when snap is bypassed.
    fn move_snap_delta(
        &self,
        candidate_anchor_beat: Beats,
        viewport: &TimelineViewportPanel,
    ) -> Beats {
        let Some(TimelineDrag::Move(m)) = self.drag.payload() else {
            return Beats::ZERO;
        };
        let snapped = if self.modifiers.command {
            candidate_anchor_beat
        } else {
            viewport.magnetic_snap(
                candidate_anchor_beat,
                m.start_layer_index,
                &m.snapshot_clip_ids.iter().cloned().collect::<Vec<_>>(),
            )
        };
        let mut beat_delta = snapped - m.start_beat;
        // Clamp: don't let the leftmost clip go below beat 0. The ONE shared
        // clamp site on the move path — the per-snapshot `.max(Beats::ZERO)`
        // below is the per-clip floor applied where the model is actually
        // written, not a second independent clamp.
        beat_delta = beat_delta.max(-m.selection_min_start_beat);
        beat_delta
    }

    /// Port of Unity InteractionOverlay.HandleTrimLeftDrag (lines 539-560).
    fn handle_trim_left_drag(
        &mut self,
        screen_pos: Vec2,
        host: &mut dyn TimelineEditingHost,
        viewport: &mut TimelineViewportPanel,
    ) {
        let Some(TimelineDrag::TrimLeft(trim)) = self.drag.payload() else {
            return;
        };
        let trim_id = trim.clip_id.clone();
        let grabbed_original_start_beat = trim.original_start_beat;

        // B11: autoscroll BEFORE the beat conversion, same ordering as
        // `handle_move_drag` — a parked pointer still advances the trim as
        // the content scrolls under it.
        viewport.autoscroll_edge(screen_pos);
        let mouse_beat = viewport.pixel_to_beat(screen_pos.x);

        let min_duration = Beats(0.25); // 1/16 note minimum (Unity line 544)

        // Snap context comes from the grabbed clip; the resulting edge delta is
        // shared by every selected clip (each then clamps individually).
        // B12: Cmd held bypasses snap entirely (raw position) — same rule,
        // same shared `magnetic_snap` call, as the move path.
        let clip_layer = host.find_clip_by_id(&trim_id).map_or(0, |c| c.layer_index);
        let snapped = if self.modifiers.command {
            mouse_beat
        } else {
            viewport.magnetic_snap(mouse_beat, clip_layer, std::slice::from_ref(&trim_id))
        };
        let raw_delta = snapped - grabbed_original_start_beat;

        for orig in &trim.originals {
            let original_end = orig.start_beat + orig.duration_beats;
            // Source media can reveal previously trimmed material until its
            // in-point reaches zero; generators extend left freely.
            let mut new_start = orig.start_beat + raw_delta;
            if !orig.is_generator {
                let source_start = orig.start_beat
                    + host.clip_beats_for_source(
                        &orig.clip_id,
                        orig.start_beat,
                        Seconds(-orig.in_point.0),
                    );
                new_start = new_start.max(source_start);
            }
            new_start = new_start.min(original_end - min_duration);
            new_start = new_start.max(Beats::ZERO);

            let new_duration = original_end - new_start;
            let new_in_point = (orig.in_point
                + host.clip_source_seconds(&orig.clip_id, orig.start_beat, new_start))
            .max(Seconds::ZERO);

            // Unity lines 554-557: direct mutation during drag
            host.set_clip_trim(&orig.clip_id, new_start, new_duration, new_in_point);
        }
    }

    /// Port of Unity InteractionOverlay.HandleTrimRightDrag (lines 562-582).
    fn handle_trim_right_drag(
        &mut self,
        screen_pos: Vec2,
        host: &mut dyn TimelineEditingHost,
        viewport: &mut TimelineViewportPanel,
    ) {
        let Some(TimelineDrag::TrimRight(trim)) = self.drag.payload() else {
            return;
        };
        let trim_id = trim.clip_id.clone();
        let grabbed_original_end = trim.original_start_beat + trim.original_duration_beats;

        // B11: autoscroll BEFORE the beat conversion (see handle_trim_left_drag).
        viewport.autoscroll_edge(screen_pos);
        let mouse_beat = viewport.pixel_to_beat(screen_pos.x);

        let min_duration = Beats(0.25); // Unity line 566

        // Snap context from the grabbed clip; the edge delta fans over the
        // whole selection (each clip clamps individually). B12: Cmd bypasses
        // snap entirely (raw position) — same shared `magnetic_snap` call.
        let clip_layer = host.find_clip_by_id(&trim_id).map_or(0, |c| c.layer_index);
        let snapped = if self.modifiers.command {
            mouse_beat
        } else {
            viewport.magnetic_snap(mouse_beat, clip_layer, std::slice::from_ref(&trim_id))
        };
        let raw_delta = snapped - grabbed_original_end;

        for orig in &trim.originals {
            let new_end = (orig.start_beat + orig.duration_beats + raw_delta)
                .max(orig.start_beat + min_duration);
            let mut new_duration = new_end - orig.start_beat;

            // Unity lines 573-578: clamp to video source length when not looping
            // (generators extend freely). in_point is unchanged by a right trim.
            if !orig.is_looping && !orig.is_generator {
                let max_dur = host.get_max_duration_beats(&orig.clip_id);
                if max_dur > Beats::ZERO {
                    new_duration = new_duration.min(max_dur);
                }
            }

            // Unity line 580: trimClip.DurationBeats = newDurationBeats
            host.set_clip_trim(&orig.clip_id, orig.start_beat, new_duration, orig.in_point);
        }
    }

    // ────────────────────────────────────────────────────────────
    // DRAG HELPERS
    // ────────────────────────────────────────────────────────────

    /// Port of Unity InteractionOverlay.BeginMoveDrag (lines 592-660).
    fn begin_move_drag(
        &mut self,
        clip_id: &str,
        layer_index: usize,
        mouse_beat: Beats,
        press_pos: Vec2,
        opt_duplicate: bool,
        host: &mut dyn TimelineEditingHost,
        ui_state: &mut UIState,
        _viewport: &TimelineViewportPanel,
    ) {
        // Unity lines 598-648: region-partial move
        if let Some(region) = ui_state.current_region().cloned()
            && let Some(clip) = host.find_clip_by_id(clip_id)
        {
            {
                let layer_in_region = host
                    .layer_id_at_index(clip.layer_index)
                    .is_some_and(|lid| region.contains_layer_id(&lid));
                let hit_in_region = clip.end_beat > region.start_beat
                    && clip.start_beat < region.end_beat
                    && layer_in_region;

                if hit_in_region {
                    let split_result = host.split_clips_for_region_move(&region);

                    // Find anchor among interior clips
                    let mut anchor_id = None;
                    for interior_id in &split_result.interior_clip_ids {
                        if let Some(ic) = host.find_clip_by_id(interior_id)
                            && ic.layer_index == layer_index
                            && mouse_beat >= ic.start_beat
                            && mouse_beat < ic.end_beat
                        {
                            anchor_id = Some(interior_id.clone());
                            break;
                        }
                    }
                    // Fallback: first interior clip on same layer
                    if anchor_id.is_none() {
                        for interior_id in &split_result.interior_clip_ids {
                            if let Some(ic) = host.find_clip_by_id(interior_id)
                                && ic.layer_index == layer_index
                            {
                                anchor_id = Some(interior_id.clone());
                                break;
                            }
                        }
                    }
                    // Fallback: first interior clip
                    if anchor_id.is_none() && !split_result.interior_clip_ids.is_empty() {
                        anchor_id = Some(split_result.interior_clip_ids[0].clone());
                    }

                    if let Some(anchor) = anchor_id
                        && let Some(ac) = host.find_clip_by_id(&anchor)
                    {
                        let (
                            snapshots,
                            snapshot_clip_ids,
                            selection_min_start_beat,
                            selection_min_layer,
                            selection_max_layer,
                        ) = Self::capture_move_selection_from_ids(
                            &split_result.interior_clip_ids,
                            host,
                        );
                        let move_drag = MoveDrag {
                            anchor_clip_id: anchor.clone(),
                            start_layer_index: ac.layer_index,
                            snapshots,
                            snapshot_clip_ids,
                            selection_min_start_beat,
                            selection_min_layer,
                            selection_max_layer,
                            layer_blocked: false,
                            duplicate_on_release: opt_duplicate,
                            start_beat: ac.start_beat,
                            offset_beats: mouse_beat - ac.start_beat,
                        };
                        self.drag.start(TimelineDrag::Move(move_drag), press_pos);
                        return;
                    }
                    // No interior clips — fall through to normal move
                }
            }
        }

        // Unity lines 650-659: normal move
        if !ui_state.is_selected(clip_id) {
            let lid = host.layer_id_at_index(layer_index).unwrap_or_default();
            ui_state.select_clip(ClipId::new(clip_id), lid);
            host.on_clip_selected(clip_id);
        }
        let anchor_clip_id = ClipId::new(clip_id);
        let (start_beat, offset_beats) = match host.find_clip_by_id(clip_id) {
            Some(clip) => (clip.start_beat, mouse_beat - clip.start_beat),
            None => (Beats::ZERO, Beats::ZERO),
        };
        let (
            snapshots,
            snapshot_clip_ids,
            selection_min_start_beat,
            selection_min_layer,
            selection_max_layer,
        ) = Self::capture_drag_selection(ui_state, host, &anchor_clip_id);
        let move_drag = MoveDrag {
            anchor_clip_id,
            start_layer_index: layer_index,
            snapshots,
            snapshot_clip_ids,
            selection_min_start_beat,
            selection_min_layer,
            selection_max_layer,
            layer_blocked: false,
            duplicate_on_release: opt_duplicate,
            start_beat,
            offset_beats,
        };
        self.drag.start(TimelineDrag::Move(move_drag), press_pos);
    }

    /// Port of Unity InteractionOverlay.CaptureDragSelection (lines 695-753).
    /// Returns `(snapshots, snapshot_clip_ids, min_start_beat, min_layer,
    /// max_layer)` for the `MoveDrag` constructor (P7.5) — `fallback_anchor`
    /// is the grabbed clip, consulted only if nothing in the CURRENT
    /// selection captures (Unity's anchor-only fallback).
    fn capture_drag_selection(
        ui_state: &UIState,
        host: &dyn TimelineEditingHost,
        fallback_anchor: &ClipId,
    ) -> (Vec<DragSnapshot>, HashSet<ClipId>, Beats, usize, usize) {
        let mut snapshots = Vec::new();
        let mut snapshot_clip_ids = HashSet::new();
        let mut min_start_beat = Beats::ZERO;
        let mut min_layer = 0usize;
        let mut max_layer = 0usize;

        let selected_ids = ui_state.get_selected_clip_ids();
        let mut found_any = false;

        for id in &selected_ids {
            if let Some(clip) = host.find_clip_by_id(id) {
                if clip.is_locked {
                    continue;
                }
                snapshots.push(DragSnapshot {
                    clip_id: id.clone(),
                    start_beat: clip.start_beat,
                    layer_index: clip.layer_index,
                });
                snapshot_clip_ids.insert(id.clone());

                if !found_any {
                    min_start_beat = clip.start_beat;
                    min_layer = clip.layer_index;
                    max_layer = clip.layer_index;
                    found_any = true;
                } else {
                    min_start_beat = min_start_beat.min(clip.start_beat);
                    min_layer = min_layer.min(clip.layer_index);
                    max_layer = max_layer.max(clip.layer_index);
                }
            }
        }

        // Unity lines 740-753: fallback — anchor clip only
        if !found_any && let Some(clip) = host.find_clip_by_id(fallback_anchor) {
            snapshots.push(DragSnapshot {
                clip_id: fallback_anchor.clone(),
                start_beat: clip.start_beat,
                layer_index: clip.layer_index,
            });
            snapshot_clip_ids.insert(fallback_anchor.clone());
            min_start_beat = clip.start_beat;
            min_layer = clip.layer_index;
            max_layer = clip.layer_index;
        }

        (
            snapshots,
            snapshot_clip_ids,
            min_start_beat,
            min_layer,
            max_layer,
        )
    }

    /// Port of Unity InteractionOverlay.CaptureDragSelectionFromClips (lines 665-693).
    fn capture_move_selection_from_ids(
        clip_ids: &[ClipId],
        host: &dyn TimelineEditingHost,
    ) -> (Vec<DragSnapshot>, HashSet<ClipId>, Beats, usize, usize) {
        let mut snapshots = Vec::new();
        let mut snapshot_clip_ids = HashSet::new();
        let mut min_start_beat = Beats::ZERO;
        let mut min_layer = 0usize;
        let mut max_layer = 0usize;

        let mut first = true;
        for id in clip_ids {
            if let Some(clip) = host.find_clip_by_id(id) {
                if clip.is_locked {
                    continue;
                }
                snapshots.push(DragSnapshot {
                    clip_id: id.clone(),
                    start_beat: clip.start_beat,
                    layer_index: clip.layer_index,
                });
                snapshot_clip_ids.insert(id.clone());

                if first {
                    min_start_beat = clip.start_beat;
                    min_layer = clip.layer_index;
                    max_layer = clip.layer_index;
                    first = false;
                } else {
                    min_start_beat = min_start_beat.min(clip.start_beat);
                    min_layer = min_layer.min(clip.layer_index);
                    max_layer = max_layer.max(clip.layer_index);
                }
            }
        }

        (
            snapshots,
            snapshot_clip_ids,
            min_start_beat,
            min_layer,
            max_layer,
        )
    }

    // ────────────────────────────────────────────────────────────
    // REGION SELECT
    // ────────────────────────────────────────────────────────────

    /// Port of Unity InteractionOverlay.BeginRegionDrag (lines 778-795).
    /// Builds the `RegionDrag` payload (P7.4) — no `&mut self` needed, the
    /// caller starts the controller with the result.
    fn begin_region_drag(
        press_pos: Vec2,
        ctrl_held: bool,
        ui_state: &mut UIState,
        viewport: &TimelineViewportPanel,
    ) -> RegionDrag {
        let start_beat = viewport.pixel_to_beat(press_pos.x);
        let start_layer = viewport.layer_at_y(press_pos.y).unwrap_or(0);

        // Unity lines 793-794: clear selection unless Ctrl held
        if !ctrl_held {
            ui_state.clear_selection();
        }
        RegionDrag {
            start_beat,
            start_layer,
        }
    }

    /// Port of Unity InteractionOverlay.UpdateRegionDrag (lines 797-836).
    fn update_region_drag(
        &mut self,
        pos: Vec2,
        ui_state: &mut UIState,
        viewport: &mut TimelineViewportPanel,
        host: &dyn TimelineEditingHost,
    ) {
        let Some(TimelineDrag::RegionSelect(region)) = self.drag.payload() else {
            return;
        };
        let (region_start_beat, region_start_layer) = (region.start_beat, region.start_layer);

        // B11: edge autoscroll for rubber-band, same ordering as move/trim —
        // BEFORE the beat conversion below.
        viewport.autoscroll_edge(pos);
        let beat = viewport.pixel_to_beat(pos.x);
        let layer = viewport.layer_at_y(pos.y).unwrap_or(region_start_layer);

        let min_beat = region_start_beat.min(beat);
        let max_beat = region_start_beat.max(beat);
        let min_layer = region_start_layer.min(layer);
        let max_layer = region_start_layer.max(layer);

        // Unity lines 818-821: grid snap both edges
        let snapped_min = viewport.snap_to_grid(min_beat);
        let snapped_max = viewport.snap_to_grid(max_beat);

        // Unity line 835: update region live — bumps SelectionVersion
        ui_state.set_region(
            snapped_min,
            snapped_max,
            min_layer as i32,
            max_layer as i32,
            host.layers(),
        );
    }

    // ────────────────────────────────────────────────────────────
    // UTILITY
    // ────────────────────────────────────────────────────────────

    /// Hit-test at a screen position using the viewport's coordinate conversion.
    fn hit_test_at(&self, pos: Vec2, viewport: &TimelineViewportPanel) -> Option<ClipHitResult> {
        if !viewport.tracks_rect().contains(pos) {
            return None;
        }

        let beat = viewport.pixel_to_beat(pos.x).as_f32();
        let y_in_tracks = pos.y - viewport.tracks_rect().y;

        ClipHitTester::hit_test(
            beat,
            y_in_tracks + viewport.scroll_y_px(),
            self.clip_vertical_padding,
            viewport.mapper(),
            |layer_idx| viewport.clips_for_layer(layer_idx),
            |layer_idx| viewport.is_group_layer(layer_idx),
        )
    }

    /// Check if a clip is locked.
    fn clip_is_locked(&self, clip_id: &str, viewport: &TimelineViewportPanel) -> bool {
        (0..viewport.layer_count()).any(|i| {
            viewport
                .clips_for_layer(i)
                .iter()
                .any(|c| c.clip_id == clip_id && c.is_locked)
        })
    }
}

// ── B4 regression tests ─────────────────────────────────────────────
// A press on an
// already-selected clip must not collapse the selection until release
// without a drag; a drag begun on ANY selected member grabs the whole
// group. Reading `on_pointer_click`'s bare-click arm and
// `begin_move_drag`'s "normal move" arm (above) shows this is already
// correct by construction — `Click` and `DragBegin` are emitted as
// mutually exclusive terminal events by the input layer (`input.rs`;
// `DragBegin` fires once movement is detected, `Click` only when release
// comes with none), so nothing touches selection during the press itself
// either way; `begin_move_drag` only calls `select_clip` (collapsing) when
// the pressed clip is NOT already selected, and `capture_drag_selection`
// always fans over the CURRENT selection set. No production change was
// needed for B4 — these tests pin the contract through the real
// `on_pointer_click`/`on_begin_drag` entry points instead of leaving it
// implicit.
/// The test hosts' fixed clip tempo: 120 BPM, no warp, no tempo map.
#[cfg(test)]
const TEST_SECONDS_PER_BEAT: f64 = 0.5;

#[cfg(test)]
mod b4_group_move_tests;

// ── P1.4 gesture integrity tests ─────────────────────────────────────
// Preview == committed result, per-frame snap+clamp, Escape restores the
// pre-gesture snapshot, and a 4px drag threshold.
#[cfg(test)]
mod p1_4_gesture_integrity_tests;

// ── P2 motion tests ────────────────
// `InteractionOverlay` has no existing test harness (every other method here
// takes `&mut dyn TimelineEditingHost`, which would need a full mock to
// exercise) — these drive the new drag-visual state machine directly via
// its private fields/`tick`, which take no host, rather than building one.
#[cfg(test)]
mod motion_tests;
