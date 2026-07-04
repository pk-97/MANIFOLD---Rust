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

use crate::node::Vec2;
use crate::panels::viewport::AutomationLaneScreen;

/// Grab radius for an existing breakpoint dot, in screen pixels. A click
/// within this radius of a dot's center grabs/selects/deletes that dot
/// instead of adding a new point at the click location.
pub const DOT_HIT_RADIUS_PX: f32 = 7.0;

/// Result of an automation hit-test.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutomationHit {
    /// An existing breakpoint. Indices are into the `lanes` slice the test
    /// was run against and that lane's `dots` — the caller re-indexes rather
    /// than this type owning a copy, mirroring `ClipHitResult`'s "identity,
    /// not a snapshot" shape.
    Dot { lane_index: usize, dot_index: usize },
    /// Empty strip area — clicking here adds a new breakpoint at `pos`.
    Strip { lane_index: usize },
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
        return Some(match nearest {
            Some((dot_index, _)) => AutomationHit::Dot { lane_index, dot_index },
            None => AutomationHit::Strip { lane_index },
        });
    }
    None
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
}
