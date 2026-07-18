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

/// Row range `[start, end)` within `table` whose leading column equals
/// `key`, assuming rows are grouped ascending by that column. One linear
/// scan; generalizes `gltf_skeleton_pose`'s original per-joint scan.
pub(crate) fn row_range_for_key(table: Option<&TableData>, key: usize) -> (usize, usize) {
    let Some(table) = table else { return (0, 0) };
    let n = table.row_count();
    let mut start = None;
    let mut end = n;
    for i in 0..n {
        let Some(row) = table.row(i) else { continue };
        if row.is_empty() {
            continue;
        }
        let idx = row[0].round() as i64;
        if idx == key as i64 {
            if start.is_none() {
                start = Some(i);
            }
        } else if start.is_some() {
            end = i;
            break;
        }
    }
    match start {
        Some(s) => (s, end),
        None => (0, 0),
    }
}

/// Row range `[start, end)` where column 0 equals `outer` AND column 1
/// equals `inner`, assuming rows are grouped ascending by `(outer, inner,
/// ...)`. Same shape as [`row_range_for_key`] with one more equality check —
/// A4's clip-selection extension of the per-joint/per-target scan.
pub(crate) fn row_range_for_compound_key(
    table: Option<&TableData>,
    outer: usize,
    inner: usize,
) -> (usize, usize) {
    let Some(table) = table else { return (0, 0) };
    let n = table.row_count();
    let mut start = None;
    let mut end = n;
    for i in 0..n {
        let Some(row) = table.row(i) else { continue };
        if row.len() < 2 {
            continue;
        }
        let matches = row[0].round() as i64 == outer as i64 && row[1].round() as i64 == inner as i64;
        if matches {
            if start.is_none() {
                start = Some(i);
            }
        } else if start.is_some() {
            end = i;
            break;
        }
    }
    match start {
        Some(s) => (s, end),
        None => (0, 0),
    }
}

/// glTF sampler interpolation mode, read from a track table's trailing
/// `mode` column (IMPORT_ANYTHING_WAVE_DESIGN.md W2). The column sits right
/// after the value block (`val_col + N`, `N` = 3 for vec3 / 4 for quat) and
/// is optional: a table built before W2 (or any table that just never
/// widened) has no column there at all, and reads as `Linear` — this is a
/// purely additive format, every pre-W2 table keeps behaving exactly as it
/// always did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Interp {
    Linear,
    Step,
    CubicSpline,
}

impl Interp {
    fn from_row(row: &[f32], mode_col: usize) -> Self {
        match row.get(mode_col).map(|v| v.round() as i64) {
            Some(1) => Interp::Step,
            Some(2) => Interp::CubicSpline,
            _ => Interp::Linear,
        }
    }
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

/// Sample a vec3 track within `range`, time in column `time_col`, xyz
/// starting at `val_col`. LINEAR lerps between bracketing keyframes; STEP
/// holds the lower keyframe's value; CUBICSPLINE runs the glTF Hermite
/// formula against the tangent columns immediately following the mode
/// column (`val_col+4..val_col+7` = in-tangent, `val_col+7..val_col+10` =
/// out-tangent — IMPORT_ANYTHING_WAVE_DESIGN.md W2). Holds boundary values
/// outside the range (the glTF spec's own before-first/after-last rule —
/// independent of this primitive's own progress wrap, which happens one
/// level up).
pub(crate) fn sample_vec3_range(
    table: Option<&TableData>,
    range: (usize, usize),
    time_col: usize,
    val_col: usize,
    t: f32,
    default: [f32; 3],
) -> [f32; 3] {
    let (start, end) = range;
    let Some(table) = table else { return default };
    let n = end.saturating_sub(start);
    if n == 0 {
        return default;
    }
    let row = |i: usize| -> [f32; 3] {
        let r = table.row(start + i).unwrap();
        [r[val_col], r[val_col + 1], r[val_col + 2]]
    };
    let time = |i: usize| -> f32 { table.row(start + i).unwrap()[time_col] };
    if n == 1 {
        return row(0);
    }
    let (first_t, last_t) = (time(0), time(n - 1));
    if t <= first_t {
        return row(0);
    }
    if t >= last_t {
        return row(n - 1);
    }
    let mut lo = 0usize;
    let mut hi = n - 1;
    while lo + 1 < hi {
        let mid = (lo + hi) / 2;
        if time(mid) <= t { lo = mid } else { hi = mid }
    }
    let (t0, t1) = (time(lo), time(hi));
    let f = if (t1 - t0).abs() > 1e-9 { (t - t0) / (t1 - t0) } else { 0.0 };
    let mode_col = val_col + 3;
    let mode = if table.col_count() > mode_col {
        Interp::from_row(table.row(start + lo).unwrap(), mode_col)
    } else {
        Interp::Linear
    };
    match mode {
        Interp::Step => row(lo),
        Interp::CubicSpline => {
            let in_col = mode_col + 1;
            let out_col = in_col + 3;
            let r_lo = table.row(start + lo).unwrap();
            let r_hi = table.row(start + hi).unwrap();
            let p0 = [r_lo[val_col], r_lo[val_col + 1], r_lo[val_col + 2]];
            let p1 = [r_hi[val_col], r_hi[val_col + 1], r_hi[val_col + 2]];
            let m0 = [r_lo[out_col], r_lo[out_col + 1], r_lo[out_col + 2]];
            let m1 = [r_hi[in_col], r_hi[in_col + 1], r_hi[in_col + 2]];
            hermite(p0, m0, p1, m1, f, t1 - t0)
        }
        Interp::Linear => {
            let (a, b) = (row(lo), row(hi));
            [a[0] + (b[0] - a[0]) * f, a[1] + (b[1] - a[1]) * f, a[2] + (b[2] - a[2]) * f]
        }
    }
}

/// Same as [`sample_vec3_range`] for a `[x, y, z, w]` quaternion. LINEAR
/// slerps; STEP holds; CUBICSPLINE runs the Hermite formula over the 4-wide
/// tangent columns (`val_col+5..val_col+9` in, `val_col+9..val_col+13` out)
/// and re-normalizes the result (a Hermite blend of unit quaternions isn't
/// itself unit length).
pub(crate) fn sample_quat_range(
    table: Option<&TableData>,
    range: (usize, usize),
    time_col: usize,
    val_col: usize,
    t: f32,
) -> [f32; 4] {
    let default = [0.0, 0.0, 0.0, 1.0];
    let (start, end) = range;
    let Some(table) = table else { return default };
    let n = end.saturating_sub(start);
    if n == 0 {
        return default;
    }
    let row = |i: usize| -> [f32; 4] {
        let r = table.row(start + i).unwrap();
        [r[val_col], r[val_col + 1], r[val_col + 2], r[val_col + 3]]
    };
    let time = |i: usize| -> f32 { table.row(start + i).unwrap()[time_col] };
    if n == 1 {
        return row(0);
    }
    let (first_t, last_t) = (time(0), time(n - 1));
    if t <= first_t {
        return row(0);
    }
    if t >= last_t {
        return row(n - 1);
    }
    let mut lo = 0usize;
    let mut hi = n - 1;
    while lo + 1 < hi {
        let mid = (lo + hi) / 2;
        if time(mid) <= t { lo = mid } else { hi = mid }
    }
    let (t0, t1) = (time(lo), time(hi));
    let f = if (t1 - t0).abs() > 1e-9 { (t - t0) / (t1 - t0) } else { 0.0 };
    let mode_col = val_col + 4;
    let mode = if table.col_count() > mode_col {
        Interp::from_row(table.row(start + lo).unwrap(), mode_col)
    } else {
        Interp::Linear
    };
    match mode {
        Interp::Step => row(lo),
        Interp::CubicSpline => {
            let in_col = mode_col + 1;
            let out_col = in_col + 4;
            let r_lo = table.row(start + lo).unwrap();
            let r_hi = table.row(start + hi).unwrap();
            let p0 = [r_lo[val_col], r_lo[val_col + 1], r_lo[val_col + 2], r_lo[val_col + 3]];
            let p1 = [r_hi[val_col], r_hi[val_col + 1], r_hi[val_col + 2], r_hi[val_col + 3]];
            let m0 = [r_lo[out_col], r_lo[out_col + 1], r_lo[out_col + 2], r_lo[out_col + 3]];
            let m1 = [r_hi[in_col], r_hi[in_col + 1], r_hi[in_col + 2], r_hi[in_col + 3]];
            normalize_quat(hermite(p0, m0, p1, m1, f, t1 - t0))
        }
        Interp::Linear => slerp(row(lo), row(hi), f),
    }
}

/// Sample a scalar track within `range`, time in column `time_col`, value in
/// column `val_col`.
pub(crate) fn sample_scalar_range(
    table: Option<&TableData>,
    range: (usize, usize),
    time_col: usize,
    val_col: usize,
    t: f32,
    default: f32,
) -> f32 {
    let (start, end) = range;
    let Some(table) = table else { return default };
    let n = end.saturating_sub(start);
    if n == 0 {
        return default;
    }
    let value = |i: usize| -> f32 { table.row(start + i).unwrap()[val_col] };
    let time = |i: usize| -> f32 { table.row(start + i).unwrap()[time_col] };
    if n == 1 {
        return value(0);
    }
    let (first_t, last_t) = (time(0), time(n - 1));
    if t <= first_t {
        return value(0);
    }
    if t >= last_t {
        return value(n - 1);
    }
    let mut lo = 0usize;
    let mut hi = n - 1;
    while lo + 1 < hi {
        let mid = (lo + hi) / 2;
        if time(mid) <= t { lo = mid } else { hi = mid }
    }
    let (t0, t1) = (time(lo), time(hi));
    let f = if (t1 - t0).abs() > 1e-9 { (t - t0) / (t1 - t0) } else { 0.0 };
    let (a, b) = (value(lo), value(hi));
    a + (b - a) * f
}

// ─── GLTF_ANIM_RUNTIME_V2_DESIGN.md D3 — slice-based samplers ─────────────
// Same interpolation math as `sample_vec3_range`/`sample_quat_range`
// above (LINEAR lerp/slerp, STEP hold, CUBICSPLINE Hermite), retargeted
// from `TableData` rows to flat `&[f32]` slices (`gltf_anim_cache::
// Channel`'s storage) so a lookup is `partition_point` + O(1) indexing
// instead of a table row read. `values`/`in_tangents`/`out_tangents` are
// flat SoA: `values[i*stride..i*stride+stride]` is keyframe `i`.

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

    #[test]
    fn row_range_for_compound_key_finds_the_matching_clip_and_joint_block() {
        // (clip, joint) rows: (0,0),(0,0),(0,1),(1,0),(1,1),(1,1)
        let table = TableData::new(vec![
            vec![0.0, 0.0, 0.0, 0.0],
            vec![0.0, 0.0, 1.0, 0.0],
            vec![0.0, 1.0, 0.0, 0.0],
            vec![1.0, 0.0, 0.0, 0.0],
            vec![1.0, 1.0, 0.0, 0.0],
            vec![1.0, 1.0, 1.0, 0.0],
        ])
        .unwrap();
        assert_eq!(row_range_for_compound_key(Some(&table), 0, 0), (0, 2));
        assert_eq!(row_range_for_compound_key(Some(&table), 0, 1), (2, 3));
        assert_eq!(row_range_for_compound_key(Some(&table), 1, 0), (3, 4));
        assert_eq!(row_range_for_compound_key(Some(&table), 1, 1), (4, 6));
        assert_eq!(row_range_for_compound_key(Some(&table), 2, 0), (0, 0), "no rows for clip 2");
    }

    #[test]
    fn sample_vec3_range_lerps_with_configurable_columns() {
        // clip_index, time_s, x, y, z
        let table = TableData::new(vec![vec![0.0, 0.0, 0.0, 0.0, 0.0], vec![0.0, 1.0, 10.0, 0.0, 0.0]])
            .unwrap();
        let range = row_range_for_key(Some(&table), 0);
        let mid = sample_vec3_range(Some(&table), range, 1, 2, 0.5, [0.0, 0.0, 0.0]);
        assert!((mid[0] - 5.0).abs() < 1e-4);
    }

    // ─── IMPORT_ANYTHING_WAVE_DESIGN.md W2 — STEP + CUBICSPLINE ────────────
    // Both tests use the design doc's own fixture shape: a 4-keyframe
    // translation channel, times 0/0.5/1.0/1.5, values
    // (0,0,0)->(1,0,0)->(1,1,0)->(0,0,0). Row layout: [clip, time, x,y,z,
    // mode, in_x,in_y,in_z, out_x,out_y,out_z] — mode_col = val_col+3 = 5.

    fn interp_track_table(mode: f32) -> TableData {
        let row = |time: f32, v: [f32; 3]| {
            vec![0.0, time, v[0], v[1], v[2], mode, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]
        };
        TableData::new(vec![
            row(0.0, [0.0, 0.0, 0.0]),
            row(0.5, [1.0, 0.0, 0.0]),
            row(1.0, [1.0, 1.0, 0.0]),
            row(1.5, [0.0, 0.0, 0.0]),
        ])
        .unwrap()
    }

    #[test]
    fn step_holds_the_lower_keyframe_value() {
        let table = interp_track_table(1.0); // STEP
        let range = row_range_for_key(Some(&table), 0);
        let at_025 = sample_vec3_range(Some(&table), range, 1, 2, 0.25, [9.0, 9.0, 9.0]);
        assert!(
            (at_025[0] - 0.0).abs() < 1e-4 && (at_025[1] - 0.0).abs() < 1e-4,
            "STEP at t=0.25 must hold keyframe0's (0,0,0), got {at_025:?}"
        );
        let at_06 = sample_vec3_range(Some(&table), range, 1, 2, 0.6, [9.0, 9.0, 9.0]);
        assert!(
            (at_06[0] - 1.0).abs() < 1e-4 && (at_06[1] - 0.0).abs() < 1e-4,
            "STEP at t=0.6 must hold keyframe1's (1,0,0), got {at_06:?}"
        );
    }

    #[test]
    fn cubicspline_with_zero_tangents_hermite_blends_the_bracketing_values() {
        let table = interp_track_table(2.0); // CUBICSPLINE
        let range = row_range_for_key(Some(&table), 0);
        let at_025 = sample_vec3_range(Some(&table), range, 1, 2, 0.25, [9.0, 9.0, 9.0]);
        assert!(
            (at_025[0] - 0.5).abs() < 1e-4 && (at_025[1] - 0.0).abs() < 1e-4,
            "CUBICSPLINE (zero tangents) at t=0.25 must equal the smoothstep blend (0.5,0,0), \
             got {at_025:?}"
        );
    }

    #[test]
    fn cubicspline_quat_hermite_result_is_renormalized() {
        // Two keyframes 90-degrees apart about Z, zero tangents — a bare
        // Hermite blend of two unit quaternions is not itself unit length,
        // so the sampler must renormalize.
        let row = |time: f32, v: [f32; 4]| {
            vec![0.0, time, v[0], v[1], v[2], v[3], 2.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]
        };
        let table = TableData::new(vec![
            row(0.0, [0.0, 0.0, 0.0, 1.0]),
            row(1.0, [0.0, 0.0, std::f32::consts::FRAC_1_SQRT_2, std::f32::consts::FRAC_1_SQRT_2]),
        ])
        .unwrap();
        let range = row_range_for_key(Some(&table), 0);
        let q = sample_quat_range(Some(&table), range, 1, 2, 0.5);
        let len_sq = q[0] * q[0] + q[1] * q[1] + q[2] * q[2] + q[3] * q[3];
        assert!((len_sq - 1.0).abs() < 1e-4, "Hermite quat blend must be renormalized, got {q:?}");
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
}
