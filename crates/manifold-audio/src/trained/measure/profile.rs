//! prof32 and fall16 on the paired band spectrum. Ports of `rise_profile` and `fall_profile` in
//! `tools/audio_analysis/eval/kick_goal_profile.py`: per paired band, against the mean of the three frames
//! ending two hops before the candidate, over the early and the late half of `[cand, avail]`.

use super::spec::{BandSpec, PAIRS};
use super::templates::{BASE_FROM, BASE_LEN, MIN_CAND};

/// The pre-onset base per paired band.
fn base(spec: &BandSpec, cand: u64) -> [f64; PAIRS] {
    let mut base = [0.0; PAIRS];
    for (b, v) in base.iter_mut().enumerate() {
        let f = |d: u64| spec.paired_at(cand - BASE_FROM + d)[b];
        *v = (f(0) + f(1) + f(2)) / BASE_LEN as f64;
    }
    base
}

/// The first frame of the late half.
fn mid(cand: u64, avail: u64) -> u64 {
    cand + (avail - cand).div_ceil(2).max(1)
}

fn check(spec: &BandSpec, cand: u64, avail: u64) {
    assert!(avail >= cand, "profile: deadline {avail} before candidate {cand}");
    assert!(spec.holds(cand - BASE_FROM, avail), "profile: candidate {cand} read outside the stream history");
}

/// prof32: `[rise_early_0..16, rise_late_0..16]`, each band's max rise.
pub(super) fn rise(spec: &BandSpec, cand: u64, avail: u64, out: &mut [f64]) {
    out.fill(0.0);
    if cand < MIN_CAND {
        return;
    }
    check(spec, cand, avail);
    let base = base(spec, cand);
    let mid = mid(cand, avail);
    let max_over = |from: u64, to: u64, b: usize| (from..to).map(|f| spec.paired_at(f)[b]).fold(f64::NEG_INFINITY, f64::max);
    let (early, late) = out.split_at_mut(PAIRS);
    for b in 0..PAIRS {
        early[b] = max_over(cand, mid, b) - base[b];
        late[b] = if avail + 1 > mid { max_over(mid, avail + 1, b) - base[b] } else { early[b] };
    }
}

/// fall16: `fall_late_0..16`, each band's late-half minimum against the base (0 without a late half).
pub(super) fn fall(spec: &BandSpec, cand: u64, avail: u64, out: &mut [f64]) {
    out.fill(0.0);
    if cand < MIN_CAND {
        return;
    }
    check(spec, cand, avail);
    let base = base(spec, cand);
    let mid = mid(cand, avail);
    if avail < mid {
        return;
    }
    for (b, o) in out.iter_mut().enumerate() {
        *o = (mid..avail + 1).map(|f| spec.paired_at(f)[b]).fold(f64::INFINITY, f64::min) - base[b];
    }
}
