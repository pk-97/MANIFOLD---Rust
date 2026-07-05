//! Pure math hit-tester for automation lane strips — the automation twin of
//! `clip_hit_tester.rs`. Maps a screen position to either an existing
//! breakpoint (drag/delete target) or a bare spot on a lane's strip (click
//! adds a new breakpoint there). No engine access, no allocations beyond the
//! `Option` return.
//!
//! Operates directly over `TimelineViewportPanel::automation_lane_screens`'
//! output — the SAME geometry the renderer draws from, so a click can never
//! disagree with what's on screen (mirrors `ClipHitTester`'s "one source for
//! draw and hit-test" discipline). See `docs/AUTOMATION_LANES_DESIGN.md` §7.

use crate::hit_targets::{HitTargetEntry, HitTargets};
use crate::node::{Rect, Vec2};
use crate::panels::viewport::AutomationLaneScreen;
use crate::view::automation_segment_bend;

/// Grab radius for an existing breakpoint dot, in screen pixels. A click
/// within this radius of a dot's center grabs/selects/deletes that dot
/// instead of adding a new point at the click location.
pub const DOT_HIT_RADIUS_PX: f32 = 7.0;

/// Vertical tolerance (screen pixels) around a segment's drawn curve for a
/// "grab this segment" hit — P4 Unit B (`docs/AUTOMATION_LANES_DESIGN.md`
/// §7's "drag a segment" / "modifier-drag a segment" bullets). Deliberately
/// close to `DOT_HIT_RADIUS_PX` — a click has to land ON the line, not just
/// somewhere in the strip, or it falls through to `Strip` (add-a-point).
pub const SEGMENT_HIT_DISTANCE_PX: f32 = 8.0;

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
    /// Empty strip area — clicking here adds a new breakpoint at `pos`.
    Strip { lane_index: usize },
}

/// The on-screen Y a segment's curve occupies at `pos.x`, using the same
/// per-shape math the renderer's polyline sampling uses (`viewport.rs`'s
/// `automation_lane_screens` / `UiAutomationLane::value_at_norm`) — so a
/// segment grab can never disagree with what's drawn. Returns `None` when
/// `pos.x` isn't between the two dots (segment doesn't span that X).
fn segment_screen_y(
    lane: &AutomationLaneScreen,
    left: &crate::panels::viewport::AutomationDotScreen,
    right: &crate::panels::viewport::AutomationDotScreen,
    x: f32,
) -> Option<f32> {
    let span = right.x - left.x;
    if span <= 0.0 || x < left.x || x > right.x {
        return None;
    }
    let t = (x - left.x) / span;
    let norm = match left.shape {
        crate::view::UiSegmentShape::Hold => left.value_norm,
        crate::view::UiSegmentShape::Linear => {
            left.value_norm + (right.value_norm - left.value_norm) * t
        }
        crate::view::UiSegmentShape::Curved(bend) => {
            let shaped = automation_segment_bend(t, bend);
            left.value_norm + (right.value_norm - left.value_norm) * shaped
        }
    };
    Some(lane.strip_rect.y + lane.strip_rect.height * (1.0 - norm))
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

        // No dot grabbed — check whether `pos` lands on the curve between two
        // consecutive dots (P4 Unit B: segment drag / Alt-drag curve bend).
        for left_dot_index in 0..lane.dots.len().saturating_sub(1) {
            let left = &lane.dots[left_dot_index];
            let right = &lane.dots[left_dot_index + 1];
            if let Some(y) = segment_screen_y(lane, left, right, pos.x)
                && (pos.y - y).abs() <= SEGMENT_HIT_DISTANCE_PX
            {
                return Some(AutomationHit::Segment { lane_index, left_dot_index });
            }
        }

        return Some(AutomationHit::Strip { lane_index });
    }
    None
}

/// Build the marquee rect from two screen-space corners in ANY order (the
/// drag's press position and its current position) — normalizes to a
/// non-negative-size `Rect` regardless of drag direction.
pub fn marquee_rect(a: Vec2, b: Vec2) -> Rect {
    let x0 = a.x.min(b.x);
    let y0 = a.y.min(b.y);
    let x1 = a.x.max(b.x);
    let y1 = a.y.max(b.y);
    Rect::new(x0, y0, x1 - x0, y1 - y0)
}

/// All `(lane_index, dot_index)` pairs whose dot falls within `rect` — the
/// pure core of marquee-select (P4 Unit B, §7's "Marquee-select multiple
/// dots"). `InteractionOverlay` calls this every frame during an
/// `AutomationMarquee` drag to refresh `UIState::selected_automation_points`.
pub fn dots_in_rect(rect: Rect, lanes: &[AutomationLaneScreen]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    for (lane_index, lane) in lanes.iter().enumerate() {
        for (dot_index, dot) in lane.dots.iter().enumerate() {
            if rect.contains(Vec2::new(dot.x, dot.y)) {
                out.push((lane_index, dot_index));
            }
        }
    }
    out
}

// ── Automation surface (UI_AUTOMATION_DESIGN.md D5/§5) ───────────

/// Stable text form of a lane's addressing target, shared by the strip and
/// point payloads below (`"effect:<id>"` / `"generator:<id>"`).
fn target_key(target: &crate::view::UiGraphTarget) -> String {
    match target {
        crate::view::UiGraphTarget::Effect(id) => format!("effect:{}", id.as_str()),
        crate::view::UiGraphTarget::Generator(id) => format!("generator:{}", id.as_str()),
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
                let d = DOT_HIT_RADIUS_PX;
                out.push(HitTargetEntry {
                    kind: "automation_point",
                    label: format!("{} pt{dot_index}", lane.label),
                    rect: Rect::new(dot.x - d, dot.y - d, d * 2.0, d * 2.0),
                    payload: format!("{key}|{}|{dot_index}", lane.param_id.as_ref()),
                });
            }
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

    fn test_lane(strip_rect: Rect, dots: Vec<AutomationDotScreen>) -> AutomationLaneScreen {
        AutomationLaneScreen {
            strip_rect,
            label: "Test: amount".into(),
            overridden: false,
            polyline: Vec::new(),
            dots,
            target: UiGraphTarget::Effect(EffectId::new("fx")),
            param_id: ParamId::from("amount"),
            param_min: 0.0,
            param_max: 1.0,
            whole_numbers: false,
        }
    }

    fn dot(x: f32, y: f32) -> AutomationDotScreen {
        AutomationDotScreen {
            x,
            y,
            beat: Beats(x as f64),
            value_norm: 0.5,
            shape: UiSegmentShape::Linear,
        }
    }

    #[test]
    fn outside_every_strip_misses() {
        let lanes = vec![test_lane(Rect::new(0.0, 0.0, 100.0, 28.0), vec![])];
        assert_eq!(hit_test_automation(Vec2::new(50.0, 200.0), &lanes), None);
    }

    #[test]
    fn empty_strip_area_hits_strip_not_dot() {
        let lanes = vec![test_lane(Rect::new(0.0, 0.0, 100.0, 28.0), vec![dot(10.0, 14.0)])];
        // Far from the one dot, but still inside the strip rect.
        let hit = hit_test_automation(Vec2::new(90.0, 14.0), &lanes);
        assert_eq!(hit, Some(AutomationHit::Strip { lane_index: 0 }));
    }

    #[test]
    fn click_within_radius_grabs_the_dot() {
        let lanes = vec![test_lane(Rect::new(0.0, 0.0, 100.0, 28.0), vec![dot(10.0, 14.0)])];
        let hit = hit_test_automation(Vec2::new(12.0, 15.0), &lanes);
        assert_eq!(hit, Some(AutomationHit::Dot { lane_index: 0, dot_index: 0 }));
    }

    #[test]
    fn picks_the_nearest_dot_when_two_are_in_range() {
        let lanes = vec![test_lane(
            Rect::new(0.0, 0.0, 100.0, 28.0),
            vec![dot(10.0, 14.0), dot(13.0, 14.0)],
        )];
        // Closer to the second dot (index 1) than the first.
        let hit = hit_test_automation(Vec2::new(12.5, 14.0), &lanes);
        assert_eq!(hit, Some(AutomationHit::Dot { lane_index: 0, dot_index: 1 }));
    }

    #[test]
    fn second_lane_is_addressed_correctly() {
        let lanes = vec![
            test_lane(Rect::new(0.0, 0.0, 100.0, 28.0), vec![]),
            test_lane(Rect::new(0.0, 28.0, 100.0, 28.0), vec![dot(10.0, 42.0)]),
        ];
        let hit = hit_test_automation(Vec2::new(10.0, 42.0), &lanes);
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
        let hit = hit_test_automation(Vec2::new(50.0, 50.0), &lanes);
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
        let miss = hit_test_automation(Vec2::new(50.0, 50.0), &lanes);
        assert_eq!(
            miss,
            Some(AutomationHit::Strip { lane_index: 0 }),
            "a click at the straight-line midpoint must miss a bent curve's actual position"
        );
        // The curve's real position at x=50 IS a hit.
        let hit = hit_test_automation(Vec2::new(50.0, 93.75), &lanes);
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
        let hit = hit_test_automation(Vec2::new(60.0, flat_y), &lanes);
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
        let hit = hit_test_automation(Vec2::new(2.0, 1.0), &lanes);
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
        let hit = hit_test_automation(Vec2::new(50.0, 5.0), &lanes);
        assert_eq!(hit, Some(AutomationHit::Strip { lane_index: 0 }));
    }

    // ── Marquee-select (P4 Unit B) ───────────────────────────────────

    #[test]
    fn marquee_rect_normalizes_any_drag_direction() {
        // Dragging bottom-right to top-left must produce the SAME rect as
        // top-left to bottom-right.
        let a = marquee_rect(Vec2::new(10.0, 10.0), Vec2::new(50.0, 40.0));
        let b = marquee_rect(Vec2::new(50.0, 40.0), Vec2::new(10.0, 10.0));
        assert_eq!(a, b);
        assert_eq!(a, Rect::new(10.0, 10.0, 40.0, 30.0));
    }

    #[test]
    fn dots_in_rect_selects_only_dots_inside() {
        let lanes = vec![
            test_lane(
                Rect::new(0.0, 0.0, 100.0, 28.0),
                vec![dot(10.0, 14.0), dot(60.0, 14.0)],
            ),
            test_lane(Rect::new(0.0, 28.0, 100.0, 28.0), vec![dot(10.0, 42.0)]),
        ];
        // Rect covers (0,0)-(30,50): lane 0's first dot and lane 1's dot, NOT
        // lane 0's second dot at x=60.
        let rect = Rect::new(0.0, 0.0, 30.0, 50.0);
        let mut hits = dots_in_rect(rect, &lanes);
        hits.sort();
        assert_eq!(hits, vec![(0, 0), (1, 0)]);
    }

    #[test]
    fn dots_in_rect_empty_when_nothing_inside() {
        let lanes = vec![test_lane(Rect::new(0.0, 0.0, 100.0, 28.0), vec![dot(90.0, 14.0)])];
        let rect = Rect::new(0.0, 0.0, 10.0, 10.0);
        assert!(dots_in_rect(rect, &lanes).is_empty());
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
        // One strip entry + one entry per dot.
        assert_eq!(out.len(), 3);
        assert_eq!(out[0].kind, "automation_strip");
        assert_eq!(out[0].payload, "effect:fx|amount");
        assert_eq!(out[1].kind, "automation_point");
        assert_eq!(out[1].payload, "effect:fx|amount|0");
        assert_eq!(out[2].payload, "effect:fx|amount|1");
    }
}
