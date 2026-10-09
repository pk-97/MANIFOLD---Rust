//! Cached, calibrated power-domain spectrum resampling.
use crate::MIN_DB;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SpectrumBandwidth {
    /// Maximum FFT-bin power in each output pixel, preserving narrow peaks.
    None,
    FixedHalfOctaves(f32),
    Erb,
}

pub struct SpectrumSmoothingPlan {
    frequencies: Vec<f32>,
    offsets: Vec<usize>,
    indices: Vec<usize>,
    weights: Vec<f64>,
    bins: usize,
    peak: bool,
}

impl SpectrumSmoothingPlan {
    pub fn new(
        sr: f32,
        size: usize,
        min: f32,
        max: f32,
        columns: usize,
        bandwidth: SpectrumBandwidth,
    ) -> Self {
        assert!(sr.is_finite() && sr > 0.0 && size >= 2 && columns > 0);
        assert!(min.is_finite() && max.is_finite() && min > 0.0 && max >= min);
        let bins = size / 2 + 1;
        let bin_hz = sr as f64 / size as f64;
        let log_lo = (min as f64).ln();
        let span = (max as f64).ln() - log_lo;
        let frequencies: Vec<f32> = (0..columns)
            .map(|i| {
                (log_lo + span * i as f64 / columns.saturating_sub(1).max(1) as f64).exp() as f32
            })
            .collect();
        let peak = bandwidth == SpectrumBandwidth::None;
        let mut plan = Self {
            frequencies,
            offsets: vec![0],
            indices: Vec::new(),
            weights: Vec::new(),
            bins,
            peak,
        };
        for col in 0..columns {
            let freq = plan.frequencies[col] as f64;
            let start = plan.indices.len();
            if peak {
                let lo = if col == 0 {
                    min as f64
                } else {
                    (freq * plan.frequencies[col - 1] as f64).sqrt()
                };
                let hi = if col + 1 == columns {
                    max as f64
                } else {
                    (freq * plan.frequencies[col + 1] as f64).sqrt()
                };
                let first = (lo / bin_hz).ceil() as usize;
                let last = ((hi / bin_hz).floor() as usize).min(bins - 1);
                if first <= last {
                    for i in first..=last {
                        plan.indices.push(i);
                        plan.weights.push(1.0);
                    }
                }
            } else {
                let half = match bandwidth {
                    SpectrumBandwidth::FixedHalfOctaves(h) => h.max(0.0) as f64,
                    SpectrumBandwidth::Erb => {
                        (1.0 + 0.5 * 24.7 * (4.37 * freq / 1000.0 + 1.0) / freq).log2()
                    }
                    SpectrumBandwidth::None => unreachable!(),
                };
                let lo = (freq * 2.0_f64.powf(-half) / bin_hz).clamp(0.0, (bins - 1) as f64);
                let hi = (freq * 2.0_f64.powf(half) / bin_hz).clamp(0.0, (bins - 1) as f64);
                let width = hi - lo;
                if width > 1e-8 {
                    for i in (lo.floor() as usize)..(hi.ceil() as usize).min(bins - 1) {
                        let a = lo.max(i as f64);
                        let b = hi.min((i + 1) as f64);
                        let (integral, moment) = bh_integrals((a - lo) / width, (b - lo) / width);
                        // Integrate linear power interpolation against the BH window.
                        let right = ((lo - i as f64) * integral + width * moment).max(0.0);
                        let left = (integral - right).max(0.0);
                        plan.add(start, i, left);
                        plan.add(start, i + 1, right);
                    }
                    let total: f64 = plan.weights[start..].iter().sum();
                    assert!(
                        total > 0.0 && total.is_finite(),
                        "invalid smoothing weights"
                    );
                    for w in &mut plan.weights[start..] {
                        *w /= total;
                    }
                }
            }
            if plan.indices.len() == start {
                plan.indices
                    .push(((freq / bin_hz).round() as usize).min(bins - 1));
                plan.weights.push(1.0);
            }
            plan.offsets.push(plan.indices.len());
        }
        plan
    }
    fn add(&mut self, row_start: usize, index: usize, weight: f64) {
        if weight == 0.0 {
            return;
        }
        if self.indices.len() > row_start && self.indices.last() == Some(&index) {
            *self.weights.last_mut().unwrap() += weight;
        } else {
            self.indices.push(index);
            self.weights.push(weight);
        }
    }
    pub fn frequencies(&self) -> &[f32] {
        &self.frequencies
    }
    pub fn columns(&self) -> usize {
        self.frequencies.len()
    }
    pub fn num_bins(&self) -> usize {
        self.bins
    }
    /// Average signed scalar fields (e.g. correlation), using the same frequency support.
    pub fn apply_values(&self, values: &[f32], out: &mut [f32]) {
        assert!(values.len() >= self.bins && out.len() >= self.columns());
        for (row, dst) in out.iter_mut().take(self.columns()).enumerate() {
            let range = self.offsets[row]..self.offsets[row + 1];
            let mut sum = 0.0;
            let mut weight = 0.0;
            for j in range {
                sum += values[self.indices[j]] as f64 * self.weights[j];
                weight += self.weights[j];
            }
            *dst = (sum / weight) as f32;
        }
    }
    /// Input and output storage belongs to the caller; applying a plan never allocates.
    pub fn apply_powers(&self, powers: &[f32], out: &mut [f32]) {
        assert!(powers.len() >= self.bins && out.len() >= self.columns());
        for (row, dst) in out.iter_mut().take(self.columns()).enumerate() {
            let range = self.offsets[row]..self.offsets[row + 1];
            let mut sum = 0.0_f64;
            for j in range {
                let p = powers[self.indices[j]].max(0.0) as f64;
                if self.peak {
                    sum = sum.max(p);
                } else {
                    sum += p * self.weights[j];
                }
            }
            *dst = ((10.0 * sum.max(1e-12).log10()) as f32).max(MIN_DB);
        }
    }
}

// Integrals of BH(u) and u*BH(u), with u=0/1 at the window edges.
fn bh_integrals(a: f64, b: f64) -> (f64, f64) {
    let mut area = 0.35875 * (b - a);
    let mut moment = 0.35875 * (b * b - a * a) * 0.5;
    for (harmonic, coefficient) in [(1.0, -0.48829), (2.0, 0.14128), (3.0, -0.01168)] {
        let k = std::f64::consts::TAU * harmonic;
        area += coefficient * ((k * b).sin() - (k * a).sin()) / k;
        moment += coefficient
            * ((b * (k * b).sin() - a * (k * a).sin()) / k
                + ((k * b).cos() - (k * a).cos()) / (k * k));
    }
    (area, moment)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn constants_and_endpoints_preserved() {
        for sr in [44100.0, 48000.0, 96000.0] {
            for mode in [
                SpectrumBandwidth::None,
                SpectrumBandwidth::FixedHalfOctaves(1.0 / 6.0),
                SpectrumBandwidth::Erb,
            ] {
                let p = SpectrumSmoothingPlan::new(sr, 2048, 10.0, sr * 0.5, 257, mode);
                let mut out = vec![0.0; 257];
                p.apply_powers(&vec![0.25; 1025], &mut out);
                assert!(out.iter().all(|v| (*v + 6.0206).abs() < 0.0001));
            }
        }
        let p = SpectrumSmoothingPlan::new(48000.0, 64, 1000.0, 1000.0, 1, SpectrumBandwidth::Erb);
        let mut out = [0.0];
        p.apply_powers(&[0.0; 33], &mut out);
        assert_eq!(out, [MIN_DB]);
    }
    #[test]
    fn pixel_resampling_preserves_high_frequency_tone() {
        let p =
            SpectrumSmoothingPlan::new(48000.0, 32768, 20.0, 20000.0, 800, SpectrumBandwidth::None);
        let mut input = vec![0.0; 16385];
        input[12288] = 0.0625;
        let mut out = vec![MIN_DB; 800];
        p.apply_powers(&input, &mut out);
        assert!((out.into_iter().fold(MIN_DB, f32::max) + 12.0412).abs() < 0.0001);
    }
    #[test]
    fn narrow_tone_matches_independent_dense_integration() {
        let size = 32768;
        let sr = 48000.0;
        let mut input = vec![1e-12; size / 2 + 1];
        // Include an isolated HF peak and a sloping background to catch wrong phase,
        // row-boundary merging and dropped bins independently of the FFT engine.
        input[12288] = 0.0625;
        input[12287] = 0.02;
        input[12289] = 0.02;
        for mode in [
            SpectrumBandwidth::FixedHalfOctaves(1.0 / 48.0),
            SpectrumBandwidth::FixedHalfOctaves(1.0 / 6.0),
            SpectrumBandwidth::Erb,
        ] {
            let p = SpectrumSmoothingPlan::new(sr, size, 17000.0, 19000.0, 31, mode);
            let mut out = vec![0.0; 31];
            p.apply_powers(&input, &mut out);
            for (f, actual) in p.frequencies().iter().zip(out) {
                let f = *f as f64;
                let half = match mode {
                    SpectrumBandwidth::FixedHalfOctaves(h) => h as f64,
                    _ => (1.0 + 0.5 * 24.7 * (4.37 * f / 1000.0 + 1.0) / f).log2(),
                };
                let lo = f * 2.0_f64.powf(-half) * size as f64 / sr as f64;
                let hi = f * 2.0_f64.powf(half) * size as f64 / sr as f64;
                let mut sum = 0.0;
                let mut wsum = 0.0;
                for i in 0..65536 {
                    let t = (i as f64 + 0.5) / 65536.0;
                    let x = lo + (hi - lo) * t;
                    let b = x.floor() as usize;
                    let frac = x - b as f64;
                    let phase = std::f64::consts::TAU * t;
                    let w = 0.35875 - 0.48829 * phase.cos() + 0.14128 * (2.0 * phase).cos()
                        - 0.01168 * (3.0 * phase).cos();
                    sum += ((1.0 - frac) * input[b] as f64 + frac * input[b + 1] as f64) * w;
                    wsum += w;
                }
                let expected = 10.0 * (sum / wsum).max(1e-12).log10();
                assert!(
                    (actual as f64 - expected).abs() < 0.01,
                    "{mode:?} {f}: {actual} != {expected}"
                );
            }
        }
    }
}
