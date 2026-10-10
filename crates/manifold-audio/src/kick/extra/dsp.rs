//! Building blocks shared by the extra features: absolute-index history rings, the scipy
//! `sosfilt` cascade, the causal running-sum moving average, and the reference's 1 ms grid.

/// The last `cap` values of a stream, addressed by absolute index.
pub(super) struct History<T> {
    buf: Vec<T>,
    pushed: u64,
}

impl<T: Copy + Default> History<T> {
    pub fn new(cap: usize) -> Self {
        Self { buf: vec![T::default(); cap], pushed: 0 }
    }

    pub fn push(&mut self, v: T) {
        let cap = self.buf.len() as u64;
        self.buf[(self.pushed % cap) as usize] = v;
        self.pushed += 1;
    }

    /// Number of values pushed so far (the next absolute index).
    pub fn len(&self) -> u64 {
        self.pushed
    }

    /// Whether `[first, last]` has been pushed and not yet overwritten.
    pub fn holds(&self, first: u64, last: u64) -> bool {
        last < self.pushed && first + self.buf.len() as u64 >= self.pushed
    }

    pub fn get(&self, i: u64) -> &T {
        debug_assert!(self.holds(i, i), "history index {i} outside [{}, {})", self.pushed.saturating_sub(self.buf.len() as u64), self.pushed);
        &self.buf[(i % self.buf.len() as u64) as usize]
    }
}

/// Second-order sections in scipy's `sosfilt` form, zero initial state.
pub(super) struct Sos {
    coef: Vec<[f64; 6]>,
    z: Vec<[f64; 2]>,
}

impl Sos {
    pub fn new(flat: &[f64]) -> Self {
        let coef: Vec<[f64; 6]> = flat.chunks_exact(6).map(|c| c.try_into().expect("6 coefficients")).collect();
        let z = vec![[0.0; 2]; coef.len()];
        Self { coef, z }
    }

    /// One sample through the cascade, in scipy `_sosfilt`'s direct-form-II-transposed
    /// operation order so the outputs match bit for bit.
    #[inline]
    pub fn step(&mut self, x: f64) -> f64 {
        let mut cur = x;
        for (c, z) in self.coef.iter().zip(self.z.iter_mut()) {
            let y = c[0] * cur + z[0];
            z[0] = c[1] * cur - c[4] * y + z[1];
            z[1] = c[2] * cur - c[5] * y;
            cur = y;
        }
        cur
    }
}

/// The reference's `k`-sample moving average from a running `np.cumsum`: the value at
/// sample `s` is `(cum[s] - cum[s - k]) / k`, with `cum[s - k] = 0` before the stream.
/// Kept as a running sum, not a sliding one, because the reference's cancellation error
/// is part of its output.
pub(super) struct RunningMean {
    cum: History<f64>,
    sum: f64,
    k: u64,
}

impl RunningMean {
    pub fn new(k: usize) -> Self {
        Self { cum: History::new(k + 1), sum: 0.0, k: k as u64 }
    }

    /// Adds sample `s = self.len()` and returns its moving average.
    #[inline]
    pub fn push(&mut self, y: f64) -> f64 {
        self.sum += y;
        let s = self.cum.len();
        self.cum.push(self.sum);
        let old = if s >= self.k { *self.cum.get(s - self.k) } else { 0.0 };
        (self.sum - old) / self.k as f64
    }
}

/// The reference's exact 1 ms grid, `np.round(np.arange(n) * MS * sr)`: walks grid
/// points in order and says when a sample index is one.
pub(super) struct MsGrid {
    ms: f64,
    sr: f64,
    next: u64,
    next_sample: u64,
}

impl MsGrid {
    pub fn new(ms: f64, sr: f64) -> Self {
        Self { ms, sr, next: 0, next_sample: 0 }
    }

    /// The sample index of grid point `j`.
    pub fn sample_of(&self, j: u64) -> u64 {
        ((j as f64) * self.ms * self.sr).round_ties_even() as u64
    }

    /// If sample `s` is the next grid point, returns its grid index and advances.
    #[inline]
    pub fn hit(&mut self, s: u64) -> Option<u64> {
        if s != self.next_sample {
            return None;
        }
        let j = self.next;
        self.next += 1;
        self.next_sample = self.sample_of(self.next);
        Some(j)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_addresses_absolute_indices() {
        let mut h = History::<f64>::new(4);
        for i in 0..10 {
            h.push(i as f64);
        }
        assert!(h.holds(6, 9));
        assert!(!h.holds(5, 9));
        assert!(!h.holds(6, 10));
        assert_eq!(*h.get(7), 7.0);
    }

    #[test]
    fn running_mean_includes_current_sample() {
        let mut m = RunningMean::new(3);
        let out: Vec<f64> = [3.0, 6.0, 9.0, 12.0].iter().map(|&y| m.push(y)).collect();
        assert_eq!(out, vec![1.0, 3.0, 6.0, 9.0]);
    }

    #[test]
    fn ms_grid_is_every_48_samples_at_48k() {
        let mut g = MsGrid::new(0.001, 48000.0);
        let hits: Vec<(u64, u64)> = (0..200).filter_map(|s| g.hit(s).map(|j| (j, s))).collect();
        assert_eq!(hits, vec![(0, 0), (1, 48), (2, 96), (3, 144), (4, 192)]);
    }

    #[test]
    fn sos_identity_section_passes_through() {
        let mut f = Sos::new(&[1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);
        assert_eq!(f.step(0.25), 0.25);
    }
}
