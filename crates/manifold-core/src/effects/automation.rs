//! Timeline automation lanes (`AutomationLane`, `AutomationPoint`,
//! `SegmentShape`) plus the shared per-instance automation prune/take
//! helpers and `RemovedAutomation`. Extracted from effects.rs (P2-E, D4).

use serde::{Deserialize, Serialize};
use crate::units::Beats;
use super::ParamId;
use super::{ParamEnvelope, ParameterDriver};

// ─── Automation lanes ───
//
// Timeline arrangement automation — a tier-1 "hand" sampled from the
// arrangement each tick (`manifold-playback::automation`), riding on top of
// the same base/value slot every other hand writes through. See
// `docs/AUTOMATION_LANES_DESIGN.md`.

/// Per-param timeline automation, keyed by `param_id` — the exact pattern of
/// the sibling per-param automation rows (`drivers` / `envelopes` /
/// `audio_mods` / `ableton_mappings`) that already live on `PresetInstance`.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomationLane {
    pub param_id: ParamId,
    /// Lane on/off (Ableton: a deactivated automation lane). A disabled lane
    /// neither samples nor participates in touch/latch bookkeeping.
    pub enabled: bool,
    /// Sorted ascending by `beat` — the write-time invariant P2's editing
    /// commands enforce (mirrors `TempoMap::ensure_sorted`). [`Self::value_at`]
    /// assumes this and does not re-sort.
    pub points: Vec<AutomationPoint>,
}

/// One breakpoint on an [`AutomationLane`]. `value` is stored in param-range
/// units (not normalized) — a lane's points are only ever resolved against
/// [`resolve_param_in`]'s min/max for clamping, at write time (P2) and again
/// at sample time (defensive against a range narrowed after the point was
/// authored).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomationPoint {
    /// Arrangement beat, absolute (not clip-relative). Automation lanes are
    /// beat-indexed, so they stretch with tempo automatically.
    pub beat: Beats,
    pub value: f32,
    /// Shape of the segment LEAVING this point, toward the next one.
    pub shape: SegmentShape,
}

/// The interpolation shape of one automation segment.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "camelCase")]
pub enum SegmentShape {
    Linear,
    /// Step — holds the earlier point's value for the whole segment.
    /// Required for enum/int-backed params (the sampler doesn't round; the
    /// existing param write path handles that exactly as slider writes do —
    /// authoring with `Hold` is what keeps an enum param from reading a
    /// nonsense mid-interpolation value).
    Hold,
    /// Power-curve bend, Ableton-style segment drag. `-1..1`: negative bends
    /// concave (slow start), positive bends convex (fast start), `0` is
    /// linear. Values outside `-1..1` are clamped at evaluation time.
    Curved(f32),
}

impl AutomationLane {
    /// Sample the curve at `beat`, in param-range units. Pure,
    /// allocation-free: binary-search the segment containing `beat`.
    ///
    /// - Empty lane → `0.0` (never sampled in practice — the evaluator skips
    ///   empty lanes before calling this).
    /// - Before the first point → the first point's value (Ableton
    ///   behavior: no backward extrapolation).
    /// - After the last point → the last point's value.
    /// - Between two points → the earlier point's [`SegmentShape`] decides:
    ///   `Linear` interpolates, `Hold` steps, `Curved(bend)` applies the
    ///   power-curve bend to the interpolation parameter before lerping.
    pub fn value_at(&self, beat: Beats) -> f32 {
        match self.points.as_slice() {
            [] => 0.0,
            [only] => only.value,
            points => {
                let first = &points[0];
                if beat.0 <= first.beat.0 {
                    return first.value;
                }
                let last = &points[points.len() - 1];
                if beat.0 >= last.beat.0 {
                    return last.value;
                }
                // `partial_cmp` is safe here: both operands come from
                // `Beats(f64)` values that reached the arrangement (never
                // NaN in practice), and a NaN comparison degrading to
                // `Equal` only widens the binary search, never panics.
                let idx = match points
                    .binary_search_by(|p| p.beat.0.partial_cmp(&beat.0).unwrap_or(std::cmp::Ordering::Equal))
                {
                    Ok(i) => i,
                    // `i > 0` is guaranteed: the `beat <= first.beat` check
                    // above already returned for any beat at or before index 0.
                    Err(i) => i - 1,
                };
                let a = &points[idx];
                let b = &points[idx + 1];
                let span = (b.beat.0 - a.beat.0) as f32;
                if span <= 0.0 {
                    return a.value;
                }
                let t = ((beat.0 - a.beat.0) as f32 / span).clamp(0.0, 1.0);
                match a.shape {
                    SegmentShape::Hold => a.value,
                    SegmentShape::Linear => a.value + (b.value - a.value) * t,
                    SegmentShape::Curved(bend) => {
                        let shaped = segment_bend(t, bend);
                        a.value + (b.value - a.value) * shaped
                    }
                }
            }
        }
    }
}

/// Power-curve bend for a `Curved` segment's interpolation parameter `t`
/// (already `[0, 1]`). `bend` in `-1..1`: `0` is identity (linear); positive
/// bends convex (`t^exponent`, exponent > 1, slow start / fast finish);
/// negative bends concave (exponent < 1, fast start / slow finish) — the
/// standard symmetric power-curve shape, Ableton's segment-drag feel.
/// Endpoints are exact regardless of bend: `f(0) = 0`, `f(1) = 1`.
fn segment_bend(t: f32, bend: f32) -> f32 {
    let bend = bend.clamp(-1.0, 1.0);
    if bend == 0.0 {
        return t;
    }
    let exponent = if bend > 0.0 {
        1.0 + bend * 3.0 // 1..4
    } else {
        1.0 / (1.0 - bend * 3.0) // 1..0.25
    };
    t.powf(exponent)
}

/// Drop every element of an optional automation list whose key is in `ids`,
/// collapsing the list to `None` when it empties. Shared by the four
/// per-instance automation homes (drivers / Ableton mappings / envelopes /
/// audio mods).
pub(super) fn prune_automation_by_ids<T>(
    opt: &mut Option<Vec<T>>,
    ids: &std::collections::HashSet<&str>,
    key: impl Fn(&T) -> &str,
) {
    if let Some(v) = opt.as_mut() {
        v.retain(|t| !ids.contains(key(t)));
        if v.is_empty() {
            *opt = None;
        }
    }
}

/// Like [`prune_automation_by_ids`] but *captures* the removed rows (for undo)
/// instead of dropping them, and matches against an owned id set.
pub(super) fn take_automation_by_ids<T>(
    opt: &mut Option<Vec<T>>,
    ids: &std::collections::HashSet<String>,
    key: impl Fn(&T) -> &str,
) -> Vec<T> {
    let mut taken = Vec::new();
    if let Some(v) = opt.as_mut() {
        let mut i = 0;
        while i < v.len() {
            if ids.contains(key(&v[i])) {
                taken.push(v.remove(i));
            } else {
                i += 1;
            }
        }
        if v.is_empty() {
            *opt = None;
        }
    }
    taken
}

/// Automation rows (drivers / Ableton mappings / envelopes / automation
/// lanes) removed because their `param_id` no longer resolved to a live
/// param. Returned by [`PresetInstance::prune_orphaned_automation`] and
/// restored by [`PresetInstance::restore_automation`] on undo.
#[derive(Debug, Clone, Default)]
pub struct RemovedAutomation {
    pub(super) drivers: Vec<ParameterDriver>,
    pub(super) ableton_mappings: Vec<crate::ableton_mapping::AbletonParamMapping>,
    pub(super) envelopes: Vec<ParamEnvelope>,
    pub(super) audio_mods: Vec<crate::audio_mod::ParameterAudioMod>,
    pub(super) automation_lanes: Vec<AutomationLane>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::test_support::*;
    use crate::units::Beats;

    #[test]
    fn automation_lane_empty_returns_zero() {
        let lane = AutomationLane {
            param_id: ParamId::from("amount"),
            enabled: true,
            points: Vec::new(),
        };
        assert_eq!(lane.value_at(Beats(4.0)), 0.0);
    }

    #[test]
    fn automation_lane_single_point_holds_everywhere() {
        let lane = AutomationLane {
            param_id: ParamId::from("amount"),
            enabled: true,
            points: vec![pt(4.0, 0.7, SegmentShape::Linear)],
        };
        assert_eq!(lane.value_at(Beats(-10.0)), 0.7);
        assert_eq!(lane.value_at(Beats(4.0)), 0.7);
        assert_eq!(lane.value_at(Beats(100.0)), 0.7);
    }

    #[test]
    fn automation_lane_before_first_point_holds_first_value() {
        // Ableton behavior: no backward extrapolation.
        let lane = AutomationLane {
            param_id: ParamId::from("amount"),
            enabled: true,
            points: vec![
                pt(4.0, 0.2, SegmentShape::Linear),
                pt(8.0, 0.8, SegmentShape::Linear),
            ],
        };
        assert_eq!(lane.value_at(Beats(0.0)), 0.2);
        assert_eq!(lane.value_at(Beats(4.0)), 0.2);
    }

    #[test]
    fn automation_lane_after_last_point_holds_last_value() {
        let lane = AutomationLane {
            param_id: ParamId::from("amount"),
            enabled: true,
            points: vec![
                pt(4.0, 0.2, SegmentShape::Linear),
                pt(8.0, 0.8, SegmentShape::Linear),
            ],
        };
        assert_eq!(lane.value_at(Beats(8.0)), 0.8);
        assert_eq!(lane.value_at(Beats(1000.0)), 0.8);
    }

    #[test]
    fn automation_lane_linear_segment_interpolates() {
        let lane = AutomationLane {
            param_id: ParamId::from("amount"),
            enabled: true,
            points: vec![
                pt(0.0, 0.0, SegmentShape::Linear),
                pt(4.0, 1.0, SegmentShape::Linear),
            ],
        };
        assert!((lane.value_at(Beats(2.0)) - 0.5).abs() < 1e-6);
        assert!((lane.value_at(Beats(1.0)) - 0.25).abs() < 1e-6);
    }

    #[test]
    fn automation_lane_hold_segment_steps() {
        // `Hold` on the earlier point: the segment holds that point's value
        // for its whole span, then jumps at the next point — required for
        // enum/int-backed params.
        let lane = AutomationLane {
            param_id: ParamId::from("amount"),
            enabled: true,
            points: vec![
                pt(0.0, 0.0, SegmentShape::Hold),
                pt(4.0, 1.0, SegmentShape::Hold),
                pt(8.0, 2.0, SegmentShape::Linear),
            ],
        };
        assert_eq!(lane.value_at(Beats(0.0)), 0.0);
        assert_eq!(lane.value_at(Beats(3.9)), 0.0, "holds through the segment");
        assert_eq!(lane.value_at(Beats(4.0)), 1.0, "steps exactly at the next point");
        assert_eq!(lane.value_at(Beats(7.9)), 1.0);
    }

    #[test]
    fn automation_lane_curved_segment_bends_but_keeps_endpoints() {
        let convex = AutomationLane {
            param_id: ParamId::from("amount"),
            enabled: true,
            points: vec![
                pt(0.0, 0.0, SegmentShape::Curved(1.0)),
                pt(4.0, 1.0, SegmentShape::Linear),
            ],
        };
        let concave = AutomationLane {
            param_id: ParamId::from("amount"),
            enabled: true,
            points: vec![
                pt(0.0, 0.0, SegmentShape::Curved(-1.0)),
                pt(4.0, 1.0, SegmentShape::Linear),
            ],
        };
        // Endpoints exact regardless of bend.
        assert_eq!(convex.value_at(Beats(0.0)), 0.0);
        assert_eq!(convex.value_at(Beats(4.0)), 1.0);
        // Midpoint: positive bend (convex) sits BELOW the linear midpoint
        // (slow start); negative bend (concave) sits ABOVE it (fast start).
        let mid_linear = 0.5;
        let mid_convex = convex.value_at(Beats(2.0));
        let mid_concave = concave.value_at(Beats(2.0));
        assert!(mid_convex < mid_linear, "convex bend lags at the midpoint");
        assert!(mid_concave > mid_linear, "concave bend leads at the midpoint");
    }

    #[test]
    fn automation_lane_bend_out_of_range_is_clamped() {
        // `Curved` bends are only meaningful in -1..1; anything past that
        // clamps rather than producing a wild exponent.
        let over = AutomationLane {
            param_id: ParamId::from("amount"),
            enabled: true,
            points: vec![
                pt(0.0, 0.0, SegmentShape::Curved(5.0)),
                pt(4.0, 1.0, SegmentShape::Linear),
            ],
        };
        let clamped = AutomationLane {
            param_id: ParamId::from("amount"),
            enabled: true,
            points: vec![
                pt(0.0, 0.0, SegmentShape::Curved(1.0)),
                pt(4.0, 1.0, SegmentShape::Linear),
            ],
        };
        assert!((over.value_at(Beats(2.0)) - clamped.value_at(Beats(2.0))).abs() < 1e-6);
    }

    #[test]
    fn automation_lane_three_points_binary_search_finds_middle_segment() {
        let lane = AutomationLane {
            param_id: ParamId::from("amount"),
            enabled: true,
            points: vec![
                pt(0.0, 0.0, SegmentShape::Linear),
                pt(4.0, 1.0, SegmentShape::Linear),
                pt(8.0, 0.0, SegmentShape::Linear),
            ],
        };
        assert!((lane.value_at(Beats(6.0)) - 0.5).abs() < 1e-6);
    }

}
