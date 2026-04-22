//! Unified stereo FFT analyser.
//!
//! Consolidates what used to be four independent mono `Analyzer`
//! instances (Mid / Side / L / R) plus the separate
//! `StereoCrossAnalyzer` into a single module that does the work
//! of all five with **just two FFTs per hop** (one for L, one for R).
//!
//! Given complex L_k and R_k:
//! * `M_k = (L_k + R_k) / 2`  →  |M|² for the Mid dB curve
//! * `S_k = (L_k − R_k) / 2`  →  |S|² for the Side dB curve
//! * `|L|²`, `|R|²`           →  L and R dB curves
//! * `Re(L_k · conj(R_k))`    →  numerator of the per-bin correlation
//!
//! Same Blackman–Harris window, same power-normalisation, same
//! asymmetric attack/release EMA as the existing mono `Analyzer`, so
//! swapping it in is a drop-in replacement for the averaged-curve
//! display path. Correlation gets its own symmetric smoother because
//! it wants a longer/steadier time constant than the peak-style curves.

use crate::{MIN_DB, blackman_harris_window, ms_to_alpha};
use realfft::{RealFftPlanner, RealToComplex};
use rustfft::num_complex::Complex;
use std::sync::Arc;

pub struct StereoAnalyzer {
    fft_size: usize,
    hop_size: usize,
    sample_rate: f32,
    fft: Arc<dyn RealToComplex<f32>>,
    window: Vec<f32>,
    window_sum: f32,
    ring_l: Vec<f32>,
    ring_r: Vec<f32>,
    ring_write_pos: usize,
    samples_since_last_fft: usize,
    // Real-input FFT plumbing. Windowed time-domain samples land in the
    // `_buf` halves (length fft_size), the complex spectrum comes out in
    // `_out` (length fft_size/2 + 1, positive half + Nyquist). The DC
    // bin and Nyquist bin are always purely real; the bins we actually
    // read (`[0..num_bins = fft_size/2]`) are all within the output.
    fft_scratch: Vec<Complex<f32>>,
    fft_buf_l: Vec<f32>,
    fft_buf_r: Vec<f32>,
    fft_out_l: Vec<Complex<f32>>,
    fft_out_r: Vec<Complex<f32>>,

    // Asymmetric attack/release EMA in the power domain, one
    // accumulator per displayed curve. Separate per-bin floors so
    // attacks register instantly while releases decay on a slower alpha.
    power_avg_mid: Vec<f32>,
    power_avg_side: Vec<f32>,
    power_avg_l: Vec<f32>,
    power_avg_r: Vec<f32>,
    attack_alpha: f32,
    release_alpha: f32,
    attack_ms: f32,
    release_ms: f32,

    // Symmetric 1-pole smoother in power / cross-power domain feeding
    // the per-bin signed correlation. Longer time constant than the
    // curves — a steady numeric readout, not a peak meter.
    corr_power_l: Vec<f32>,
    corr_power_r: Vec<f32>,
    corr_re_lr: Vec<f32>,
    corr_alpha: f32,
    corr_smooth_ms: f32,

    // Symmetric 1-pole smoother for L and R specifically used by the
    // L/R balance column. Same attack + release so values track the
    // CURRENT signal rather than peak-holding — otherwise a past L
    // transient would pin the balance line until something equal or
    // louder hit R.
    balance_power_l: Vec<f32>,
    balance_power_r: Vec<f32>,
    balance_alpha: f32,
    balance_smooth_ms: f32,

    mid_db: Vec<f32>,
    side_db: Vec<f32>,
    left_db: Vec<f32>,
    right_db: Vec<f32>,
    left_balance_db: Vec<f32>,
    right_balance_db: Vec<f32>,
    correlation: Vec<f32>,
}

impl StereoAnalyzer {
    pub fn new(sample_rate: f32, fft_size: usize) -> Self {
        assert!(
            fft_size.is_power_of_two() && fft_size >= 64,
            "fft_size must be a power of two and >= 64"
        );
        let hop_size = fft_size / 2;
        let mut planner = RealFftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(fft_size);
        let scratch_len = fft.get_scratch_len();
        let out_len = fft_size / 2 + 1;
        let window = blackman_harris_window(fft_size);
        let window_sum: f32 = window.iter().sum();
        // fft_size/2 + 1 = positive-half count *including* Nyquist (matches
        // the realfft output length). DC + Nyquist are single-sided; their
        // power is corrected in `compute_frame` so they read at the same
        // dB scale as the folded bins between them.
        let num_bins = fft_size / 2 + 1;

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
            fft_buf_l: vec![0.0; fft_size],
            fft_buf_r: vec![0.0; fft_size],
            fft_out_l: vec![Complex::new(0.0, 0.0); out_len],
            fft_out_r: vec![Complex::new(0.0, 0.0); out_len],
            power_avg_mid: vec![0.0; num_bins],
            power_avg_side: vec![0.0; num_bins],
            power_avg_l: vec![0.0; num_bins],
            power_avg_r: vec![0.0; num_bins],
            attack_alpha: 1.0,
            release_alpha: 1.0,
            attack_ms: 0.0,
            release_ms: 0.0,
            corr_power_l: vec![0.0; num_bins],
            corr_power_r: vec![0.0; num_bins],
            corr_re_lr: vec![0.0; num_bins],
            corr_alpha: 1.0,
            corr_smooth_ms: 0.0,
            balance_power_l: vec![0.0; num_bins],
            balance_power_r: vec![0.0; num_bins],
            balance_alpha: 1.0,
            balance_smooth_ms: 0.0,
            mid_db: vec![MIN_DB; num_bins],
            side_db: vec![MIN_DB; num_bins],
            left_db: vec![MIN_DB; num_bins],
            right_db: vec![MIN_DB; num_bins],
            left_balance_db: vec![MIN_DB; num_bins],
            right_balance_db: vec![MIN_DB; num_bins],
            correlation: vec![0.0; num_bins],
        }
    }

    pub fn set_overlap_ratio(&mut self, ratio: f32) {
        let ratio = ratio.clamp(0.0, 0.99);
        let hop = ((1.0 - ratio) * self.fft_size as f32).round() as usize;
        self.hop_size = hop.max(1);
        self.samples_since_last_fft = 0;
        self.recompute_alphas();
    }

    pub fn set_attack_release_ms(&mut self, attack_ms: f32, release_ms: f32) {
        self.attack_ms = attack_ms.max(0.0);
        self.release_ms = release_ms.max(0.0);
        self.recompute_alphas();
    }

    pub fn set_correlation_smoothing_ms(&mut self, ms: f32) {
        self.corr_smooth_ms = ms.max(0.0);
        self.recompute_alphas();
    }

    pub fn set_balance_smoothing_ms(&mut self, ms: f32) {
        self.balance_smooth_ms = ms.max(0.0);
        self.recompute_alphas();
    }

    fn recompute_alphas(&mut self) {
        let dt_s = self.hop_size as f32 / self.sample_rate.max(1.0);
        self.attack_alpha = ms_to_alpha(self.attack_ms, dt_s);
        self.release_alpha = ms_to_alpha(self.release_ms, dt_s);
        self.corr_alpha = ms_to_alpha(self.corr_smooth_ms, dt_s);
        self.balance_alpha = ms_to_alpha(self.balance_smooth_ms, dt_s);
    }

    pub fn num_bins(&self) -> usize {
        self.fft_size / 2 + 1
    }

    pub fn latest_mid_db(&self) -> &[f32] {
        &self.mid_db
    }

    pub fn latest_side_db(&self) -> &[f32] {
        &self.side_db
    }

    pub fn latest_left_db(&self) -> &[f32] {
        &self.left_db
    }

    pub fn latest_right_db(&self) -> &[f32] {
        &self.right_db
    }

    /// Symmetric-EMA smoothed L magnitude — no peak hold, tracks the
    /// current signal. Used by the L/R balance column so the displayed
    /// delta reflects the live relationship rather than a mix of stale
    /// peak-holds from two different moments.
    pub fn latest_left_balance_db(&self) -> &[f32] {
        &self.left_balance_db
    }

    pub fn latest_right_balance_db(&self) -> &[f32] {
        &self.right_balance_db
    }

    pub fn latest_correlation(&self) -> &[f32] {
        &self.correlation
    }

    pub fn reset(&mut self) {
        self.ring_l.fill(0.0);
        self.ring_r.fill(0.0);
        self.ring_write_pos = 0;
        self.samples_since_last_fft = 0;
        self.power_avg_mid.fill(0.0);
        self.power_avg_side.fill(0.0);
        self.power_avg_l.fill(0.0);
        self.power_avg_r.fill(0.0);
        self.corr_power_l.fill(0.0);
        self.corr_power_r.fill(0.0);
        self.corr_re_lr.fill(0.0);
        self.balance_power_l.fill(0.0);
        self.balance_power_r.fill(0.0);
        self.mid_db.fill(MIN_DB);
        self.side_db.fill(MIN_DB);
        self.left_db.fill(MIN_DB);
        self.right_db.fill(MIN_DB);
        self.left_balance_db.fill(MIN_DB);
        self.right_balance_db.fill(MIN_DB);
        self.correlation.fill(0.0);
    }

    /// Push a block of stereo samples. Returns `true` if at least
    /// one hop-aligned frame completed (and the `latest_*` outputs
    /// refreshed). Allocation-free after construction.
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
        let start = self.ring_write_pos;
        for i in 0..self.fft_size {
            let idx = (start + i) % self.fft_size;
            let w = self.window[i];
            self.fft_buf_l[i] = self.ring_l[idx] * w;
            self.fft_buf_r[i] = self.ring_r[idx] * w;
        }
        // realfft's `process_with_scratch` is infallible for the correct
        // input/output/scratch lengths, which we own — the only failure
        // mode is size mismatch and those sizes are fixed at construction.
        self.fft
            .process_with_scratch(
                &mut self.fft_buf_l,
                &mut self.fft_out_l,
                &mut self.fft_scratch,
            )
            .expect("realfft: input/output/scratch sizes are fixed");
        self.fft
            .process_with_scratch(
                &mut self.fft_buf_r,
                &mut self.fft_out_r,
                &mut self.fft_scratch,
            )
            .expect("realfft: input/output/scratch sizes are fixed");

        let norm = 2.0 / self.window_sum;
        let norm_sq = norm * norm;
        let attack_alpha = self.attack_alpha;
        let release_alpha = self.release_alpha;
        let corr_alpha = self.corr_alpha;
        let num_bins = self.num_bins();
        let last_bin = num_bins - 1; // Nyquist bin index (== fft_size/2)
        // 10^(MIN_DB/10) — anything below this in smoothed power is
        // treated as silence by the correlation block so noise-floor
        // bins settle at 0 instead of ±1.
        let floor_power = 10.0_f32.powf(MIN_DB * 0.1);

        for bin in 0..num_bins {
            let l = self.fft_out_l[bin];
            let r = self.fft_out_r[bin];
            // Mid/Side complex spectra are linear combinations of L and R
            // → the 0.5 scaling carries into the power as 0.25.
            let m_re = 0.5 * (l.re + r.re);
            let m_im = 0.5 * (l.im + r.im);
            let s_re = 0.5 * (l.re - r.re);
            let s_im = 0.5 * (l.im - r.im);

            // DC + Nyquist are single-sided: no negative-frequency twin to
            // fold in, so amplitude is /2 (power /4) of the folded bins.
            let single_sided = bin == 0 || bin == last_bin;
            let bin_norm_sq = if single_sided { norm_sq * 0.25 } else { norm_sq };
            let p_l = (l.re * l.re + l.im * l.im) * bin_norm_sq;
            let p_r = (r.re * r.re + r.im * r.im) * bin_norm_sq;
            let p_m = (m_re * m_re + m_im * m_im) * bin_norm_sq;
            let p_s = (s_re * s_re + s_im * s_im) * bin_norm_sq;
            let re_lr = (l.re * r.re + l.im * r.im) * bin_norm_sq;

            // Asymmetric EMA for the four dB curves.
            let (prev_m, prev_s, prev_l, prev_r) = (
                self.power_avg_mid[bin],
                self.power_avg_side[bin],
                self.power_avg_l[bin],
                self.power_avg_r[bin],
            );
            let a_m = if p_m > prev_m { attack_alpha } else { release_alpha };
            let a_s = if p_s > prev_s { attack_alpha } else { release_alpha };
            let a_l = if p_l > prev_l { attack_alpha } else { release_alpha };
            let a_r = if p_r > prev_r { attack_alpha } else { release_alpha };
            self.power_avg_mid[bin] = a_m * p_m + (1.0 - a_m) * prev_m;
            self.power_avg_side[bin] = a_s * p_s + (1.0 - a_s) * prev_s;
            self.power_avg_l[bin] = a_l * p_l + (1.0 - a_l) * prev_l;
            self.power_avg_r[bin] = a_r * p_r + (1.0 - a_r) * prev_r;

            self.mid_db[bin] = 10.0 * (self.power_avg_mid[bin] + 1e-24).log10();
            self.side_db[bin] = 10.0 * (self.power_avg_side[bin] + 1e-24).log10();
            self.left_db[bin] = 10.0 * (self.power_avg_l[bin] + 1e-24).log10();
            self.right_db[bin] = 10.0 * (self.power_avg_r[bin] + 1e-24).log10();

            // Symmetric smoother for the correlation numerator +
            // denominator. Using a separate EMA from the curves lets
            // us keep the curves snappy (fast attack) without the
            // correlation readout chattering on transients.
            self.corr_power_l[bin] =
                corr_alpha * p_l + (1.0 - corr_alpha) * self.corr_power_l[bin];
            self.corr_power_r[bin] =
                corr_alpha * p_r + (1.0 - corr_alpha) * self.corr_power_r[bin];
            self.corr_re_lr[bin] =
                corr_alpha * re_lr + (1.0 - corr_alpha) * self.corr_re_lr[bin];

            // Dedicated symmetric smoother for the L/R balance column.
            // Faster TC than correlation so the balance line feels live,
            // but still symmetric so transients don't peak-hold a skew.
            let balance_alpha = self.balance_alpha;
            self.balance_power_l[bin] =
                balance_alpha * p_l + (1.0 - balance_alpha) * self.balance_power_l[bin];
            self.balance_power_r[bin] =
                balance_alpha * p_r + (1.0 - balance_alpha) * self.balance_power_r[bin];
            self.left_balance_db[bin] =
                10.0 * (self.balance_power_l[bin] + 1e-24).log10();
            self.right_balance_db[bin] =
                10.0 * (self.balance_power_r[bin] + 1e-24).log10();

            let denom_sq = self.corr_power_l[bin] * self.corr_power_r[bin];
            self.correlation[bin] = if self.corr_power_l[bin] < floor_power
                || self.corr_power_r[bin] < floor_power
                || denom_sq <= 1e-30
            {
                0.0
            } else {
                (self.corr_re_lr[bin] / denom_sq.sqrt()).clamp(-1.0, 1.0)
            };
        }
    }
}
