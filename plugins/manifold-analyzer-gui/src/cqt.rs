//! Brown-Puckette constant-Q / variable-Q transform.
//!
//! CQT is the academic-grade answer to "how do I match human hearing's
//! time-frequency trade-off across the whole audible range?". Every CQT
//! bin has the same Q (= center_freq / bandwidth), so low-frequency bins
//! use long windows (tight freq resolution, coarse time resolution) and
//! high-frequency bins use short windows (coarse freq resolution, tight
//! time resolution). Most academic-grade spectrogram tools (e.g. the
//! `librosa` reference implementation) are built on this.
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

pub use rustfft::num_complex::Complex as CqtComplex;

/// Sparse kernel + FFT state. Stateless with respect to the audio
/// stream — feed it one N_fft-sample segment at a time.
pub struct CqtTransform {
    n_fft: usize,
    // `fft` is used at construction to FFT each time-domain kernel into
    // its spectral form. The per-hop forward transform now runs on the
    // GPU via `GpuFft` + `gpu_cqt::GpuCqt`; this field plus the two
    // scratch buffers stay live so the CPU-side `process_complex` path
    // (exercised by unit tests and available as an offline fallback)
    // keeps working.
    #[allow(dead_code)]
    fft: Arc<dyn Fft<f32>>,
    #[allow(dead_code)]
    fft_scratch: Vec<Complex<f32>>,
    #[allow(dead_code)]
    fft_buffer: Vec<Complex<f32>>,
    // CSR-style sparse kernel matrix. Row k spans indices
    // `[row_ptr[k], row_ptr[k+1])` of `col_idx` and `coef`. `row_ptr`
    // is stored as u32 (not usize) so the GPU compute shader can read
    // the buffer as-is without a conversion pass; the ceiling is
    // num_bins × n_fft ≈ 17M nonzeros, well under u32::MAX.
    row_ptr: Vec<u32>,
    col_idx: Vec<u32>,
    coef: Vec<Complex<f32>>,
    num_bins: usize,
    center_freqs: Vec<f32>,
    bandwidths_hz: Vec<f32>,
}

impl CqtTransform {
    /// Build a VQT transform. Kernel construction runs one FFT per bin,
    /// so this is the expensive step — do it once up front (e.g. at
    /// renderer init / sample-rate change).
    ///
    /// * `bpo` — bins per octave. 24 = 2/semitone is good spectrogram density.
    /// * `gamma_lo_hz`, `gamma_hi_hz`, `gamma_transition_hz` — define the
    ///   bandwidth floor γ as a smooth ramp from `gamma_lo_hz` at 0 Hz up
    ///   to `gamma_hi_hz` at `gamma_transition_hz` and above. Using a
    ///   smaller γ at the very bottom lets sub-bass bins grow long enough
    ///   windows to fit ≥ 4 cycles (kills the 2f ripple on pure bass
    ///   sines) while the normal γ above the knee keeps mid-and-above
    ///   kernels short enough for crisp transients. Pass
    ///   `gamma_lo_hz == gamma_hi_hz` for a constant floor.
    /// * `min_kernel_len` — per-bin kernel floor in samples. Prevents HF
    ///   kernels from shrinking below this, which keeps bandwidth narrow
    ///   enough to resolve closely-spaced partials and guarantees enough
    ///   overlap between consecutive hops for synchrosqueezing's phase
    ///   measurement to stay coherent. Set to `4 · hop` or so.
    /// * `causal_window` — if true, use the left half of a symmetric
    ///   length-(2·n_k − 1) Blackman-Harris as the per-bin window. The
    ///   window then peaks at the **newest** sample and tapers back to
    ///   the oldest, so each column reflects audio "as of now" instead
    ///   of audio centered n_k/2 samples ago. Wider main lobe than a
    ///   symmetric window of the same length (roughly 2× bandwidth),
    ///   in exchange for zero effective display latency.
    /// * `threshold_rel` — prunes kernel entries below
    ///   `threshold_rel · max_entry` per row; 0.005 is conservative.
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
        causal_window: bool,
        threshold_rel: f32,
    ) -> Self {
        assert!(n_fft.is_power_of_two(), "n_fft must be a power of two");
        assert!(fmax > fmin && fmin > 0.0, "need fmax > fmin > 0");
        assert!(bpo > 0);
        assert!(gamma_lo_hz >= 0.0 && gamma_hi_hz >= 0.0);
        assert!(gamma_transition_hz > 0.0);
        assert!((0.0..1.0).contains(&threshold_rel));

        // γ(f) = lo + (hi − lo) · min(1, f / transition).
        let gamma_at = |f: f32| -> f32 {
            let t = (f / gamma_transition_hz).clamp(0.0, 1.0);
            gamma_lo_hz + (gamma_hi_hz - gamma_lo_hz) * t
        };

        let num_bins = (bpo as f32 * (fmax / fmin).log2()).floor() as usize;
        assert!(num_bins > 0);

        // α is the inverse of the asymptotic high-freq Q. With classical
        // CQT Q = 1/(2^(1/bpo) − 1); we keep that as the high-freq limit
        // so tones stay pitch-sharp above the γ transition.
        let alpha = 2.0_f32.powf(1.0 / bpo as f32) - 1.0;

        let mut center_freqs = Vec::with_capacity(num_bins);
        let mut bandwidths_hz = Vec::with_capacity(num_bins);
        // Causal windows (half of a 2N−1 symmetric) have ~2× the
        // effective bandwidth of a symmetric window of the same length.
        // The IF-consistency gate reads these bandwidths, so match the
        // real spectral width.
        let bw_multiplier = if causal_window { 2.0 } else { 1.0 };
        for k in 0..num_bins {
            let f_k = fmin * 2.0_f32.powf(k as f32 / bpo as f32);
            center_freqs.push(f_k);
            // Effective bandwidth accounts for the kernel floor: when
            // `min_kernel_len` clamps n_k, the actual analysis bandwidth
            // shrinks to `sr / n_k`.
            let ideal = alpha * f_k + gamma_at(f_k);
            let ideal_n_k = (sample_rate / ideal).ceil() as usize;
            let n_k = ideal_n_k.min(n_fft).max(min_kernel_len).max(4);
            let effective_bw = bw_multiplier * sample_rate / n_k as f32;
            bandwidths_hz.push(effective_bw);
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
            // Variable-Q bandwidth. At high freq `α·f_k` dominates
            // (constant Q); below `gamma_transition_hz`, γ ramps down
            // so deep-bass windows grow long enough to fit several
            // cycles (kills the 2f AM ripple of a sub-bass sine).
            let bandwidth = alpha * f_k + gamma_at(f_k);
            let n_k_ideal = (sample_rate / bandwidth).ceil() as usize;
            let n_k = n_k_ideal.min(n_fft).max(min_kernel_len).max(4);

            // Symmetric: standard length-n_k BH, peaks in the middle.
            // Causal: left half of a length-(2n_k−1) symmetric BH —
            // peaks at the newest sample (index n_k−1), tapers to ~0
            // at the oldest (index 0). Zero effective display latency
            // at the cost of a wider main lobe.
            let w = if causal_window && n_k >= 2 {
                let full = blackman_harris_window(2 * n_k - 1);
                full[..n_k].to_vec()
            } else {
                blackman_harris_window(n_k)
            };
            let w_sum: f32 = w.iter().sum();
            // Normalise so that a unit-amplitude sinusoid at f_k yields
            // |CQT[k]| = 1. Derivation: for x[n] = cos(2π f_k n / sr),
            // <x, g_k> ≈ 0.5 · Σ w[n], so we scale by 2/Σw.
            let scale = 2.0 / w_sum;

            // Time-domain kernel: g_k[n] = w[n] · exp(+i 2π f_k n / sr),
            // **right-aligned** in the n_fft-length FFT buffer. Because
            // the audio buffer we hand to the FFT is in [oldest → newest]
            // order, right-alignment makes each kernel sample the NEWEST
            // N_k samples of that buffer — so every bin is "up to date"
            // at the rightmost sample. Left-alignment would make short
            // high-freq kernels read N_fft-seconds-old audio, making the
            // spectrogram lag by the full window length.
            //
            // The bin-dependent constant phase this introduces
            // (`exp(+i 2π f_k (N_fft - N_k) / sr)`) cancels in phase-diff
            // so synchrosqueezing is unaffected; magnitude is unaffected
            // period.
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
            bandwidths_hz,
        }
    }

    /// Per-bin bandwidth (Hz): `α · f_k + γ`. Used by synchrosqueezing's
    /// IF-consistency gate to reject aliased IF estimates that fall
    /// outside the bin's legitimate response region.
    pub fn bandwidths_hz(&self) -> &[f32] {
        &self.bandwidths_hz
    }

    pub fn num_bins(&self) -> usize {
        self.num_bins
    }

    #[allow(dead_code)] // diagnostic accessor
    pub fn n_fft(&self) -> usize {
        self.n_fft
    }

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

    /// Raw CSR sparse-kernel storage — `(row_ptr, col_idx, coef)`. The
    /// GPU CQT pipeline uploads these to immutable storage buffers once
    /// at worker spawn and reuses them across hops. CPU-side
    /// `process_complex` still reads them in-place.
    pub fn csr_raw(&self) -> (&[u32], &[u32], &[Complex<f32>]) {
        (&self.row_ptr, &self.col_idx, &self.coef)
    }

    /// Transform one N_fft-sample audio segment into complex VQT values
    /// per bin. Callers that only want magnitude take `.norm_sqr()`;
    /// callers that want synchrosqueezing need the phase too, hence
    /// we expose complex output directly.
    ///
    /// Per-hop runtime callers use the GPU pipeline in `gpu_cqt::GpuCqt`
    /// instead — this function is retained for unit tests that validate
    /// kernel construction and as a fallback if a future platform
    /// without MPSGraph ever needs it.
    #[allow(dead_code)]
    pub fn process_complex(&mut self, audio: &[f32], output: &mut [Complex<f32>]) {
        assert_eq!(audio.len(), self.n_fft);
        assert_eq!(output.len(), self.num_bins);

        // Real audio → complex FFT buffer (imaginary = 0).
        for (dst, &s) in self.fft_buffer.iter_mut().zip(audio.iter()) {
            *dst = Complex::new(s, 0.0);
        }
        self.fft
            .process_with_scratch(&mut self.fft_buffer, &mut self.fft_scratch);

        for (k, out) in output.iter_mut().enumerate().take(self.num_bins) {
            let lo = self.row_ptr[k] as usize;
            let hi = self.row_ptr[k + 1] as usize;
            let mut acc = Complex::new(0.0f32, 0.0f32);
            for idx in lo..hi {
                let m = self.col_idx[idx] as usize;
                acc += self.fft_buffer[m] * self.coef[idx];
            }
            *out = acc;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn powers_db(cqt: &mut CqtTransform, audio: &[f32]) -> Vec<f32> {
        let mut complex = vec![Complex::new(0.0, 0.0); cqt.num_bins()];
        cqt.process_complex(audio, &mut complex);
        complex
            .iter()
            .map(|c| 10.0 * (c.norm_sqr() + 1e-24).log10())
            .collect()
    }

    #[test]
    fn unit_sine_reads_near_zero_db_at_its_bin_cqt() {
        let sr = 48000.0;
        let n_fft = 16384;
        let fmin = 100.0;
        let fmax = 8000.0;
        let bpo = 24;
        let mut cqt = CqtTransform::new(sr, n_fft, fmin, fmax, bpo, 0.0, 0.0, 1.0, 4, false, 0.005);

        let target_freq = 1000.0;
        let audio: Vec<f32> = (0..n_fft)
            .map(|n| (2.0 * std::f32::consts::PI * target_freq * n as f32 / sr).cos())
            .collect();

        let out = powers_db(&mut cqt, &audio);

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
        let mut cqt = CqtTransform::new(sr, 8192, 100.0, 4000.0, 12, 0.0, 0.0, 1.0, 4, false, 0.005);
        let audio = vec![0.0; cqt.n_fft()];
        let out = powers_db(&mut cqt, &audio);
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
        let mut cqt = CqtTransform::new(sr, n_fft, 20.0, 1000.0, 12, 20.0, 20.0, 1.0, 4, false, 0.005);
        let target = 50.0_f32;
        let audio: Vec<f32> = (0..n_fft)
            .map(|n| (2.0 * std::f32::consts::PI * target * n as f32 / sr).cos())
            .collect();
        let out = powers_db(&mut cqt, &audio);
        let peak_db = out.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
        assert!(peak_db > -3.0, "VQT peak {peak_db} dB, expected near 0");
    }
}
