//! Pure math hit-tester for automation lane strips — the automation twin of
//! `clip_hit_tester.rs`. Maps a screen position to either an existing
//! breakpoint (drag/delete target) or a bare spot on a lane's strip (click
//! adds a new breakpoint there). No engine access, no allocations beyond the
//! `Option` return.
//!
//! Operates directly over `TimelineViewportPanel::automation_lane_screens`'
//! output — the SAME geometry the renderer draws from, so a click can never
//! disagree with what's on screen (mirrors `ClipHitTester`'s "one source for
//! draw and hit-test" discipline). See `docs/AUTOMATION_LANES_DESIGN.md` section 7.

use crate::hit_targets::{HitTargetEntry, HitTargets};
use crate::node::{Rect, Vec2};
use crate::panels::viewport::AutomationLaneScreen;
use crate::view::UiGraphTarget;
use manifold_foundation::{Beats, ParamId};

/// Grab radius for an existing breakpoint dot, in screen pixels. A click
/// within this radius of a dot's center grabs/selects/deletes that dot
/// instead of adding a new point at the click location.
pub const DOT_HIT_RADIUS_PX: f32 = 7.0;

/// Distance (screen pixels) from a segment's drawn curve that still grabs the
/// segment (`docs/AUTOMATION_LANES_DESIGN.md` section 7's "drag a segment"
/// bullets). Measured to the nearest point of the drawn stroke, so the steep
/// part of a curve is as easy to grab as the flat part. Close to
/// `DOT_HIT_RADIUS_PX`: a click has to land on the line, or it falls through
/// to `Strip` (add a point).
pub const SEGMENT_HIT_DISTANCE_PX: f32 = 8.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutomationOperation {
    Point,
    Segment,
    Bend,
    Insert,
    Marquee,
    Draw,
    Resize,
    /// The lane title block: right-click opens the lane menu, left-click
    /// reveals the parameter in the inspector. Never edits the curve.
    Title,
    Blocked,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AutomationFeedback {
    pub target: UiGraphTarget,
    pub param_id: ParamId,
    pub operation: AutomationOperation,
    pub point_beat: Option<Beats>,
    pub segment_end_beat: Option<Beats>,
    pub position: Vec2,
    pub beat: Beats,
    pub value: f32,
    pub active: bool,
    pub hint: &'static str,
}

/// Resolve the visual operation without touching engine state. Points and the
/// line (segments, both tails, a placeholder's flat line) win over the resize
/// grip, even under the title, so the whole strip stays editable; the grip
/// takes only bare space; the title takes only bare space inside its own
/// rect. Alt changes a segment drag into a bend, and draw mode owns every
/// strip hit.
pub fn resolve_feedback(
    pos: Vec2,
    lanes: &[AutomationLaneScreen],
    alt: bool,
    draw_mode: bool,
) -> Option<(usize, AutomationOperation, Option<usize>)> {
    for (lane_index, lane) in lanes.iter().enumerate() {
        if !lane.strip_rect.contains(pos) { continue; }
        let editable = lane.param_min.is_finite() && lane.param_max.is_finite() && lane.param_max > lane.param_min;
        let hit = if editable { hit_test_automation(pos, std::slice::from_ref(lane)) } else { None };
        if lane.resize_rect().contains(pos) && !hit.is_some_and(|h| h.is_line_or_point()) {
            return Some((lane_index, AutomationOperation::Resize, None));
        }
        if editable && draw_mode {
            return Some((lane_index, AutomationOperation::Draw, None));
        }
        return Some(match hit {
            _ if !editable && lane.title_rect.contains(pos) =>
                (lane_index, AutomationOperation::Title, None),
            _ if !editable => (lane_index, AutomationOperation::Blocked, None),
            Some(AutomationHit::Dot { dot_index, .. }) =>
                (lane_index, AutomationOperation::Point, Some(dot_index)),
            Some(AutomationHit::Segment { left_dot_index, .. }) =>
                (lane_index, if alt && !lane.whole_numbers { AutomationOperation::Bend } else { AutomationOperation::Segment }, Some(left_dot_index)),
            Some(AutomationHit::Tail { dot_index, .. }) =>
                (lane_index, AutomationOperation::Segment, dot_index),
            _ if lane.title_rect.contains(pos) => (lane_index, AutomationOperation::Title, None),
            _ => (lane_index, AutomationOperation::Insert, None),
        });
    }
    None
}

/// The lane whose resize grip is under `pos`, unless a point or the line is
/// there too: those win the press.
pub fn resize_grip_lane(pos: Vec2, lanes: &[AutomationLaneScreen]) -> Option<usize> {
    let index = lanes.iter().position(|l| l.strip_rect.contains(pos) && l.resize_rect().contains(pos))?;
    let lane = &lanes[index];
    let editable = lane.param_min.is_finite() && lane.param_max.is_finite() && lane.param_max > lane.param_min;
    let on_line = editable
        && hit_test_automation(pos, std::slice::from_ref(lane)).is_some_and(|h| h.is_line_or_point());
    (!on_line).then_some(index)
}

/// Result of an automation hit-test.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutomationHit {
    /// An existing breakpoint. Indices are into the `lanes` slice the test
    /// was run against and that lane's `dots` — the caller re-indexes rather
    /// than this type owning a copy, mirroring `ClipHitResult`'s "identity,
    /// not a snapshot" shape.
    Dot { lane_index: usize, dot_index: usize },
    /// A point on the curve strictly between two consecutive dots — a drag
    /// grabs the segment (vertical move, or Alt for a curve bend) rather than
    /// either endpoint. `left_dot_index` is the index (into that lane's
    /// `dots`) of the point the segment LEAVES — the point whose `shape`
    /// describes it, matching `AutomationPoint::shape`'s "shape of the
    /// segment leaving this point" convention. The right endpoint is
    /// `left_dot_index + 1`.
    Segment { lane_index: usize, left_dot_index: usize },
    /// The flat run before the first point or after the last, or a
    /// placeholder lane's full-width flat line. `dot_index` is the end point
    /// the run extends from; a placeholder has none. There is no right
    /// endpoint, so a tail has no segment end.
    Tail { lane_index: usize, dot_index: Option<usize> },
    /// Empty strip area — clicking here adds a new breakpoint at `pos`.
    Strip { lane_index: usize },
}

impl AutomationHit {
    pub fn lane_index(self) -> usize {
        match self {
            Self::Dot { lane_index, .. }
            | Self::Segment { lane_index, .. }
            | Self::Tail { lane_index, .. }
            | Self::Strip { lane_index } => lane_index,
        }
    }

    /// A point or any part of the drawn line.
    pub fn is_line_or_point(self) -> bool {
        !matches!(self, Self::Strip { .. })
    }
}

/// The part of a lane's drawn line a press landed on.
enum LineHit {
    /// Between dots `i` and `i + 1`.
    Between(usize),
    /// Past the end dot (or no dots: a placeholder).
    Tail(Option<usize>),
}

/// The part of the line whose drawn stroke passes nearest `pos`, within
/// `SEGMENT_HIT_DISTANCE_PX`. Reads the lane's own polyline (the stroke the
/// renderer draws), so a grab can never disagree with what is on screen, and
/// measures true point-to-stroke distance, so a steep stretch is as
/// grabbable as a flat one. Only stroke edges near `pos.x` are visited.
fn nearest_line(lane: &AutomationLaneScreen, pos: Vec2) -> Option<LineHit> {
    let line = &lane.polyline;
    let reach = SEGMENT_HIT_DISTANCE_PX;
    let start = line.partition_point(|p| p.0 < pos.x - reach).saturating_sub(1);
    let mut best: Option<(f32, f32)> = None;
    for edge in line[start..].windows(2) {
        let (a, b) = (edge[0], edge[1]);
        if a.0 > pos.x + reach {
            break;
        }
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let len_sq = dx * dx + dy * dy;
        let t = if len_sq > 0.0 {
            (((pos.x - a.0) * dx + (pos.y - a.1) * dy) / len_sq).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let dist_sq = (pos.x - a.0 - t * dx).powi(2) + (pos.y - a.1 - t * dy).powi(2);
        if dist_sq <= reach * reach && best.is_none_or(|(d, _)| dist_sq < d) {
            best = Some((dist_sq, (a.0 + b.0) * 0.5));
        }
    }
    // The stroke breaks at every point's exact x, so an edge never straddles a
    // point. A Hold step's vertical edge sits on its right point and belongs to
    // the segment arriving there.
    let edge_x = best?.1;
    if lane.dots.is_empty() {
        return Some(LineHit::Tail(None));
    }
    if let Some(left) = lane.dots.windows(2).position(|pair| pair[0].x < edge_x && edge_x <= pair[1].x) {
        return Some(LineHit::Between(left));
    }
    // Not between two dots: the flat run before the first or after the last.
    let last = lane.dots.len().checked_sub(1);
    Some(LineHit::Tail(if edge_x <= lane.dots.first()?.x { Some(0) } else { last }))
}

/// Hit-test `pos` (screen-space, same coordinates `AutomationLaneScreen::
/// strip_rect`/`dots` are expressed in) against every visible lane strip.
/// Returns `None` when `pos` isn't inside any strip's `strip_rect` — the
/// caller then falls through to ordinary clip/track hit-testing.
///
/// Nearest-dot-within-radius wins over "empty strip" so a click that's
/// technically inside a dot's bounding area but slightly off-line still
/// grabs the dot rather than adding a spurious second point beside it.
pub fn hit_test_automation(pos: Vec2, lanes: &[AutomationLaneScreen]) -> Option<AutomationHit> {
    for (lane_index, lane) in lanes.iter().enumerate() {
        if !lane.strip_rect.contains(pos) {
            continue;
        }
        let mut nearest: Option<(usize, f32)> = None;
        for (dot_index, dot) in lane.dots.iter().enumerate() {
            // `automation_lane_screens` includes edge-adjacent endpoints so
            // crossing segments remain editable. They are renderer inputs,
            // not point hit targets: only dots inside the strip's horizontal
            // viewport may be grabbed.
            if dot.x < lane.strip_rect.x || dot.x > lane.strip_rect.x_max() {
                continue;
            }
            let dx = pos.x - dot.x;
            let dy = pos.y - dot.y;
            let dist_sq = dx * dx + dy * dy;
            if dist_sq <= DOT_HIT_RADIUS_PX * DOT_HIT_RADIUS_PX
                && nearest.is_none_or(|(_, best)| dist_sq < best)
            {
                nearest = Some((dot_index, dist_sq));
            }
        }
        if let Some((dot_index, _)) = nearest {
            return Some(AutomationHit::Dot { lane_index, dot_index });
        }

        // No dot grabbed: a press on the drawn line grabs it. Between two
        // points that is the segment (drag moves it, Alt-drag bends it);
        // past the end points, or on a placeholder, it is a tail.
        match nearest_line(lane, pos) {
            Some(LineHit::Between(left_dot_index)) =>
                return Some(AutomationHit::Segment { lane_index, left_dot_index }),
            Some(LineHit::Tail(dot_index)) =>
                return Some(AutomationHit::Tail { lane_index, dot_index }),
            None => {}
        }

        return Some(AutomationHit::Strip { lane_index });
    }
    None
}

// ── Automation surface (UI_AUTOMATION_DESIGN.md D5/section 5) ───────────

/// Stable text form of a lane's addressing target, shared by the strip and
/// point payloads below (`"effect:<id>"` / `"generator:<id>"`).
fn target_key(target: &crate::view::UiGraphTarget) -> String {
    match target {
        crate::view::UiGraphTarget::Effect(id) => format!("effect:{}", id.as_str()),
        crate::view::UiGraphTarget::Generator(id) => format!("generator:{}", id.as_str()),
        crate::view::UiGraphTarget::SceneModifier { owner, modifier_id } => {
            let owner = target_key(owner);
            format!("modifier:{}:{owner}:{}", owner.len(), modifier_id.as_str())
        }
    }
}

/// [`HitTargets`] over the currently-visible automation lane strips — the same
/// [`AutomationLaneScreen`] list `hit_test_automation` and the lane-strip
/// renderer both read (`TimelineViewportPanel::automation_lane_screens`), so an
/// enumerated strip/point's rect can never disagree with what's on screen or
/// what's clickable.
pub struct AutomationHitTargets<'a>(pub &'a [AutomationLaneScreen]);

impl HitTargets for AutomationHitTargets<'_> {
    fn surface_id(&self) -> &'static str {
        "automation_lanes"
    }

    fn enumerate(&self, out: &mut Vec<HitTargetEntry>) {
        for lane in self.0 {
            let key = target_key(&lane.target);
            out.push(HitTargetEntry {
                kind: "automation_strip",
                label: lane.label.clone(),
                rect: lane.strip_rect,
                payload: format!("{key}|{}", lane.param_id.as_ref()),
            });
            for (dot_index, dot) in lane.dots.iter().enumerate() {
                if dot.x < lane.strip_rect.x || dot.x > lane.strip_rect.x_max() {
                    continue;
                }
                let d = DOT_HIT_RADIUS_PX;
                out.push(HitTargetEntry {
                    kind: "automation_point",
                    label: format!("{} pt{dot_index}", lane.label),
                    rect: Rect::new(dot.x - d, dot.y - d, d * 2.0, d * 2.0),
                    payload: format!("{key}|{}|{dot_index}", lane.param_id.as_ref()),
                });
            }
            out.push(HitTargetEntry {
                kind: "automation_resize",
                label: lane.label.clone(),
                rect: lane.resize_rect(),
                payload: format!("{key}|{}", lane.param_id.as_ref()),
            });
            out.push(HitTargetEntry {
                kind: "automation_title",
                label: lane.label.clone(),
                rect: lane.title_rect,
                payload: format!("{key}|{}", lane.param_id.as_ref()),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::Rect;
    use crate::panels::viewport::AutomationDotScreen;
    use crate::view::{UiGraphTarget, UiSegmentShape};
    use manifold_foundation::{Beats, EffectId, ParamId};

    fn test_lane(strip_rect: Rect, mut dots: Vec<AutomationDotScreen>) -> AutomationLaneScreen {
        let graph = AutomationLaneScreen::curve_rect_for(strip_rect);
        for dot in &mut dots { dot.y = graph.y + graph.height * ((dot.y - strip_rect.y) / strip_rect.height); }
        // The drawn stroke, sampled densely from each segment's shape.
        let mut polyline = Vec::new();
        for pair in dots.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            for step in 0..=400 {
                let t = step as f32 / 400.0;
                let norm = a.value_norm + (b.value_norm - a.value_norm) * a.shape.sample(t);
                polyline.push((a.x + (b.x - a.x) * t, graph.y + graph.height * (1.0 - norm)));
            }
        }
        // The flat runs before the first dot and after the last, as drawn.
        if let (Some(first), Some(last)) = (dots.first().copied(), dots.last().copied()) {
            if first.x > strip_rect.x {
                polyline.insert(0, (strip_rect.x, first.y));
            }
            if last.x < strip_rect.x_max() {
                polyline.push((strip_rect.x_max(), last.y));
            }
        }
        AutomationLaneScreen {
            strip_rect,
            pixels_per_beat: 48.0,
            visible_beat_start: 0.0,
            grid_step: 0.25,
            beats_per_bar: 4.0,
            bar_skip: 1,
            label: "Test: amount".into(),
            effect_id: EffectId::new("fx"),
            polyline,
            dots,
            target: UiGraphTarget::Effect(EffectId::new("fx")),
            param_id: ParamId::from("amount"),
            param_min: 0.0,
            param_max: 1.0,
            whole_numbers: false,
            placeholder_value: None,
            display: crate::view::UiLaneDisplay::default(),
            // Top-right corner block, clear of every test's points.
            title_rect: Rect::new(strip_rect.x_max() - 30.0, strip_rect.y + 2.0, 20.0, 10.0),
            title_value_w: 0.0,
            playhead_value: 0.0,
        }
    }


    fn graph_hit(mut pos: Vec2, lanes: &[AutomationLaneScreen]) -> Option<AutomationHit> {
        if let Some(lane) = lanes.iter().find(|l| l.strip_rect.contains(pos)) {
            let norm = 1.0 - (pos.y - lane.strip_rect.y) / lane.strip_rect.height;
            pos.y = lane.y_at_norm(norm);
        }
        hit_test_automation(pos, lanes)
    }

    fn dot(x: f32, y: f32) -> AutomationDotScreen {
        AutomationDotScreen {
            x,
            y,
            beat: Beats(x as f64),
            value_norm: 0.5,
            shape: UiSegmentShape::Linear,
            past_end: false,
        }
    }

    #[test]
    fn outside_every_strip_misses() {
        let lanes = vec![test_lane(Rect::new(0.0, 0.0, 100.0, 28.0), vec![])];
        assert_eq!(graph_hit(Vec2::new(50.0, 200.0), &lanes), None);
    }

    #[test]
    fn empty_strip_area_hits_strip_not_dot() {
        let lanes = vec![test_lane(Rect::new(0.0, 0.0, 100.0, 28.0), vec![dot(10.0, 14.0)])];
        // Far from the one dot and clear of its flat run, but inside the strip rect.
        let hit = hit_test_automation(Vec2::new(90.0, 2.0), &lanes);
        assert_eq!(hit, Some(AutomationHit::Strip { lane_index: 0 }));
    }

    // ── P6a: the line is one object ─────────────────────────────────

    fn flat_lane(dots: Vec<AutomationDotScreen>) -> AutomationLaneScreen {
        test_lane(Rect::new(0.0, 0.0, 100.0, 100.0), dots)
    }

    #[test]
    fn tails_before_the_first_and_after_the_last_point_are_line_hits() {
        let lanes = vec![flat_lane(vec![
            dot_v(30.0, 0.5, UiSegmentShape::Linear, 100.0),
            dot_v(70.0, 0.8, UiSegmentShape::Linear, 100.0),
        ])];
        let lane = &lanes[0];
        let before = Vec2::new(10.0, lane.dots[0].y);
        let after = Vec2::new(90.0, lane.dots[1].y);
        assert_eq!(hit_test_automation(before, &lanes), Some(AutomationHit::Tail { lane_index: 0, dot_index: Some(0) }));
        assert_eq!(hit_test_automation(after, &lanes), Some(AutomationHit::Tail { lane_index: 0, dot_index: Some(1) }));
        // A tail is a Segment operation with no right endpoint to name.
        assert_eq!(resolve_feedback(before, &lanes, false, false), Some((0, AutomationOperation::Segment, Some(0))));
        assert_eq!(resolve_feedback(after, &lanes, false, false), Some((0, AutomationOperation::Segment, Some(1))));
        // Alt still bends a segment only: a tail has nothing to bend.
        assert_eq!(resolve_feedback(after, &lanes, true, false).map(|f| f.1), Some(AutomationOperation::Segment));
        // The same distance rule as a segment: off the stroke is bare strip.
        let off = Vec2::new(90.0, lane.dots[1].y + SEGMENT_HIT_DISTANCE_PX + 2.0);
        assert_eq!(hit_test_automation(off, &lanes), Some(AutomationHit::Strip { lane_index: 0 }));
    }

    #[test]
    fn single_point_lane_has_a_tail_either_side_of_its_dot() {
        let lanes = vec![flat_lane(vec![dot_v(50.0, 0.4, UiSegmentShape::Linear, 100.0)])];
        let y = lanes[0].dots[0].y;
        for x in [10.0, 90.0] {
            assert_eq!(
                hit_test_automation(Vec2::new(x, y), &lanes),
                Some(AutomationHit::Tail { lane_index: 0, dot_index: Some(0) }),
                "x {x}"
            );
        }
        assert_eq!(hit_test_automation(Vec2::new(50.0, y), &lanes), Some(AutomationHit::Dot { lane_index: 0, dot_index: 0 }));
    }

    #[test]
    fn placeholder_line_is_a_tail_hit_with_no_point() {
        let mut lane = flat_lane(vec![]);
        let y = lane.y_at_norm(0.3);
        lane.polyline = vec![(0.0, y), (100.0, y)];
        lane.placeholder_value = Some(0.3);
        let lanes = vec![lane];
        let on = Vec2::new(60.0, y);
        assert_eq!(hit_test_automation(on, &lanes), Some(AutomationHit::Tail { lane_index: 0, dot_index: None }));
        assert_eq!(resolve_feedback(on, &lanes, false, false), Some((0, AutomationOperation::Segment, None)));
        let off = Vec2::new(60.0, y + SEGMENT_HIT_DISTANCE_PX + 2.0);
        assert_eq!(hit_test_automation(off, &lanes), Some(AutomationHit::Strip { lane_index: 0 }));
    }

    #[test]
    fn points_and_line_win_over_the_resize_grip() {
        let mut lane = flat_lane(vec![
            dot_v(30.0, 0.0, UiSegmentShape::Linear, 100.0),
            dot_v(70.0, 0.0, UiSegmentShape::Linear, 100.0),
        ]);
        let grip = lane.resize_rect();
        let inside = Vec2::new(10.0, grip.y + 2.0);
        let lanes_with_line = vec![lane.clone()];
        assert_eq!(resolve_feedback(inside, &lanes_with_line, false, false).map(|f| f.1), Some(AutomationOperation::Segment));
        assert_eq!(resize_grip_lane(inside, &lanes_with_line), None);
        // With no line there, the grip is the grip.
        lane.polyline.clear();
        lane.dots.clear();
        let lanes = vec![lane];
        assert_eq!(resolve_feedback(inside, &lanes, false, false).map(|f| f.1), Some(AutomationOperation::Resize));
        assert_eq!(resize_grip_lane(inside, &lanes), Some(0));
    }

    #[test]
    fn click_within_radius_grabs_the_dot() {
        let lanes = vec![test_lane(Rect::new(0.0, 0.0, 100.0, 28.0), vec![dot(10.0, 14.0)])];
        let hit = graph_hit(Vec2::new(12.0, 15.0), &lanes);
        assert_eq!(hit, Some(AutomationHit::Dot { lane_index: 0, dot_index: 0 }));
    }

    #[test]
    fn picks_the_nearest_dot_when_two_are_in_range() {
        let lanes = vec![test_lane(
            Rect::new(0.0, 0.0, 100.0, 28.0),
            vec![dot(10.0, 14.0), dot(13.0, 14.0)],
        )];
        // Closer to the second dot (index 1) than the first.
        let hit = graph_hit(Vec2::new(12.5, 14.0), &lanes);
        assert_eq!(hit, Some(AutomationHit::Dot { lane_index: 0, dot_index: 1 }));
    }

    #[test]
    fn second_lane_is_addressed_correctly() {
        let lanes = vec![
            test_lane(Rect::new(0.0, 0.0, 100.0, 28.0), vec![]),
            test_lane(Rect::new(0.0, 28.0, 100.0, 28.0), vec![dot(10.0, 42.0)]),
        ];
        let hit = graph_hit(Vec2::new(10.0, 42.0), &lanes);
        assert_eq!(hit, Some(AutomationHit::Dot { lane_index: 1, dot_index: 0 }));
    }

    // ── Segment hit-testing (P4 Unit B) ─────────────────────────────

    fn dot_v(x: f32, value_norm: f32, shape: UiSegmentShape, strip_h: f32) -> AutomationDotScreen {
        AutomationDotScreen {
            x,
            y: strip_h * (1.0 - value_norm),
            beat: Beats(x as f64),
            value_norm,
            shape,
            past_end: false,
        }
    }

    #[test]
    fn segment_hit_on_linear_midpoint() {
        // strip 100x100: dot0 at value_norm 1.0 (y=0), dot1 at value_norm 0.0
        // (y=100). Midpoint of a straight line is (50, 50).
        let lanes = vec![test_lane(
            Rect::new(0.0, 0.0, 100.0, 100.0),
            vec![
                dot_v(0.0, 1.0, UiSegmentShape::Linear, 100.0),
                dot_v(100.0, 0.0, UiSegmentShape::Linear, 100.0),
            ],
        )];
        let hit = graph_hit(Vec2::new(50.0, 50.0), &lanes);
        assert_eq!(hit, Some(AutomationHit::Segment { lane_index: 0, left_dot_index: 0 }));
    }

    #[test]
    fn segment_hit_follows_the_curve_not_the_straight_line() {
        // dot0 Curved(1.0): exponent = 1 + 1*3 = 4, so t=0.5 -> shaped = 0.5^4 = 0.0625.
        // value_norm goes 0.0 -> 1.0, so at x=50 the curve sits near y=93.75
        // (H=100), NOT the straight-line midpoint y=50.
        let lanes = vec![test_lane(
            Rect::new(0.0, 0.0, 100.0, 100.0),
            vec![
                dot_v(0.0, 0.0, UiSegmentShape::Curved(1.0), 100.0),
                dot_v(100.0, 1.0, UiSegmentShape::Linear, 100.0),
            ],
        )];
        // The straight-line midpoint (50, 50) is far from the actual curve
        // (which sits near y=93.75) — must NOT register as a segment hit.
        let miss = graph_hit(Vec2::new(50.0, 50.0), &lanes);
        assert_eq!(
            miss,
            Some(AutomationHit::Strip { lane_index: 0 }),
            "a click at the straight-line midpoint must miss a bent curve's actual position"
        );
        // The curve's real position at x=50 IS a hit.
        let hit = graph_hit(Vec2::new(50.0, 93.75), &lanes);
        assert_eq!(hit, Some(AutomationHit::Segment { lane_index: 0, left_dot_index: 0 }));
    }

    #[test]
    fn segment_hit_respects_hold_shape_flat_step() {
        // Hold: the whole segment sits flat at dot0's value until the very
        // end (mirrors `UiAutomationLane::value_at_norm`'s Hold arm).
        let lanes = vec![test_lane(
            Rect::new(0.0, 0.0, 100.0, 100.0),
            vec![
                dot_v(0.0, 0.2, UiSegmentShape::Hold, 100.0),
                dot_v(100.0, 0.9, UiSegmentShape::Linear, 100.0),
            ],
        )];
        let flat_y = 100.0 * (1.0 - 0.2);
        let hit = graph_hit(Vec2::new(60.0, flat_y), &lanes);
        assert_eq!(hit, Some(AutomationHit::Segment { lane_index: 0, left_dot_index: 0 }));
    }

    #[test]
    fn dot_grab_wins_over_segment_near_an_endpoint() {
        // A click near the left dot must grab the DOT, not the segment
        // passing through that same point — dots are checked first.
        let lanes = vec![test_lane(
            Rect::new(0.0, 0.0, 100.0, 100.0),
            vec![
                dot_v(0.0, 1.0, UiSegmentShape::Linear, 100.0),
                dot_v(100.0, 0.0, UiSegmentShape::Linear, 100.0),
            ],
        )];
        let hit = graph_hit(Vec2::new(2.0, 1.0), &lanes);
        assert_eq!(hit, Some(AutomationHit::Dot { lane_index: 0, dot_index: 0 }));
    }

    #[test]
    fn segment_far_from_curve_falls_through_to_strip() {
        let lanes = vec![test_lane(
            Rect::new(0.0, 0.0, 100.0, 100.0),
            vec![
                dot_v(0.0, 1.0, UiSegmentShape::Linear, 100.0),
                dot_v(100.0, 0.0, UiSegmentShape::Linear, 100.0),
            ],
        )];
        // Straight line passes through (50, 50); (50, 5) is far above it.
        let hit = graph_hit(Vec2::new(50.0, 5.0), &lanes);
        assert_eq!(hit, Some(AutomationHit::Strip { lane_index: 0 }));
    }

    #[test]
    fn crossing_segment_remains_hit_testable_with_both_endpoints_offscreen() {
        let lanes = vec![test_lane(
            Rect::new(0.0, 0.0, 100.0, 100.0),
            vec![
                dot_v(-20.0, 1.0, UiSegmentShape::Curved(-0.5), 100.0),
                dot_v(120.0, 0.0, UiSegmentShape::Curved(-1.0), 100.0),
            ],
        )];
        // At x=50, the first curved segment's actual curve is below the
        // straight midpoint; this point is inside the viewport even though
        // both breakpoint endpoints are outside it.
        let t = (50.0 + 20.0) / 140.0;
        let shaped = UiSegmentShape::Curved(-0.5).sample(t);
        let y = 100.0 * (1.0 - (1.0 - shaped));
        assert_eq!(graph_hit(Vec2::new(50.0, y), &lanes),
            Some(AutomationHit::Segment { lane_index: 0, left_dot_index: 0 }));
    }

    #[test]
    fn steep_stretch_of_a_curve_is_a_segment_hit() {
        let lanes = vec![test_lane(
            Rect::new(0.0, 0.0, 100.0, 100.0),
            vec![
                dot_v(0.0, 0.0, UiSegmentShape::Curved(1.0), 100.0),
                dot_v(100.0, 1.0, UiSegmentShape::Linear, 100.0),
            ],
        )];
        let lane = &lanes[0];
        let on_curve = |t: f32| lane.y_at_norm(UiSegmentShape::Curved(1.0).sample(t));
        // Four pixels left of the stroke where it climbs steeply: more than
        // the grab distance away vertically, well within it in true distance.
        let pos = Vec2::new(91.0, on_curve(0.95));
        assert!((pos.y - on_curve(0.91)).abs() > SEGMENT_HIT_DISTANCE_PX);
        assert_eq!(hit_test_automation(pos, &lanes),
            Some(AutomationHit::Segment { lane_index: 0, left_dot_index: 0 }));
        assert_eq!(resolve_feedback(pos, &lanes, false, false).map(|f| f.1),
            Some(AutomationOperation::Segment));
        assert_eq!(resolve_feedback(pos, &lanes, true, false).map(|f| f.1),
            Some(AutomationOperation::Bend));
    }

    #[test]
    fn title_takes_only_bare_space_inside_its_rect() {
        let strip = Rect::new(0.0, 0.0, 200.0, 60.0);
        let mut lane = test_lane(strip, vec![dot_v(165.0, 0.9, UiSegmentShape::Linear, 60.0)]);
        lane.title_rect = Rect::new(150.0, 3.0, 40.0, 30.0);
        let lanes = vec![lane];
        let op = |x: f32, y: f32| resolve_feedback(Vec2::new(x, y), &lanes, false, false).map(|f| f.1);
        assert_eq!(op(185.0, 25.0), Some(AutomationOperation::Title));
        // A point under the title text is still grabbed.
        let dot = lanes[0].dots[0];
        assert_eq!(op(dot.x, dot.y), Some(AutomationOperation::Point));
        // Just outside the title, and everywhere else in the strip, is curve.
        assert_eq!(op(148.0, 25.0), Some(AutomationOperation::Insert));
        assert_eq!(op(185.0, 40.0), Some(AutomationOperation::Insert));
        assert_eq!(op(185.0, 0.5), Some(AutomationOperation::Insert));
        assert_eq!(op(185.0, 59.5), Some(AutomationOperation::Insert));
        // Draw mode owns the whole strip, title included.
        assert_eq!(resolve_feedback(Vec2::new(185.0, 25.0), &lanes, false, true).map(|f| f.1),
            Some(AutomationOperation::Draw));
    }

    #[test]
    fn offscreen_dot_is_never_a_point_hit() {
        let lanes = vec![test_lane(
            Rect::new(0.0, 0.0, 100.0, 28.0),
            vec![dot(-1.0, 14.0)],
        )];
        assert_eq!(graph_hit(Vec2::new(0.0, 14.0), &lanes),
            Some(AutomationHit::Strip { lane_index: 0 }));
    }

    // ── HitTargets (UI_AUTOMATION_DESIGN.md P1) ──────────────────────

    #[test]
    fn automation_hit_targets_enumerates_strips_and_points_with_payloads() {
        let lanes = vec![test_lane(
            Rect::new(0.0, 0.0, 100.0, 28.0),
            vec![dot(10.0, 14.0), dot(60.0, 14.0)],
        )];
        let targets = AutomationHitTargets(&lanes);
        assert_eq!(targets.surface_id(), "automation_lanes");
        let mut out = Vec::new();
        targets.enumerate(&mut out);
        // One strip, one entry per dot, the resize handle and the title.
        assert_eq!(out.len(), 5);
        assert_eq!(out[0].kind, "automation_strip");
        assert_eq!(out[0].payload, "effect:fx|amount");
        assert_eq!(out[1].kind, "automation_point");
        assert_eq!(out[1].payload, "effect:fx|amount|0");
        assert_eq!(out[2].payload, "effect:fx|amount|1");
        assert_eq!(out[3].kind, "automation_resize");
        assert_eq!(out[3].rect, lanes[0].resize_rect());
        assert_eq!(out[4].kind, "automation_title");
        assert_eq!(out[4].rect, lanes[0].title_rect);
    }

    #[test]
    fn automation_hit_targets_excludes_edge_adjacent_offscreen_points() {
        let lanes = vec![test_lane(
            Rect::new(0.0, 0.0, 100.0, 28.0),
            vec![dot(-1.0, 14.0), dot(50.0, 14.0), dot(101.0, 14.0)],
        )];
        let mut out = Vec::new();
        AutomationHitTargets(&lanes).enumerate(&mut out);
        assert_eq!(out.iter().filter(|entry| entry.kind == "automation_point").count(), 1);
        assert_eq!(out[1].payload, "effect:fx|amount|1");
    }
}
