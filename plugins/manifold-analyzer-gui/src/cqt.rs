//! Brown-Puckette constant-Q / variable-Q transform.
//!
//! CQT is the academic-grade answer to "how do I match human hearing's
//! time-frequency trade-off across the whole audible range?". Every CQT
//! bin has the same Q (= center_freq / bandwidth), so low-frequency bins
//! use long windows (tight freq resolution, coarse time resolution) and
//! high-frequency bins use short windows (coarse freq resolution, tight
//! time resolution). Professional spectrograms (iZotope RX, Sonic
//! Visualiser, librosa) are built on this.
//!
//! We generalise to **VQT** (Schörkhuber & Klapuri 2014, "Matlab toolbox
//! for efficient CQT/VQT"): each bin's bandwidth is
//! `bandwidth(f) = α · f + γ`,
//! where `α = 1/Q_high` is the asymptotic high-freq Q-inverse and `γ` is
//! a constant-bandwidth floor that prevents bass windows from growing
//! absurdly long. `γ = 0` reduces to classical CQT; `γ ≈ 20 Hz` gives a
//! balanced hybrid that reads bass transients with ~50 ms time
//! resolution instead of ~1 s while preserving pitch tightness in the
//! mids and highs. This matches how human hearing actually responds
//! (the ERB curve is roughly `0.108·f + 24.7`).
//!
//! Brown & Puckette 1992 give the fast algorithm:
//! 1. Pre-compute time-domain kernels `g_k[n] = w[n] · exp(+i·2π·f_k·n/sr)`
//!    with per-bin window length `N_k = sr / bandwidth(f_k)`,
//!    Blackman-Harris window.
//! 2. Zero-pad each kernel to the shared FFT length `N_fft` and FFT it.
//! 3. Conjugate + normalise → spectral kernel `K_k[m]`. These are sparse
//!    because a narrow-band complex exp FFTs to a concentrated support;
//!    threshold small entries and store the rest in CSR format.
//! 4. At runtime: `Y = FFT(audio)`; for each bin,
//!    `VQT[k] = Σ Y[m] · K_k[m]` over the sparse support. `|VQT[k]|²`
//!    is the power at `f_k`.
//!
//! Per-column cost is dominated by one shared FFT plus a handful of
//! complex mul-adds per bin — typically ~0.5 ms total on Apple Silicon.

use manifold_analyzer_dsp::blackman_harris_window;
use rustfft::{Fft, FftPlanner, num_complex::Complex};
use std::sync::Arc;

/// Sparse kernel + FFT state. Stateless with respect to the audio
/// stream — feed it one N_fft-sample segment at a time.
pub struct CqtTransform {
    n_fft: usize,
    fft: Arc<dyn Fft<f32>>,
    fft_scratch: Vec<Complex<f32>>,
    fft_buffer: Vec<Complex<f32>>,
    // CSR-style sparse kernel matrix. Row k spans indices
    // `[row_ptr[k], row_ptr[k+1])` of `col_idx` and `coef`.
    row_ptr: Vec<usize>,
    col_idx: Vec<u32>,
    coef: Vec<Complex<f32>>,
    num_bins: usize,
    #[allow(dead_code)] // diagnostic accessor
    center_freqs: Vec<f32>,
}

impl CqtTransform {
    /// Build a VQT transform. Kernel construction runs one FFT per bin,
    /// so this is the expensive step — do it once up front (e.g. at
    /// renderer init / sample-rate change).
    ///
    /// * `bpo` — bins per octave. 24 = 2/semitone is good spectrogram density.
    /// * `gamma_hz` — bandwidth floor. `0.0` = classical CQT (long bass
    ///   windows); `20.0` = practical hybrid; `ERB(f) ≈ 0.108·f + 24.7` for
    ///   a perceptual match.
    /// * `threshold_rel` — prunes kernel entries below
    ///   `threshold_rel · max_entry` per row; 0.005 is conservative.
    pub fn new(
        sample_rate: f32,
        n_fft: usize,
        fmin: f32,
        fmax: f32,
        bpo: usize,
        gamma_hz: f32,
        threshold_rel: f32,
    ) -> Self {
        assert!(n_fft.is_power_of_two(), "n_fft must be a power of two");
        assert!(fmax > fmin && fmin > 0.0, "need fmax > fmin > 0");
        assert!(bpo > 0);
        assert!(gamma_hz >= 0.0);
        assert!((0.0..1.0).contains(&threshold_rel));

        let num_bins = (bpo as f32 * (fmax / fmin).log2()).floor() as usize;
        assert!(num_bins > 0);

        // α is the inverse of the asymptotic high-freq Q. With classical
        // CQT Q = 1/(2^(1/bpo) − 1); we keep that as the high-freq limit
        // so tones stay pitch-sharp above the γ transition.
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

        let mut row_ptr = Vec::with_capacity(num_bins + 1);
        row_ptr.push(0);
        let mut col_idx: Vec<u32> = Vec::new();
        let mut coef: Vec<Complex<f32>> = Vec::new();

        let n_fft_inv = 1.0 / n_fft as f32;
        let two_pi = 2.0 * std::f32::consts::PI;

        for &f_k in &center_freqs {
            // Variable-Q bandwidth. At high freq `α·f_k` dominates
            // (constant Q); at low freq `gamma_hz` floors the bandwidth
            // (constant bandwidth) so bass windows stay tractable.
            let bandwidth = alpha * f_k + gamma_hz;
            let n_k_ideal = (sample_rate / bandwidth).ceil() as usize;
            let n_k = n_k_ideal.min(n_fft).max(4);

            let w = blackman_harris_window(n_k);
            let w_sum: f32 = w.iter().sum();
            // Normalise so that a unit-amplitude sinusoid at f_k yields
            // |CQT[k]| = 1. Derivation: for x[n] = cos(2π f_k n / sr),
            // <x, g_k> ≈ 0.5 · Σ w[n], so we scale by 2/Σw.
            let scale = 2.0 / w_sum;

            // Time-domain kernel: g_k[n] = w[n] · exp(+i 2π f_k n / sr),
            // left-aligned in the n_fft-length FFT buffer.
            for c in kernel_buf.iter_mut() {
                *c = Complex::new(0.0, 0.0);
            }
            for n in 0..n_k {
                let phase = two_pi * f_k * n as f32 / sample_rate;
                let (s, c) = phase.sin_cos();
                let wn = w[n] * scale;
                kernel_buf[n] = Complex::new(wn * c, wn * s);
            }

            // FFT gives G_k. Spectral kernel K_k[m] = conj(G_k[m]) / n_fft
            // (from Parseval: <a, b> = (1/N) Σ A[m] · conj(B[m])).
            fft.process_with_scratch(&mut kernel_buf, &mut fft_scratch);
            for c in kernel_buf.iter_mut() {
                *c = c.conj() * n_fft_inv;
            }

            // Sparsify: threshold relative to the row's peak magnitude.
            let max_abs = kernel_buf
                .iter()
                .map(|c| c.norm())
                .fold(0.0f32, f32::max);
            let cutoff = max_abs * threshold_rel;
            for m in 0..n_fft {
                if kernel_buf[m].norm() >= cutoff {
                    col_idx.push(m as u32);
                    coef.push(kernel_buf[m]);
                }
            }
            row_ptr.push(col_idx.len());
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

    #[allow(dead_code)] // diagnostic accessor
    pub fn n_fft(&self) -> usize {
        self.n_fft
    }

    #[allow(dead_code)] // diagnostic accessor
    pub fn center_freqs(&self) -> &[f32] {
        &self.center_freqs
    }

    /// Fraction of kernel entries that survived sparsification (diagnostic).
    #[allow(dead_code)]
    pub fn density(&self) -> f32 {
        let stored = self.coef.len() as f32;
        let dense = (self.num_bins * self.n_fft) as f32;
        stored / dense
    }

    /// Transform one N_fft-sample audio segment into CQT power (dB) per
    /// bin. `audio.len() == n_fft` and `output_db.len() == num_bins`.
    pub fn process(&mut self, audio: &[f32], output_db: &mut [f32]) {
        assert_eq!(audio.len(), self.n_fft);
        assert_eq!(output_db.len(), self.num_bins);

        // Real audio → complex FFT buffer (imaginary = 0).
        for (dst, &s) in self.fft_buffer.iter_mut().zip(audio.iter()) {
            *dst = Complex::new(s, 0.0);
        }
        self.fft
            .process_with_scratch(&mut self.fft_buffer, &mut self.fft_scratch);

        for k in 0..self.num_bins {
            let lo = self.row_ptr[k];
            let hi = self.row_ptr[k + 1];
            let mut acc = Complex::new(0.0f32, 0.0f32);
            for idx in lo..hi {
                let m = self.col_idx[idx] as usize;
                acc += self.fft_buffer[m] * self.coef[idx];
            }
            let power = acc.norm_sqr();
            output_db[k] = 10.0 * (power + 1e-24).log10();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_sine_reads_near_zero_db_at_its_bin_cqt() {
        let sr = 48000.0;
        let n_fft = 16384;
        let fmin = 100.0;
        let fmax = 8000.0;
        let bpo = 24;
        let mut cqt = CqtTransform::new(sr, n_fft, fmin, fmax, bpo, 0.0, 0.005);

        let target_freq = 1000.0;
        let audio: Vec<f32> = (0..n_fft)
            .map(|n| (2.0 * std::f32::consts::PI * target_freq * n as f32 / sr).cos())
            .collect();

        let mut out = vec![0.0; cqt.num_bins()];
        cqt.process(&audio, &mut out);

        let (peak_bin, peak_db) = out
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
            .map(|(i, &v)| (i, v))
            .unwrap();
        let peak_freq = cqt.center_freqs()[peak_bin];
        assert!(
            (peak_freq - target_freq).abs() / target_freq < 0.05,
            "peak at {peak_freq} Hz, expected {target_freq}"
        );
        assert!(
            peak_db > -2.0 && peak_db < 2.0,
            "peak {peak_db} dB, expected near 0 dB"
        );
    }

    #[test]
    fn silence_reads_floor() {
        let sr = 48000.0;
        let mut cqt = CqtTransform::new(sr, 8192, 100.0, 4000.0, 12, 0.0, 0.005);
        let audio = vec![0.0; cqt.n_fft()];
        let mut out = vec![0.0; cqt.num_bins()];
        cqt.process(&audio, &mut out);
        for db in &out {
            assert!(*db < -100.0, "silence read {db} dB");
        }
    }

    #[test]
    fn vqt_low_bin_still_reads_unit_sine() {
        // Classical CQT at 50 Hz (Q ≈ 17 at bpo=12) wants a 16 000-sample
        // window — doesn't fit an 8192 FFT. VQT with γ = 20 Hz floors the
        // bandwidth so the low bins stay valid in a modest N_fft.
        let sr = 48000.0;
        let n_fft = 8192;
        let mut cqt = CqtTransform::new(sr, n_fft, 20.0, 1000.0, 12, 20.0, 0.005);
        let target = 50.0_f32;
        let audio: Vec<f32> = (0..n_fft)
            .map(|n| (2.0 * std::f32::consts::PI * target * n as f32 / sr).cos())
            .collect();
        let mut out = vec![0.0; cqt.num_bins()];
        cqt.process(&audio, &mut out);
        let peak_db = out.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
        assert!(peak_db > -3.0, "VQT peak {peak_db} dB, expected near 0");
    }
}
