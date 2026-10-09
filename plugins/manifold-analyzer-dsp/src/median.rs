//! Fixed-storage streaming quantile estimates.
//!
//! [`P2Quantile`] implements the P² algorithm from Jain and Chlamtac. It
//! keeps five marker heights and positions, so adding a sample never grows
//! storage with the length of the stream. The result is an approximation of
//! the requested quantile; it is not an exact median or percentile.

use std::collections::BTreeMap;

/// A fixed-storage approximation of one quantile in a sample stream.
#[derive(Clone, Debug)]
pub struct P2Quantile {
    quantile: f64,
    count: u64,
    initial: [f64; 5],
    initial_len: usize,
    heights: [f64; 5],
    positions: [f64; 5],
    desired_positions: [f64; 5],
    position_increments: [f64; 5],
}

impl P2Quantile {
    /// Create an estimator for `quantile`, which must be in `[0, 1]`.
    pub fn new(quantile: f32) -> Self {
        assert!(quantile.is_finite() && (0.0..=1.0).contains(&quantile));
        let q = quantile as f64;
        Self {
            quantile: q,
            count: 0,
            initial: [0.0; 5],
            initial_len: 0,
            heights: [0.0; 5],
            positions: [0.0; 5],
            desired_positions: [0.0; 5],
            position_increments: [0.0, q * 0.5, q, (1.0 + q) * 0.5, 1.0],
        }
    }

    /// Add one finite sample to the stream. Non-finite samples are ignored.
    pub fn add(&mut self, value: f32) {
        if !value.is_finite() {
            return;
        }
        let value = value as f64;
        self.count += 1;
        if self.initial_len < self.initial.len() {
            self.initial[self.initial_len] = value;
            self.initial_len += 1;
            if self.initial_len == self.initial.len() {
                self.initial.sort_by(|a, b| a.total_cmp(b));
                self.heights.copy_from_slice(&self.initial);
                self.positions = [1.0, 2.0, 3.0, 4.0, 5.0];
                self.desired_positions = [
                    1.0,
                    1.0 + 2.0 * self.quantile,
                    1.0 + 4.0 * self.quantile,
                    3.0 + 2.0 * self.quantile,
                    5.0,
                ];
            }
            return;
        }

        let mut k = 0usize;
        if value < self.heights[0] {
            self.heights[0] = value;
        } else if value >= self.heights[4] {
            self.heights[4] = value;
            k = 3;
        } else {
            for i in 0..4 {
                if value < self.heights[i + 1] {
                    k = i;
                    break;
                }
            }
        }

        for i in (k + 1)..5 {
            self.positions[i] += 1.0;
        }
        for i in 0..5 {
            self.desired_positions[i] += self.position_increments[i];
        }

        for i in 1..4 {
            let delta = self.desired_positions[i] - self.positions[i];
            let can_raise = self.positions[i + 1] - self.positions[i] > 1.0;
            let can_lower = self.positions[i] - self.positions[i - 1] > 1.0;
            let direction = if delta >= 1.0 && can_raise {
                1.0
            } else if delta <= -1.0 && can_lower {
                -1.0
            } else {
                0.0
            };
            if direction == 0.0 {
                continue;
            }

            let next = (i as isize + direction as isize) as usize;
            let candidate = self.parabolic(i, direction);
            self.heights[i] = if self.heights[i - 1] < candidate && candidate < self.heights[i + 1]
            {
                candidate
            } else {
                self.linear(i, next, direction)
            };
            self.positions[i] += direction;
        }
    }

    fn parabolic(&self, i: usize, direction: f64) -> f64 {
        let n = &self.positions;
        let q = &self.heights;
        q[i] + direction / (n[i + 1] - n[i - 1])
            * ((n[i] - n[i - 1] + direction) * (q[i + 1] - q[i]) / (n[i + 1] - n[i])
                + (n[i + 1] - n[i] - direction) * (q[i] - q[i - 1]) / (n[i] - n[i - 1]))
    }

    fn linear(&self, i: usize, next: usize, direction: f64) -> f64 {
        self.heights[i]
            + direction * (self.heights[next] - self.heights[i])
                / (self.positions[next] - self.positions[i])
    }

    /// Number of finite samples accepted by this estimator.
    pub fn count(&self) -> u64 {
        self.count
    }

    /// Return the current approximate quantile, or `None` for an empty stream.
    pub fn estimate(&self) -> Option<f32> {
        if self.count == 0 {
            return None;
        }
        if self.initial_len < self.initial.len() {
            let mut values = self.initial;
            values[..self.initial_len].sort_by(|a, b| a.total_cmp(b));
            let index = self.quantile * (self.initial_len - 1) as f64;
            let lower = index.floor() as usize;
            let upper = index.ceil() as usize;
            let fraction = index - lower as f64;
            return Some((values[lower] + (values[upper] - values[lower]) * fraction) as f32);
        }
        let height = if self.quantile == 0.0 {
            self.heights[0]
        } else if self.quantile == 1.0 {
            self.heights[4]
        } else {
            self.heights[2]
        };
        Some(height as f32)
    }
}

impl Default for P2Quantile {
    fn default() -> Self {
        Self::new(0.5)
    }
}

/// Quantization step for reference-track dB histograms.
pub const HISTOGRAM_STEP_DB: f32 = 0.01;
const HISTOGRAM_MIN_DB: f32 = -240.0;
const HISTOGRAM_MAX_DB: f32 = 400.0;

/// Errors rejected by [`QuantizedHistogram::add`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum HistogramError {
    NonFinite,
    OutOfRange,
}

impl std::fmt::Display for HistogramError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NonFinite => write!(f, "non-finite dB sample"),
            Self::OutOfRange => write!(f, "dB sample outside histogram range"),
        }
    }
}

impl std::error::Error for HistogramError {}

/// A bounded sparse dB histogram for exact-enough streaming quantiles.
///
/// The key is a 0.01 dB bin, so each returned estimate differs from the
/// corresponding quantile of the input values by at most half a bin. The
/// map's size is bounded by the fixed `[-240, 400]` dB domain, independent
/// of stream duration. Non-finite and out-of-domain samples are rejected.
#[derive(Clone, Debug, Default)]
pub struct QuantizedHistogram {
    bins: BTreeMap<i32, u64>,
    count: u64,
}

impl QuantizedHistogram {
    #[cfg(test)]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&mut self, value: f32) -> Result<(), HistogramError> {
        if !value.is_finite() {
            return Err(HistogramError::NonFinite);
        }
        if !(HISTOGRAM_MIN_DB..=HISTOGRAM_MAX_DB).contains(&value) {
            return Err(HistogramError::OutOfRange);
        }
        let key = (value / HISTOGRAM_STEP_DB).round() as i32;
        *self.bins.entry(key).or_default() += 1;
        self.count += 1;
        Ok(())
    }

    #[cfg(test)]
    pub fn count(&self) -> u64 {
        self.count
    }

    #[cfg(test)]
    pub fn distinct_bins(&self) -> usize {
        self.bins.len()
    }

    /// Return the rounded-rank quantile used by the original reference path.
    pub fn estimate(&self, quantile: f32) -> Option<f32> {
        assert!(quantile.is_finite() && (0.0..=1.0).contains(&quantile));
        if self.count == 0 {
            return None;
        }
        let rank = (quantile * (self.count - 1) as f32).round() as u64;
        Some(self.value_at_rank(rank))
    }

    /// Return approximate `[10th, 50th, 90th]` percentiles.
    pub fn estimates(&self) -> Option<[f32; 3]> {
        Some([
            self.estimate(0.10)?,
            self.estimate(0.50)?,
            self.estimate(0.90)?,
        ])
    }

    fn value_at_rank(&self, rank: u64) -> f32 {
        let mut offset = 0u64;
        for (&key, &count) in &self.bins {
            if rank < offset + count {
                return key as f32 * HISTOGRAM_STEP_DB;
            }
            offset += count;
        }
        unreachable!("rank is within histogram sample count")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_uniform_distribution_stays_within_expected_error() {
        let mut q10 = P2Quantile::new(0.10);
        let mut median = P2Quantile::new(0.50);
        let mut q90 = P2Quantile::new(0.90);
        for value in 0..10_000 {
            let value = value as f32 / 10_000.0;
            q10.add(value);
            median.add(value);
            q90.add(value);
        }
        assert!((q10.estimate().unwrap() - 0.10).abs() < 0.01);
        assert!((median.estimate().unwrap() - 0.50).abs() < 0.01);
        assert!((q90.estimate().unwrap() - 0.90).abs() < 0.01);
    }

    #[test]
    fn small_stream_is_exactly_interpolated() {
        let mut estimator = P2Quantile::new(0.5);
        for value in [4.0, 1.0, 3.0, 2.0] {
            estimator.add(value);
        }
        assert_eq!(estimator.count(), 4);
        assert_eq!(estimator.estimate(), Some(2.5));
    }

    #[test]
    fn storage_does_not_grow_with_stream_length() {
        let mut estimator = P2Quantile::new(0.5);
        let size = std::mem::size_of_val(&estimator);
        for value in 0..100_000 {
            estimator.add(value as f32);
        }
        assert_eq!(std::mem::size_of_val(&estimator), size);
        assert_eq!(estimator.count(), 100_000);
    }

    #[test]
    fn endpoint_quantiles_track_stream_extrema() {
        let mut minimum = P2Quantile::new(0.0);
        let mut maximum = P2Quantile::new(1.0);
        for value in [8.0, 3.0, 5.0, 1.0, 13.0, 2.0] {
            minimum.add(value);
            maximum.add(value);
        }
        assert_eq!(minimum.estimate(), Some(1.0));
        assert_eq!(maximum.estimate(), Some(13.0));
    }

    #[test]
    fn reference_histogram_matches_uniform_and_transient_quantiles() {
        let mut histogram = QuantizedHistogram::new();
        for value in 0..9_500 {
            histogram.add(value as f32 * 0.01).unwrap();
        }
        for _ in 0..500 {
            histogram.add(100.0).unwrap();
        }
        let values = histogram.estimates().unwrap();
        assert!((values[0] - 9.999).abs() <= 0.011, "{values:?}");
        assert!((values[1] - 49.995).abs() <= 0.011, "{values:?}");
        assert!((values[2] - 89.991).abs() <= 0.011, "{values:?}");

        let mut uniform = QuantizedHistogram::new();
        for value in 0..10_000 {
            uniform.add(value as f32 * 0.01).unwrap();
        }
        let values = uniform.estimates().unwrap();
        assert!((values[0] - 9.999).abs() <= 0.011, "{values:?}");
        assert!((values[1] - 49.995).abs() <= 0.011, "{values:?}");
        assert!((values[2] - 89.991).abs() <= 0.011, "{values:?}");
    }

    #[test]
    fn reference_histogram_tracks_silence_and_rejects_nonfinite() {
        let mut histogram = QuantizedHistogram::new();
        histogram.add(-240.0).unwrap();
        histogram.add(-120.0).unwrap();
        assert_eq!(histogram.count(), 2);
        assert_eq!(histogram.distinct_bins(), 2);
        assert_eq!(histogram.add(f32::NAN), Err(HistogramError::NonFinite));
        assert_eq!(histogram.add(f32::INFINITY), Err(HistogramError::NonFinite));
        assert_eq!(histogram.add(401.0), Err(HistogramError::OutOfRange));
    }

    #[test]
    fn reference_histogram_storage_is_bounded() {
        let mut histogram = QuantizedHistogram::new();
        for _ in 0..9_500 {
            histogram.add(0.0).unwrap();
        }
        for _ in 0..500 {
            histogram.add(100.0).unwrap();
        }
        assert_eq!(histogram.distinct_bins(), 2);
        assert_eq!(histogram.count(), 10_000);
    }
}
