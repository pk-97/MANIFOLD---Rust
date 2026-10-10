//! Low filter bank (16): eight causal bandpass envelopes below 400 Hz, the rise of the
//! early-half and late-half max over the pre-onset median. Port of `band_envelopes` and
//! `lowbank_features` in `tools/audio_analysis/eval/kick_goal_lowbank.py`.

use super::dsp::{History, MsGrid, RunningMean, Sos};
use super::{Consts, HIST_MS};
use crate::kick::container::{Container, ContainerError};

const BANDS: usize = 8;
/// Pre-onset median window, ms relative to the onset: `[-60, -10)`.
const BASE_FROM: u64 = 60;
const BASE_TO: u64 = 10;
const BASE_LEN: usize = (BASE_FROM - BASE_TO) as usize;
/// The reference zeroes candidates whose onset lands before this grid index.
const MIN_ONSET_MS: i64 = 60;

pub(super) struct LowBank {
    sos: Vec<Sos>,
    ma: Vec<RunningMean>,
    grid: MsGrid,
    floor: f64,
    env: History<[f64; BANDS]>,
}

impl LowBank {
    pub fn new(c: &Container, k: &Consts) -> Result<Self, ContainerError> {
        let ma_len = super::positive(c, "extra.low_ma_len")? as usize;
        let sos = c.f64s("extra.low_sos", &[BANDS, 2, 6])?;
        Ok(Self {
            sos: sos.chunks_exact(12).map(Sos::new).collect(),
            ma: (0..BANDS).map(|_| RunningMean::new(ma_len)).collect(),
            grid: MsGrid::new(k.ms, k.sr),
            floor: super::scalar(c, "extra.low_floor")?,
            env: History::new(HIST_MS),
        })
    }

    #[inline]
    pub fn push(&mut self, s: u64, x: f64) {
        let mut m = [0.0; BANDS];
        for ((v, sos), ma) in m.iter_mut().zip(&mut self.sos).zip(&mut self.ma) {
            *v = ma.push(sos.step(x).abs());
        }
        if self.grid.hit(s).is_some() {
            self.env.push(m.map(|v| 20.0 * (v + self.floor).log10()));
        }
    }

    /// `int(t / MS)`: the reference truncates here, where the tail rounds.
    fn grid_index(t: f64, k: &Consts) -> i64 {
        (t / k.ms) as i64
    }

    pub fn ready_sample(&self, k: &Consts, avail: u64) -> u64 {
        let d = Self::grid_index(k.hop_end_s(avail), k).max(0) as u64;
        self.grid.sample_of(d) + 1
    }

    /// `[low_early_<centre>.., low_late_<centre>..]`.
    pub fn features(&self, k: &Consts, cand: u64, avail: u64, out: &mut [f64]) {
        out.fill(0.0);
        let a = Self::grid_index(k.hop_end_s(cand), k);
        let d = Self::grid_index(k.hop_end_s(avail), k);
        if a < MIN_ONSET_MS || d <= a {
            return;
        }
        let (a, d) = (a as u64, d as u64);
        assert!(self.env.holds(a - BASE_FROM, d), "kick low bank: candidate {cand} read outside the stream history");
        let mid = a + (d - a).div_ceil(2).max(1);
        let (early, late) = out.split_at_mut(BANDS);
        for b in 0..BANDS {
            let mut w = [0.0; BASE_LEN];
            for (i, v) in w.iter_mut().enumerate() {
                *v = self.env.get(a - BASE_FROM + i as u64)[b];
            }
            let base = median_even(&mut w);
            let max_over = |from: u64, to: u64| (from..to).map(|i| self.env.get(i)[b]).fold(f64::NEG_INFINITY, f64::max);
            early[b] = max_over(a, mid) - base;
            late[b] = if d + 1 > mid { max_over(mid, d + 1) - base } else { early[b] };
        }
    }
}

/// `np.median` of an even-length window: the mean of the two middle values.
fn median_even(w: &mut [f64]) -> f64 {
    debug_assert!(w.len().is_multiple_of(2) && !w.is_empty());
    w.sort_unstable_by(f64::total_cmp);
    let h = w.len() / 2;
    (w[h - 1] + w[h]) / 2.0
}

#[cfg(test)]
mod tests {
    use super::median_even;

    #[test]
    fn median_of_even_window_is_mean_of_middle_pair() {
        assert_eq!(median_even(&mut [4.0, 1.0, 3.0, 2.0]), 2.5);
    }
}
