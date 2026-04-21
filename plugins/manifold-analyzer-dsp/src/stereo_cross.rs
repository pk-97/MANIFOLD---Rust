//! Per-bin stereo correlation. Windowed FFT of L and R, then a
//! 1-pole smoother in the power / cross-power domain yields a
//! signed correlation value per frequency bin:
//!
//! ```text
//! ρ(bin) = Re{Σ L · R̄} / √(Σ|L|² · Σ|R|²)
//! ```
//!
//! Range is `[−1, +1]`: +1 = phase-aligned mono, 0 = uncorrelated,
//! −1 = polarity-flipped. Matches the whole-band "Correlation"
//! readout already in `LoudnessMeter`, just resolved per bin so the
//! analyzer can show which frequencies are mono-safe vs out of phase.
//!
//! Pure DSP, no GUI glue. Mirrors `Analyzer`'s hop-driven FFT pipeline
//! — same BH window, same overlap convention — so the bin centres and
//! bandwidths line up with the magnitude spectrum the user is already
//! reading.

use crate::{MIN_DB, blackman_harris_window, ms_to_alpha};
use rustfft::{Fft, FftPlanner, num_complex::Complex};
use std::sync::Arc;

pub struct StereoCrossAnalyzer {
    fft_size: usize,
    hop_size: usize,
    sample_rate: f32,
    fft: Arc<dyn Fft<f32>>,
    window: Vec<f32>,
    window_sum: f32,
    ring_l: Vec<f32>,
    ring_r: Vec<f32>,
    ring_write_pos: usize,
    samples_since_last_fft: usize,
    fft_scratch: Vec<Complex<f32>>,
    fft_buf_l: Vec<Complex<f32>>,
    fft_buf_r: Vec<Complex<f32>>,
    // 1-pole smoothed power / cross-power per bin. Smoothing happens
    // in the linear (power) domain — averaging the log-magnitude would
    // bias the result and never settle near ±1 for quiet tonal
    // content.
    smoothed_power_l: Vec<f32>,
    smoothed_power_r: Vec<f32>,
    smoothed_re_lr: Vec<f32>,
    // Latest per-bin correlation in [-1, 1]. Bins below the power
    // floor map to 0 (neutral) rather than whatever the tiny division
    // would yield.
    correlation: Vec<f32>,
    smoothing_ms: f32,
    alpha: f32,
}

impl StereoCrossAnalyzer {
    pub fn new(sample_rate: f32, fft_size: usize) -> Self {
        assert!(
            fft_size.is_power_of_two() && fft_size >= 64,
            "fft_size must be a power of two and >= 64"
        );
        let hop_size = fft_size / 2;
        let mut planner = FftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(fft_size);
        let scratch_len = fft.get_inplace_scratch_len();
        let window = blackman_harris_window(fft_size);
        let window_sum: f32 = window.iter().sum();
        let num_bins = fft_size / 2;

        Self {
            fft_size,
            hop_size,
            sample_rate,
            fft,
            window,
            window_sum,
            ring_l: vec![0.0; fft_size],
            ring_r: vec![0.0; fft_size],
            ring_write_pos: 0,
            samples_since_last_fft: 0,
            fft_scratch: vec![Complex::new(0.0, 0.0); scratch_len],
            fft_buf_l: vec![Complex::new(0.0, 0.0); fft_size],
            fft_buf_r: vec![Complex::new(0.0, 0.0); fft_size],
            smoothed_power_l: vec![0.0; num_bins],
            smoothed_power_r: vec![0.0; num_bins],
            smoothed_re_lr: vec![0.0; num_bins],
            correlation: vec![0.0; num_bins],
            smoothing_ms: 0.0,
            alpha: 1.0,
        }
    }

    pub fn set_overlap_ratio(&mut self, ratio: f32) {
        let ratio = ratio.clamp(0.0, 0.99);
        let hop = ((1.0 - ratio) * self.fft_size as f32).round() as usize;
        self.hop_size = hop.max(1);
        self.samples_since_last_fft = 0;
        self.recompute_alpha();
    }

    /// Smoothing time constant (ms) for the 1-pole average of
    /// per-bin power and cross-power. Longer = steadier numbers,
    /// shorter = more responsive. 500 ms is a sensible default for
    /// a visual readout.
    pub fn set_smoothing_ms(&mut self, ms: f32) {
        self.smoothing_ms = ms.max(0.0);
        self.recompute_alpha();
    }

    fn recompute_alpha(&mut self) {
        let dt_s = self.hop_size as f32 / self.sample_rate.max(1.0);
        self.alpha = ms_to_alpha(self.smoothing_ms, dt_s);
    }

    pub fn num_bins(&self) -> usize {
        self.fft_size / 2
    }

    pub fn latest_correlation(&self) -> &[f32] {
        &self.correlation
    }

    pub fn reset(&mut self) {
        self.ring_l.fill(0.0);
        self.ring_r.fill(0.0);
        self.ring_write_pos = 0;
        self.samples_since_last_fft = 0;
        self.smoothed_power_l.fill(0.0);
        self.smoothed_power_r.fill(0.0);
        self.smoothed_re_lr.fill(0.0);
        self.correlation.fill(0.0);
    }

    /// Push a block of stereo samples. Returns `true` if at least
    /// one hop-aligned frame completed and `correlation` was
    /// refreshed. Allocation-free after construction.
    pub fn push_stereo(&mut self, left: &[f32], right: &[f32]) -> bool {
        let n = left.len().min(right.len());
        let mut new_frame = false;
        for i in 0..n {
            self.ring_l[self.ring_write_pos] = left[i];
            self.ring_r[self.ring_write_pos] = right[i];
            self.ring_write_pos = (self.ring_write_pos + 1) % self.fft_size;
            self.samples_since_last_fft += 1;
            if self.samples_since_last_fft >= self.hop_size {
                self.samples_since_last_fft = 0;
                self.compute_frame();
                new_frame = true;
            }
        }
        new_frame
    }

    fn compute_frame(&mut self) {
        // Copy the ring (oldest-first) into the two FFT buffers,
        // windowed. Imag part is 0 — real-valued inputs.
        let start = self.ring_write_pos;
        for i in 0..self.fft_size {
            let idx = (start + i) % self.fft_size;
            let w = self.window[i];
            self.fft_buf_l[i] = Complex::new(self.ring_l[idx] * w, 0.0);
            self.fft_buf_r[i] = Complex::new(self.ring_r[idx] * w, 0.0);
        }
        self.fft
            .process_with_scratch(&mut self.fft_buf_l, &mut self.fft_scratch);
        self.fft
            .process_with_scratch(&mut self.fft_buf_r, &mut self.fft_scratch);

        // Per-bin single-sided power / cross-power, normalised the
        // same way the mono `Analyzer` does so power values are
        // comparable across modules.
        let norm_sq = (2.0 / self.window_sum).powi(2);
        let alpha = self.alpha;
        let num_bins = self.num_bins();
        for bin in 0..num_bins {
            let l = self.fft_buf_l[bin];
            let r = self.fft_buf_r[bin];
            // Re(L · conj(R)) = L.re*R.re + L.im*R.im.
            // Power products are positive so no cancellation concerns.
            let p_l = (l.re * l.re + l.im * l.im) * norm_sq;
            let p_r = (r.re * r.re + r.im * r.im) * norm_sq;
            let re_lr = (l.re * r.re + l.im * r.im) * norm_sq;
            self.smoothed_power_l[bin] =
                alpha * p_l + (1.0 - alpha) * self.smoothed_power_l[bin];
            self.smoothed_power_r[bin] =
                alpha * p_r + (1.0 - alpha) * self.smoothed_power_r[bin];
            self.smoothed_re_lr[bin] =
                alpha * re_lr + (1.0 - alpha) * self.smoothed_re_lr[bin];

            // Sanity floor: anything below a −120 dB power is treated
            // as "no signal" and reported as 0 correlation instead of
            // whatever the tiny division gave us. Otherwise noise-
            // floor bins flicker between ±1 uselessly.
            let floor_power = 10.0_f32.powf(MIN_DB * 0.1);
            let denom_sq = self.smoothed_power_l[bin] * self.smoothed_power_r[bin];
            self.correlation[bin] = if self.smoothed_power_l[bin] < floor_power
                || self.smoothed_power_r[bin] < floor_power
                || denom_sq <= 1e-30
            {
                0.0
            } else {
                (self.smoothed_re_lr[bin] / denom_sq.sqrt()).clamp(-1.0, 1.0)
            };
        }
    }
}
