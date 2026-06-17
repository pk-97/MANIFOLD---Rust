//! Brown-Puckette constant-Q / variable-Q transform (CPU).
//!
//! Ported from the Manifold Analyzer VST's `cqt.rs`, trimmed to the CPU path —
//! Manifold's spectrogram runs the transform for a *single* calibration send at
//! a modest hop, where one shared FFT plus a sparse mat-vec per hop is
//! sub-millisecond and keeps `manifold-audio` free of any GPU dependency. (The
//! Analyzer's GPU pipeline exists for a 65536-pt FFT at ~188 cols/s across a
//! full mix; we don't need it here.)
//!
//! Every CQT bin has the same Q (= center_freq / bandwidth): low bins use long
//! windows (tight freq, coarse time), high bins short windows (coarse freq,
//! tight time). We generalise to **VQT** (Schörkhuber & Klapuri 2014): each
//! bin's bandwidth is `α·f + γ`, where `α = 2^(1/bpo) − 1` is the asymptotic
//! high-freq Q-inverse and `γ` is a constant-bandwidth floor that stops bass
//! windows growing absurdly long. `γ = 0` is classical CQT; `γ ≈ 20 Hz` reads
//! bass transients in ~50 ms while keeping mid/high pitch tight.
//!
//! Algorithm (Brown & Puckette 1992): pre-compute time-domain kernels
//! `g_k[n] = w[n]·exp(+i·2π·f_k·n/sr)` (Blackman-Harris window, per-bin length
//! `N_k = sr/bandwidth(f_k)`), FFT each to a sparse spectral kernel `K_k`, then
//! per hop `VQT[k] = Σ Y[m]·K_k[m]` over the sparse support where `Y = FFT(audio)`.

use rustfft::num_complex::Complex;
use rustfft::{Fft, FftPlanner};
use std::sync::Arc;

use crate::window::blackman_harris_window;

/// Sparse kernel + FFT state. Stateless w.r.t. the audio stream — feed it one
/// `n_fft`-sample segment at a time via [`CqtTransform::process_magnitudes`].
pub struct CqtTransform {
    n_fft: usize,
    fft: Arc<dyn Fft<f32>>,
    fft_scratch: Vec<Complex<f32>>,
    fft_buffer: Vec<Complex<f32>>,
    // CSR sparse kernel matrix: row k spans `[row_ptr[k], row_ptr[k+1])` of
    // `col_idx`/`coef`.
    row_ptr: Vec<u32>,
    col_idx: Vec<u32>,
    coef: Vec<Complex<f32>>,
    num_bins: usize,
    center_freqs: Vec<f32>,
}

impl CqtTransform {
    /// Build a VQT transform. Kernel construction runs one FFT per bin, so this
    /// is the expensive step — do it once at capture/sample-rate change.
    ///
    /// * `bpo` — bins per octave (24 = 2/semitone, good spectrogram density).
    /// * `gamma_lo_hz` / `gamma_hi_hz` / `gamma_transition_hz` — frequency-ramped
    ///   bandwidth floor: `γ(f) = lo + (hi − lo)·min(1, f/transition)`. A smaller
    ///   γ in deep bass grows the kernel longer (finer bass resolution, kills the
    ///   2f ripple on sub sines) while the larger γ above the knee keeps mids and
    ///   highs as they were. `lo == hi` reduces to a constant γ; `0` → classical CQT.
    /// * `min_kernel_len` — per-bin kernel floor in samples; keeps HF bandwidth
    ///   narrow enough to resolve closely-spaced partials.
    /// * `threshold_rel` — prunes kernel entries below `threshold_rel·max_entry`
    ///   per row; 0.005 is conservative.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        sample_rate: f32,
        n_fft: usize,
        fmin: f32,
        fmax: f32,
        bpo: usize,
        gamma_lo_hz: f32,
        gamma_hi_hz: f32,
        gamma_transition_hz: f32,
        min_kernel_len: usize,
        threshold_rel: f32,
    ) -> Self {
        assert!(n_fft.is_power_of_two(), "n_fft must be a power of two");
        assert!(fmax > fmin && fmin > 0.0, "need fmax > fmin > 0");
        assert!(bpo > 0);
        assert!(gamma_lo_hz >= 0.0 && gamma_hi_hz >= 0.0);
        assert!(gamma_transition_hz > 0.0);
        assert!((0.0..1.0).contains(&threshold_rel));

        // γ(f) = lo + (hi − lo)·min(1, f/transition). Matches the Analyzer VST.
        let gamma_at = |f: f32| -> f32 {
            let t = (f / gamma_transition_hz).clamp(0.0, 1.0);
            gamma_lo_hz + (gamma_hi_hz - gamma_lo_hz) * t
        };

        let num_bins = num_bins(fmin, fmax, bpo);
        assert!(num_bins > 0);

        // α is the inverse of the asymptotic high-freq Q. Classical CQT has
        // Q = 1/(2^(1/bpo) − 1); we keep that as the high-freq limit so tones
        // stay pitch-sharp.
        let alpha = 2.0_f32.powf(1.0 / bpo as f32) - 1.0;

        let mut center_freqs = Vec::with_capacity(num_bins);
        for k in 0..num_bins {
            center_freqs.push(fmin * 2.0_f32.powf(k as f32 / bpo as f32));
        }

        let mut planner = FftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(n_fft);
        let scratch_len = fft.get_inplace_scratch_len();
        let mut fft_scratch = vec![Complex::new(0.0, 0.0); scratch_len];
        let mut kernel_buf = vec![Complex::new(0.0, 0.0); n_fft];

        let mut row_ptr: Vec<u32> = Vec::with_capacity(num_bins + 1);
        row_ptr.push(0);
        let mut col_idx: Vec<u32> = Vec::new();
        let mut coef: Vec<Complex<f32>> = Vec::new();

        let n_fft_inv = 1.0 / n_fft as f32;
        let two_pi = 2.0 * std::f32::consts::PI;

        for &f_k in &center_freqs {
            // Variable-Q bandwidth: at high freq `α·f_k` dominates (constant Q);
            // below the transition γ ramps down so deep-bass windows grow long
            // enough to fit several cycles.
            let bandwidth = alpha * f_k + gamma_at(f_k);
            let n_k_ideal = (sample_rate / bandwidth).ceil() as usize;
            let n_k = n_k_ideal.min(n_fft).max(min_kernel_len).max(4);

            let w = blackman_harris_window(n_k);
            let w_sum: f32 = w.iter().sum();
            // Normalise so a unit-amplitude sinusoid at f_k yields |VQT[k]| = 1.
            // (<x, g_k> ≈ 0.5·Σw for x = cos, so scale by 2/Σw.)
            let scale = 2.0 / w_sum;

            // Time-domain kernel right-aligned in the n_fft buffer, so each
            // kernel samples the NEWEST N_k samples (the column is "as of now").
            for c in kernel_buf.iter_mut() {
                *c = Complex::new(0.0, 0.0);
            }
            let start = n_fft - n_k;
            for n in 0..n_k {
                let phase = two_pi * f_k * n as f32 / sample_rate;
                let (s, c) = phase.sin_cos();
                let wn = w[n] * scale;
                kernel_buf[start + n] = Complex::new(wn * c, wn * s);
            }

            // FFT → G_k; spectral kernel K_k[m] = conj(G_k[m]) / n_fft.
            fft.process_with_scratch(&mut kernel_buf, &mut fft_scratch);
            for c in kernel_buf.iter_mut() {
                *c = c.conj() * n_fft_inv;
            }

            // Sparsify: threshold relative to the row's peak magnitude.
            let max_abs = kernel_buf.iter().map(|c| c.norm()).fold(0.0f32, f32::max);
            let cutoff = max_abs * threshold_rel;
            for (m, &entry) in kernel_buf.iter().enumerate() {
                if entry.norm() >= cutoff {
                    col_idx.push(m as u32);
                    coef.push(entry);
                }
            }
            row_ptr.push(col_idx.len() as u32);
        }

        Self {
            n_fft,
            fft,
            fft_scratch,
            fft_buffer: vec![Complex::new(0.0, 0.0); n_fft],
            row_ptr,
            col_idx,
            coef,
            num_bins,
            center_freqs,
        }
    }

    pub fn num_bins(&self) -> usize {
        self.num_bins
    }

    pub fn n_fft(&self) -> usize {
        self.n_fft
    }

    /// Bin center frequencies (Hz), geometrically spaced — bin index is linear
    /// in log-frequency, which the waterfall's y-axis maps directly.
    pub fn center_freqs(&self) -> &[f32] {
        &self.center_freqs
    }

    /// Transform one `n_fft`-sample audio segment into per-bin **magnitudes**
    /// (`|VQT[k]|`, normalised so a unit sine at a bin reads ≈ 1.0). Writes
    /// `num_bins` values into `out`.
    pub fn process_magnitudes(&mut self, audio: &[f32], out: &mut [f32]) {
        assert_eq!(audio.len(), self.n_fft);
        assert_eq!(out.len(), self.num_bins);

        for (dst, &s) in self.fft_buffer.iter_mut().zip(audio.iter()) {
            *dst = Complex::new(s, 0.0);
        }
        self.fft
            .process_with_scratch(&mut self.fft_buffer, &mut self.fft_scratch);

        for (k, out_k) in out.iter_mut().enumerate().take(self.num_bins) {
            let lo = self.row_ptr[k] as usize;
            let hi = self.row_ptr[k + 1] as usize;
            let mut acc = Complex::new(0.0f32, 0.0f32);
            for idx in lo..hi {
                let m = self.col_idx[idx] as usize;
                acc += self.fft_buffer[m] * self.coef[idx];
            }
            *out_k = acc.norm();
        }
    }
}

/// Number of VQT bins for a frequency range: `floor(bpo · log2(fmax/fmin))`.
/// The renderer and the worker call this so they agree on column length without
/// constructing a transform.
pub fn num_bins(fmin: f32, fmax: f32, bpo: usize) -> usize {
    (bpo as f32 * (fmax / fmin).log2()).floor() as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: f32 = 48_000.0;

    fn sine(freq: f32, n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| (std::f32::consts::TAU * freq * i as f32 / SR).cos())
            .collect()
    }

    #[test]
    fn unit_sine_peaks_near_its_bin_at_unity() {
        let n_fft = 8192;
        let mut cqt = CqtTransform::new(SR, n_fft, 50.0, 8000.0, 24, 10.0, 20.0, 200.0, 256, 0.005);
        let mut out = vec![0.0; cqt.num_bins()];
        cqt.process_magnitudes(&sine(1000.0, n_fft), &mut out);

        let (peak_bin, &peak) = out
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
            .unwrap();
        let peak_freq = cqt.center_freqs()[peak_bin];
        assert!(
            (peak_freq - 1000.0).abs() / 1000.0 < 0.05,
            "peak at {peak_freq} Hz, expected 1000"
        );
        // Normalised so a unit sine reads ≈ 1.0 at its bin.
        assert!((peak - 1.0).abs() < 0.2, "peak magnitude {peak}, expected ≈1.0");
    }

    #[test]
    fn silence_reads_near_zero() {
        let n_fft = 8192;
        let mut cqt = CqtTransform::new(SR, n_fft, 50.0, 8000.0, 12, 10.0, 20.0, 200.0, 256, 0.005);
        let mut out = vec![0.0; cqt.num_bins()];
        cqt.process_magnitudes(&vec![0.0; n_fft], &mut out);
        assert!(out.iter().all(|&v| v < 1e-5), "silence should be ~0");
    }

    #[test]
    fn num_bins_matches_formula() {
        assert_eq!(num_bins(50.0, 800.0, 12), 48); // 4 octaves * 12
    }
}
