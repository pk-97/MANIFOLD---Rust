//! Tail features (3): does a hit jump above where the ringing low-band tail should be?
//! Port of `tools/audio_analysis/eval/kick_goal_tail_features.py`.

use super::dsp::{History, MsGrid, RunningMean, Sos};
use super::{Consts, HIST_MS};
use crate::kick::container::{Container, ContainerError};

/// Pre-onset fit window, ms relative to the onset: `[-50, -5]` inclusive.
const PRE_FROM: u64 = 50;
const PRE_TO: u64 = 5;
const PRE_LEN: usize = (PRE_FROM - PRE_TO + 1) as usize;
/// The reference zeroes candidates whose onset lands before this grid index.
const MIN_ONSET_MS: i64 = 60;

/// Running max over the last `w` values: a monotonic deque in a fixed ring.
struct MaxDeque {
    idx: Vec<u64>,
    val: Vec<f64>,
    head: usize,
    len: usize,
    w: u64,
}

impl MaxDeque {
    fn new(w: usize) -> Self {
        Self { idx: vec![0; w + 1], val: vec![0.0; w + 1], head: 0, len: 0, w: w as u64 }
    }

    fn slot(&self, k: usize) -> usize {
        (self.head + k) % self.idx.len()
    }

    /// Adds value `v` at index `i` and returns the max over `[i - w + 1, i]`.
    fn push(&mut self, i: u64, v: f64) -> f64 {
        while self.len > 0 && self.val[self.slot(self.len - 1)] <= v {
            self.len -= 1;
        }
        let s = self.slot(self.len);
        self.idx[s] = i;
        self.val[s] = v;
        self.len += 1;
        while self.idx[self.head] + self.w <= i {
            self.head = (self.head + 1) % self.idx.len();
            self.len -= 1;
        }
        self.val[self.head]
    }
}

pub(super) struct Tail {
    sos: Sos,
    ma: RunningMean,
    grid: MsGrid,
    ma_len: u64,
    floor: f64,
    /// Grid points before sample `ma_len - 1`: the reference clips their source index to
    /// the first full average, so they wait for it.
    pending: u64,
    env: History<f64>,
    past_max: History<f64>,
    deque: MaxDeque,
}

impl Tail {
    pub fn new(c: &Container, k: &Consts) -> Result<Self, ContainerError> {
        let ma_len = super::positive(c, "extra.tail_ma_len")?;
        let past = super::positive(c, "extra.tail_past_ms")?;
        Ok(Self {
            sos: Sos::new(c.f64s("extra.tail_sos", &[4, 6])?),
            ma: RunningMean::new(ma_len as usize),
            grid: MsGrid::new(k.ms, k.sr),
            ma_len,
            floor: super::scalar(c, "extra.tail_floor")?,
            pending: 0,
            env: History::new(HIST_MS),
            past_max: History::new(HIST_MS),
            deque: MaxDeque::new(past as usize),
        })
    }

    #[inline]
    pub fn push(&mut self, s: u64, x: f64) {
        let y = self.sos.step(x);
        let m = self.ma.push(y * y);
        if s + 1 == self.ma_len {
            for _ in 0..std::mem::take(&mut self.pending) {
                self.emit(m);
            }
        }
        if self.grid.hit(s).is_some() {
            if s + 1 < self.ma_len {
                self.pending += 1;
            } else {
                self.emit(m);
            }
        }
    }

    fn emit(&mut self, ma: f64) {
        let e = 10.0 * (ma + self.floor).log10();
        let j = self.env.len();
        self.env.push(e);
        // Causal max over the past 4 s, inclusive. The reference's `maximum_filter1d`
        // reflects at the stream start, reading up to 4 s ahead for indices under 2 s;
        // a stream cannot, so this deviates there (approved, lead 2026-10-10).
        self.past_max.push(self.deque.push(j, e));
    }

    fn grid_index(t: f64, k: &Consts) -> i64 {
        (t / k.ms).round_ties_even() as i64
    }

    pub fn ready_sample(&self, k: &Consts, avail: u64) -> u64 {
        let i1 = Self::grid_index(k.hop_end_s(avail), k).max(0) as u64;
        self.grid.sample_of(i1) + 1
    }

    /// `[tail_excess_db, pre_rel_db, pre_slope_db_ms]`.
    pub fn features(&self, k: &Consts, cand: u64, avail: u64, out: &mut [f64]) {
        out.fill(0.0);
        let i0 = Self::grid_index(k.hop_end_s(cand), k);
        let i1 = Self::grid_index(k.hop_end_s(avail), k);
        if i0 < MIN_ONSET_MS {
            return;
        }
        let (i0, i1) = (i0 as u64, i1 as u64);
        assert!(
            self.env.holds(i0 - PRE_FROM, i1) && self.past_max.holds(i0 - PRE_TO, i0 - PRE_TO),
            "kick tail: candidate {cand} read outside the stream history"
        );

        // np.polyfit(tt, pre, 1) on tt = -50..-5, as the closed-form least-squares line.
        let mut pre = [0.0; PRE_LEN];
        for (m, p) in pre.iter_mut().enumerate() {
            *p = *self.env.get(i0 - PRE_FROM + m as u64);
        }
        let n = PRE_LEN as f64;
        let mean = pre.iter().sum::<f64>() / n;
        let xm = (0..PRE_LEN).map(|m| m as f64 - PRE_FROM as f64).sum::<f64>() / n;
        let (mut sxy, mut sxx) = (0.0, 0.0);
        for (m, &p) in pre.iter().enumerate() {
            let dx = m as f64 - PRE_FROM as f64 - xm;
            sxy += dx * (p - mean);
            sxx += dx * dx;
        }
        let slope = sxy / sxx;
        let icpt = mean - slope * xm;
        let cap = pre.iter().copied().fold(f64::NEG_INFINITY, f64::max);

        let mut excess = f64::NEG_INFINITY;
        for h in 0..=(i1 - i0) {
            let pred = (icpt + slope * h as f64).min(cap);
            excess = excess.max(self.env.get(i0 + h) - pred);
        }
        out[0] = excess;
        out[1] = mean - self.past_max.get(i0 - PRE_TO);
        out[2] = slope;
    }
}

#[cfg(test)]
mod tests {
    use super::MaxDeque;

    #[test]
    fn deque_matches_brute_force_window_max() {
        let w = 5;
        let mut d = MaxDeque::new(w);
        let mut seed = 12345u64;
        let mut xs = Vec::new();
        for i in 0..200u64 {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let v = ((seed >> 33) % 17) as f64;
            xs.push(v);
            let lo = (i as usize + 1).saturating_sub(w);
            let want = xs[lo..].iter().copied().fold(f64::NEG_INFINITY, f64::max);
            assert_eq!(d.push(i, v), want, "at {i}");
        }
    }
}
