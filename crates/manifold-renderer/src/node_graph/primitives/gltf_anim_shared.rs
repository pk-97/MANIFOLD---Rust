//! Shared clock/sampling logic for the three glTF CPU animation samplers
//! (`gltf_animation_source`, `gltf_skeleton_pose`, `gltf_morph_weights`).
//!
//! GLTF_ANIMATION_DESIGN.md A4: A1–A3 each duplicated a tiny one-line
//! `default_progress` formula "rather than shared ... no other coupling".
//! A4's new logic — loop-mode wrapping, retrigger-origin capture, and
//! compound-key row-range scanning for clip selection — is neither tiny nor
//! uncoupled: all three primitives sample the SAME imported clip and must
//! wrap/retrigger byte-identically for a coherent performance surface (a
//! rigid+morph combo, or a skinned character's pose). Centralizing only the
//! NEW logic here (the old `default_progress` duplication stands, still
//! independently gate-tested per primitive) avoids tripling a bug surface
//! that has real correctness coupling.

use crate::node_graph::effect_node::FrameTime;
use crate::node_graph::gltf_load::GltfInterp;
use crate::node_graph::parameters::TableData;

/// How `progress` maps into `[0, 1]` past the wrap point. Applies uniformly
/// to wired and default-beat-drive progress (D3: "whichever the source, the
/// sampler always wraps").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LoopMode {
    /// `rem_euclid(1.0)` — the existing A1–A3 behaviour, continuous looping.
    Loop,
    /// `clamp(0.0, 1.0)` — holds at the clip's last frame past `progress > 1`,
    /// holds at the first frame below `progress < 0`.
    Once,
    /// Triangle-fold `rem_euclid(2.0)` — plays forward then reverses,
    /// looping continuously with no snap-back discontinuity.
    PingPong,
}

impl LoopMode {
    pub(crate) fn from_enum_index(idx: f32) -> Self {
        match idx.round() as i64 {
            1 => LoopMode::Once,
            2 => LoopMode::PingPong,
            _ => LoopMode::Loop,
        }
    }

    /// Wrap raw (unbounded) progress into `[0, 1]` per this mode.
    fn apply(self, raw: f32) -> f32 {
        match self {
            LoopMode::Loop => raw.rem_euclid(1.0),
            LoopMode::Once => raw.clamp(0.0, 1.0),
            LoopMode::PingPong => {
                let folded = raw.rem_euclid(2.0);
                if folded <= 1.0 { folded } else { 2.0 - folded }
            }
        }
    }
}

pub(crate) const LOOP_MODES: &[&str] = &["Loop", "Once", "PingPong"];

/// Edge-detects a `trigger_count` input and captures the beat at which it
/// last fired — the "clip restarts from 0" origin for the default beat-drive
/// path. Same first-frame-establishes-baseline shape as
/// `scalar_array_accumulator`/`clip_trigger_index`: loading a preset with a
/// nonzero trigger_count must not immediately consume a trigger.
#[derive(Clone, Copy, Debug, Default)]
pub struct TriggerLatch {
    last_trigger_count: Option<u32>,
    origin_beats: f64,
    has_fired: bool,
}

impl TriggerLatch {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Feed this frame's raw `trigger_count` (`None` = input unwired — no
    /// retrigger source, latch stays inert) and the current transport beat.
    pub(crate) fn update(&mut self, trigger_count: Option<f32>, beats: f64) {
        let Some(raw) = trigger_count else { return };
        let count = raw.round().max(0.0) as u32;
        match self.last_trigger_count {
            None => self.last_trigger_count = Some(count),
            Some(last) if last != count => {
                self.origin_beats = beats;
                self.has_fired = true;
                self.last_trigger_count = Some(count);
            }
            _ => {}
        }
    }

    /// The last trigger's beat origin, or `None` if it has never fired
    /// (default beat-drive uses beat 0 as its origin in that case — today's
    /// A1–A3 behaviour, unchanged).
    pub(crate) fn origin_beats(&self) -> Option<f64> {
        self.has_fired.then_some(self.origin_beats)
    }

    pub(crate) fn clear(&mut self) {
        *self = Self::new();
    }
}

/// Resolve normalized progress in `[0, 1]` for one frame. `wired` is the
/// port-shadowed `progress` input if connected; `trigger_origin_beats` is
/// [`TriggerLatch::origin_beats`] (only consulted on the default/unwired
/// path — a wired progress source is already fully performer-controlled,
/// D1).
pub(crate) fn resolve_progress(
    time: FrameTime,
    wired: Option<f32>,
    duration_s: f32,
    rate: f32,
    loop_mode: LoopMode,
    trigger_origin_beats: Option<f64>,
) -> f32 {
    let raw = match wired {
        Some(p) => p,
        None => {
            let beats = time.beats.0 - trigger_origin_beats.unwrap_or(0.0);
            let seconds = time.seconds.0;
            let beats_per_second = if seconds.abs() > 1e-6 { time.beats.0 / seconds } else { 2.0 };
            let clip_beats = (duration_s as f64 * beats_per_second).max(1e-6);
            (beats * rate as f64 / clip_beats) as f32
        }
    };
    loop_mode.apply(raw)
}

/// Duration in seconds for `clip_index`, looked up in a `[clip_index,
/// duration_s]` Table; falls back to `fallback` when the table is absent or
/// carries no row for that clip (keeps pre-A4 single-clip presets/tests,
/// which never set `clip_durations`, working unchanged).
pub(crate) fn clip_duration(table: Option<&TableData>, clip_index: usize, fallback: f32) -> f32 {
    let Some(table) = table else { return fallback };
    for i in 0..table.row_count() {
        let Some(row) = table.row(i) else { continue };
        if row.len() >= 2 && row[0].round() as i64 == clip_index as i64 {
            return row[1].max(1e-6);
        }
    }
    fallback
}

/// glTF spec Appendix C cubic Hermite spline, generalized over `N` scalar
/// components (3 for vec3, 4 for quat): `p0`/`p1` are the bracketing
/// keyframes' values, `m0` is `p0`'s OUT-tangent, `m1` is `p1`'s IN-tangent,
/// `t` the normalized `[0,1]` fraction between them, `td` the real keyframe
/// time delta (tangents are scaled by the interval, not by `t`).
fn hermite<const N: usize>(p0: [f32; N], m0: [f32; N], p1: [f32; N], m1: [f32; N], t: f32, td: f32) -> [f32; N] {
    let t2 = t * t;
    let t3 = t2 * t;
    let h00 = 2.0 * t3 - 3.0 * t2 + 1.0;
    let h10 = t3 - 2.0 * t2 + t;
    let h01 = -2.0 * t3 + 3.0 * t2;
    let h11 = t3 - t2;
    std::array::from_fn(|i| h00 * p0[i] + h10 * td * m0[i] + h01 * p1[i] + h11 * td * m1[i])
}

// ─── GLTF_ANIM_RUNTIME_V2_DESIGN.md D3/P2 — slice-based samplers ──────────
// The former table-row samplers (`sample_vec3_range`/`sample_quat_range`/
// `sample_scalar_range`, `row_range_for_key`/`row_range_for_compound_key`,
// the `Interp` enum) are DELETED — every one of the three glTF CPU
// samplers now reads exclusively from `gltf_anim_cache::Channel`'s flat
// slices. `values`/`in_tangents`/`out_tangents` are flat SoA:
// `values[i*stride..i*stride+stride]` is keyframe `i`.

/// Binary-search `times` for the bracketing keyframe pair around `t`,
/// returning `(lo, hi)` indices (`lo == hi` only when `times.len() == 1`).
/// Callers must have already handled `times.is_empty()` and the
/// before-first/after-last boundary holds.
fn bracket_slice(times: &[f32], t: f32) -> (usize, usize) {
    let n = times.len();
    if n <= 1 {
        return (0, 0);
    }
    // First index whose time is > t; the bracketing pair is (hi-1, hi).
    let hi = times.partition_point(|&tt| tt <= t).clamp(1, n - 1);
    (hi - 1, hi)
}

/// Slice equivalent of [`sample_vec3_range`] — same LINEAR/STEP/CUBICSPLINE
/// behavior, `values`/tangents stride-3 flat SoA instead of table columns.
pub(crate) fn sample_vec3_slice(
    times: &[f32],
    values: &[f32],
    mode: GltfInterp,
    in_tangents: &[f32],
    out_tangents: &[f32],
    t: f32,
    default: [f32; 3],
) -> [f32; 3] {
    let n = times.len();
    if n == 0 {
        return default;
    }
    let get = |i: usize| -> [f32; 3] { [values[i * 3], values[i * 3 + 1], values[i * 3 + 2]] };
    if n == 1 {
        return get(0);
    }
    if t <= times[0] {
        return get(0);
    }
    if t >= times[n - 1] {
        return get(n - 1);
    }
    let (lo, hi) = bracket_slice(times, t);
    let (t0, t1) = (times[lo], times[hi]);
    let f = if (t1 - t0).abs() > 1e-9 { (t - t0) / (t1 - t0) } else { 0.0 };
    match mode {
        GltfInterp::Step => get(lo),
        GltfInterp::CubicSpline => {
            let p0 = get(lo);
            let p1 = get(hi);
            let m0 = [out_tangents[lo * 3], out_tangents[lo * 3 + 1], out_tangents[lo * 3 + 2]];
            let m1 = [in_tangents[hi * 3], in_tangents[hi * 3 + 1], in_tangents[hi * 3 + 2]];
            hermite(p0, m0, p1, m1, f, t1 - t0)
        }
        GltfInterp::Linear => {
            let (a, b) = (get(lo), get(hi));
            [a[0] + (b[0] - a[0]) * f, a[1] + (b[1] - a[1]) * f, a[2] + (b[2] - a[2]) * f]
        }
    }
}

/// Slice equivalent of [`sample_quat_range`] — same LINEAR (slerp) /STEP/
/// CUBICSPLINE (Hermite, renormalized) behavior, stride-4 flat SoA.
pub(crate) fn sample_quat_slice(
    times: &[f32],
    values: &[f32],
    mode: GltfInterp,
    in_tangents: &[f32],
    out_tangents: &[f32],
    t: f32,
) -> [f32; 4] {
    let default = [0.0, 0.0, 0.0, 1.0];
    let n = times.len();
    if n == 0 {
        return default;
    }
    let get = |i: usize| -> [f32; 4] {
        [values[i * 4], values[i * 4 + 1], values[i * 4 + 2], values[i * 4 + 3]]
    };
    if n == 1 {
        return get(0);
    }
    if t <= times[0] {
        return get(0);
    }
    if t >= times[n - 1] {
        return get(n - 1);
    }
    let (lo, hi) = bracket_slice(times, t);
    let (t0, t1) = (times[lo], times[hi]);
    let f = if (t1 - t0).abs() > 1e-9 { (t - t0) / (t1 - t0) } else { 0.0 };
    match mode {
        GltfInterp::Step => get(lo),
        GltfInterp::CubicSpline => {
            let p0 = get(lo);
            let p1 = get(hi);
            let m0 = [
                out_tangents[lo * 4],
                out_tangents[lo * 4 + 1],
                out_tangents[lo * 4 + 2],
                out_tangents[lo * 4 + 3],
            ];
            let m1 = [
                in_tangents[hi * 4],
                in_tangents[hi * 4 + 1],
                in_tangents[hi * 4 + 2],
                in_tangents[hi * 4 + 3],
            ];
            normalize_quat(hermite(p0, m0, p1, m1, f, t1 - t0))
        }
        GltfInterp::Linear => slerp(get(lo), get(hi), f),
    }
}

/// Extract morph target `target_index`'s scalar weight from a `Weights`
/// channel's flat SoA (`values[i*target_count + target_index]` is keyframe
/// `i`'s value for that one target) at time `t`. LINEAR-only
/// (GLTF_ANIMATION_DESIGN.md A3, unchanged by this design — glTF's own
/// spec allows STEP/CUBICSPLINE on weights channels, but this codebase's
/// importer never emitted anything but LINEAR for them, and
/// `Channel::from_weights` hard-codes `mode: GltfInterp::Linear`).
/// `target_index >= target_count` returns `default` (the caller's own
/// bounds are the authority; this is a defensive floor, not the primary
/// guard).
pub(crate) fn sample_weight_slice(
    times: &[f32],
    values: &[f32],
    target_count: usize,
    target_index: usize,
    t: f32,
    default: f32,
) -> f32 {
    if target_count == 0 || target_index >= target_count {
        return default;
    }
    let n = times.len();
    if n == 0 {
        return default;
    }
    let get = |i: usize| -> f32 { values[i * target_count + target_index] };
    if n == 1 {
        return get(0);
    }
    if t <= times[0] {
        return get(0);
    }
    if t >= times[n - 1] {
        return get(n - 1);
    }
    let (lo, hi) = bracket_slice(times, t);
    let (t0, t1) = (times[lo], times[hi]);
    let f = if (t1 - t0).abs() > 1e-9 { (t - t0) / (t1 - t0) } else { 0.0 };
    let (a, b) = (get(lo), get(hi));
    a + (b - a) * f
}

fn slerp(a: [f32; 4], b: [f32; 4], t: f32) -> [f32; 4] {
    let dot0 = a[0] * b[0] + a[1] * b[1] + a[2] * b[2] + a[3] * b[3];
    let (b, dot) = if dot0 < 0.0 { ([-b[0], -b[1], -b[2], -b[3]], -dot0) } else { (b, dot0) };
    if dot > 0.9995 {
        let lerped = [
            a[0] + (b[0] - a[0]) * t,
            a[1] + (b[1] - a[1]) * t,
            a[2] + (b[2] - a[2]) * t,
            a[3] + (b[3] - a[3]) * t,
        ];
        return normalize_quat(lerped);
    }
    let theta_0 = dot.clamp(-1.0, 1.0).acos();
    let theta = theta_0 * t;
    let sin_theta_0 = theta_0.sin();
    let s0 = (theta_0 - theta).sin() / sin_theta_0;
    let s1 = theta.sin() / sin_theta_0;
    [
        a[0] * s0 + b[0] * s1,
        a[1] * s0 + b[1] * s1,
        a[2] * s0 + b[2] * s1,
        a[3] * s0 + b[3] * s1,
    ]
}

fn normalize_quat(q: [f32; 4]) -> [f32; 4] {
    let len = (q[0] * q[0] + q[1] * q[1] + q[2] * q[2] + q[3] * q[3]).sqrt();
    if len < 1e-12 { [0.0, 0.0, 0.0, 1.0] } else { [q[0] / len, q[1] / len, q[2] / len, q[3] / len] }
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_core::{Beats, Seconds};

    fn frame_time(beats: f32, seconds: f32) -> FrameTime {
        FrameTime {
            beats: Beats(beats as f64),
            seconds: Seconds(seconds as f64),
            delta: Seconds(1.0 / 60.0),
            frame_count: 0,
        }
    }

    #[test]
    fn loop_mode_wraps_continuously() {
        assert!((LoopMode::Loop.apply(1.5) - 0.5).abs() < 1e-6);
        assert!((LoopMode::Loop.apply(-0.25) - 0.75).abs() < 1e-6);
    }

    #[test]
    fn once_mode_holds_past_the_end_and_start() {
        assert_eq!(LoopMode::Once.apply(1.5), 1.0);
        assert_eq!(LoopMode::Once.apply(-0.5), 0.0);
        assert!((LoopMode::Once.apply(0.5) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn pingpong_mode_reflects_past_one_cycle() {
        assert!((LoopMode::PingPong.apply(0.25) - 0.25).abs() < 1e-6);
        assert!((LoopMode::PingPong.apply(1.25) - 0.75).abs() < 1e-6, "reflects past 1.0");
        assert!((LoopMode::PingPong.apply(2.25) - 0.25).abs() < 1e-6, "wraps into the next cycle");
    }

    #[test]
    fn trigger_latch_first_frame_establishes_baseline_without_firing() {
        let mut latch = TriggerLatch::new();
        latch.update(Some(3.0), 10.0);
        assert_eq!(latch.origin_beats(), None, "first frame must not consume a trigger");
    }

    #[test]
    fn trigger_latch_fires_on_count_edge_and_captures_origin_beat() {
        let mut latch = TriggerLatch::new();
        latch.update(Some(0.0), 0.0);
        latch.update(Some(0.0), 4.0); // same count, no fire
        assert_eq!(latch.origin_beats(), None);
        latch.update(Some(1.0), 8.0); // edge
        assert_eq!(latch.origin_beats(), Some(8.0));
        latch.update(Some(1.0), 12.0); // idempotent
        assert_eq!(latch.origin_beats(), Some(8.0));
    }

    #[test]
    fn trigger_latch_clear_releases_the_fired_state() {
        let mut latch = TriggerLatch::new();
        latch.update(Some(0.0), 0.0);
        latch.update(Some(1.0), 5.0);
        assert_eq!(latch.origin_beats(), Some(5.0));
        latch.clear();
        assert_eq!(latch.origin_beats(), None);
    }

    #[test]
    fn resolve_progress_retriggers_from_zero_on_the_default_path() {
        // duration=1, rate=1, 120bpm-equivalent fallback ratio (beats=2*seconds).
        let p_before = resolve_progress(frame_time(4.0, 2.0), None, 1.0, 1.0, LoopMode::Loop, None);
        assert!((p_before - 0.0).abs() < 1e-3, "beats=4, clip_beats=2 -> wraps to 0");
        // Retrigger at beats=4: elapsed-since-origin is 0 -> progress 0,
        // regardless of absolute beats.
        let p_after = resolve_progress(frame_time(4.0, 2.0), None, 1.0, 1.0, LoopMode::Loop, Some(4.0));
        assert!((p_after - 0.0).abs() < 1e-3);
        let p_one_beat_later =
            resolve_progress(frame_time(5.0, 2.5), None, 1.0, 1.0, LoopMode::Loop, Some(4.0));
        assert!((p_one_beat_later - 0.5).abs() < 1e-3, "1 beat elapsed of a 2-beat clip -> progress 0.5");
    }

    #[test]
    fn resolve_progress_ignores_trigger_origin_when_wired() {
        let p = resolve_progress(frame_time(100.0, 50.0), Some(0.5), 1.0, 1.0, LoopMode::Loop, Some(90.0));
        assert!((p - 0.5).abs() < 1e-6, "wired progress is authoritative regardless of trigger state");
    }

    #[test]
    fn clip_duration_falls_back_when_table_absent_or_missing_the_clip() {
        assert_eq!(clip_duration(None, 0, 2.5), 2.5);
        let table = TableData::new(vec![vec![0.0, 1.0], vec![1.0, 3.0]]).unwrap();
        assert_eq!(clip_duration(Some(&table), 1, 2.5), 3.0);
        assert_eq!(clip_duration(Some(&table), 5, 2.5), 2.5, "no row for clip 5 -> fallback");
    }

    // ─── GLTF_ANIM_RUNTIME_V2_DESIGN.md P1 — slice samplers ────────────────
    // Same fixtures as the table-based tests above, re-expressed as flat
    // slices — proves the slice path is value-identical to the table path
    // it's replacing (D3).

    #[test]
    fn sample_vec3_slice_lerps_and_holds_boundaries() {
        let times = [0.0f32, 1.0];
        let values = [0.0f32, 0.0, 0.0, 10.0, 0.0, 0.0];
        let mid = sample_vec3_slice(&times, &values, GltfInterp::Linear, &[], &[], 0.5, [0.0; 3]);
        assert!((mid[0] - 5.0).abs() < 1e-4, "halfway lerp, got {}", mid[0]);
        let before = sample_vec3_slice(&times, &values, GltfInterp::Linear, &[], &[], -1.0, [0.0; 3]);
        assert!((before[0] - 0.0).abs() < 1e-4, "holds first keyframe before range");
        let after = sample_vec3_slice(&times, &values, GltfInterp::Linear, &[], &[], 5.0, [0.0; 3]);
        assert!((after[0] - 10.0).abs() < 1e-4, "holds last keyframe after range");
    }

    #[test]
    fn sample_vec3_slice_falls_back_to_default_when_empty() {
        let out = sample_vec3_slice(&[], &[], GltfInterp::Linear, &[], &[], 0.5, [1.0, 2.0, 3.0]);
        assert_eq!(out, [1.0, 2.0, 3.0]);
    }

    #[test]
    fn slice_step_holds_the_lower_keyframe_value() {
        let times = [0.0f32, 0.5, 1.0, 1.5];
        let values = [0.0f32, 0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 1.0, 0.0, 0.0, 0.0, 0.0];
        let at_025 = sample_vec3_slice(&times, &values, GltfInterp::Step, &[], &[], 0.25, [9.0; 3]);
        assert!(
            (at_025[0] - 0.0).abs() < 1e-4 && (at_025[1] - 0.0).abs() < 1e-4,
            "STEP at t=0.25 must hold keyframe0's (0,0,0), got {at_025:?}"
        );
        let at_06 = sample_vec3_slice(&times, &values, GltfInterp::Step, &[], &[], 0.6, [9.0; 3]);
        assert!(
            (at_06[0] - 1.0).abs() < 1e-4 && (at_06[1] - 0.0).abs() < 1e-4,
            "STEP at t=0.6 must hold keyframe1's (1,0,0), got {at_06:?}"
        );
    }

    #[test]
    fn slice_cubicspline_with_zero_tangents_hermite_blends_the_bracketing_values() {
        let times = [0.0f32, 0.5, 1.0, 1.5];
        let values = [0.0f32, 0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 1.0, 0.0, 0.0, 0.0, 0.0];
        let zero_tangents = [0.0f32; 12];
        let at_025 = sample_vec3_slice(
            &times,
            &values,
            GltfInterp::CubicSpline,
            &zero_tangents,
            &zero_tangents,
            0.25,
            [9.0; 3],
        );
        assert!(
            (at_025[0] - 0.5).abs() < 1e-4 && (at_025[1] - 0.0).abs() < 1e-4,
            "CUBICSPLINE (zero tangents) at t=0.25 must equal the smoothstep blend (0.5,0,0), \
             got {at_025:?}"
        );
    }

    #[test]
    fn slice_quat_single_keyframe_is_the_static_bind_pose() {
        let times = [0.0f32];
        let values = [0.1f32, 0.2, 0.3, 0.9];
        let q = sample_quat_slice(&times, &values, GltfInterp::Linear, &[], &[], 0.7);
        assert_eq!(q, [0.1, 0.2, 0.3, 0.9], "single-keyframe slice returns the static value at any t");
    }

    #[test]
    fn slice_cubicspline_quat_hermite_result_is_renormalized() {
        let times = [0.0f32, 1.0];
        let values = [
            0.0f32,
            0.0,
            0.0,
            1.0,
            0.0,
            0.0,
            std::f32::consts::FRAC_1_SQRT_2,
            std::f32::consts::FRAC_1_SQRT_2,
        ];
        let zero_tangents = [0.0f32; 8];
        let q = sample_quat_slice(
            &times,
            &values,
            GltfInterp::CubicSpline,
            &zero_tangents,
            &zero_tangents,
            0.5,
        );
        let len_sq = q[0] * q[0] + q[1] * q[1] + q[2] * q[2] + q[3] * q[3];
        assert!((len_sq - 1.0).abs() < 1e-4, "Hermite quat blend must be renormalized, got {q:?}");
    }

    // ─── GLTF_ANIM_RUNTIME_V2_DESIGN.md P2 — weight-channel slice sampler ──

    #[test]
    fn sample_weight_slice_lerps_one_target_out_of_a_multi_target_channel() {
        // 2 targets, 2 keyframes: target 0 goes 0.0 -> 1.0, target 1 goes
        // 0.0 -> 0.2 — proves the stride-2 extraction picks the RIGHT
        // target's column, not just the right row.
        let times = [0.0f32, 1.0];
        let values = [0.0f32, 0.0, 1.0, 0.2];
        let t0_mid = sample_weight_slice(&times, &values, 2, 0, 0.5, -1.0);
        let t1_mid = sample_weight_slice(&times, &values, 2, 1, 0.5, -1.0);
        assert!((t0_mid - 0.5).abs() < 1e-4, "target 0 halfway: got {t0_mid}");
        assert!((t1_mid - 0.1).abs() < 1e-4, "target 1 halfway: got {t1_mid}");
    }

    #[test]
    fn sample_weight_slice_holds_boundaries_and_single_row() {
        let times = [0.0f32];
        let values = [0.5f32];
        let held = sample_weight_slice(&times, &values, 1, 0, 0.73, -1.0);
        assert_eq!(held, 0.5, "single-row channel holds its static value at any t");
    }

    #[test]
    fn sample_weight_slice_falls_back_to_default_out_of_bounds() {
        assert_eq!(sample_weight_slice(&[], &[], 0, 0, 0.5, 0.25), 0.25, "empty channel");
        let times = [0.0f32, 1.0];
        let values = [0.0f32, 1.0];
        assert_eq!(
            sample_weight_slice(&times, &values, 1, 5, 0.5, 0.25),
            0.25,
            "target_index past target_count"
        );
    }
}
