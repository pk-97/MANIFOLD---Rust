//! Rise profile (32): per paired band, the rise over the pre-onset base, maxed over the
//! early and the late half of `[cand, avail]`. Port of `rise_profile` in
//! `tools/audio_analysis/eval/kick_goal_profile.py`.

use super::spec::{BandSpec, PAIRS};
use super::templates::{BASE_FROM, BASE_LEN, MIN_CAND};

/// `[rise_early_0..16, rise_late_0..16]`.
pub(super) fn features(spec: &BandSpec, cand: u64, avail: u64, out: &mut [f64]) {
    out.fill(0.0);
    if cand < MIN_CAND {
        return;
    }
    assert!(avail >= cand, "kick profile: deadline {avail} before candidate {cand}");
    assert!(spec.holds(cand - BASE_FROM, avail), "kick profile: candidate {cand} read outside the stream history");
    let mut base = [0.0; PAIRS];
    for (b, v) in base.iter_mut().enumerate() {
        let f = |d: u64| spec.paired_at(cand - BASE_FROM + d)[b];
        *v = (f(0) + f(1) + f(2)) / BASE_LEN as f64;
    }
    let mid = cand + (avail - cand).div_ceil(2).max(1);
    let max_over = |from: u64, to: u64, b: usize| (from..to).map(|f| spec.paired_at(f)[b]).fold(f64::NEG_INFINITY, f64::max);
    let (early, late) = out.split_at_mut(PAIRS);
    for b in 0..PAIRS {
        early[b] = max_over(cand, mid, b) - base[b];
        late[b] = if avail + 1 > mid { max_over(mid, avail + 1, b) - base[b] } else { early[b] };
    }
}
