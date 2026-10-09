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
//! Short kernels use the equivalent time-domain dot product when that has fewer
//! terms than their spectral support. Both paths reuse preallocated storage.

use manifold_analyzer_dsp::blackman_harris_window;
use rustfft::{Fft, FftPlanner, num_complex::Complex};
use std::sync::Arc;

pub use rustfft::num_complex::Complex as CqtComplex;

/// Sparse kernel + FFT state. Stateless with respect to the audio
/// stream — feed it one N_fft-sample segment at a time.
pub struct CqtTransform {
    n_fft: usize,
    // FFT plan and scratch storage are reused for every worker hop.
    fft: Arc<dyn Fft<f32>>,
    fft_scratch: Vec<Complex<f32>>,
    fft_buffer: Vec<Complex<f32>>,
    // CSR-style sparse kernel matrix. Row k spans indices
    // `[row_ptr[k], row_ptr[k+1])` of `col_idx` and `coef`. `row_ptr`
    // uses compact u32 offsets: num_bins × n_fft is well under u32::MAX.
    row_ptr: Vec<u32>,
    col_idx: Vec<u32>,
    coef: Vec<Complex<f32>>,
    direct_offsets: Vec<usize>,
    direct_coef: Vec<Complex<f32>>,
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
            let ideal = alpha * f_k + gamma_at(f_k);
            let ideal_n_k = (sample_rate / ideal).ceil() as usize;
            let n_k = ideal_n_k.min(n_fft).max(4);
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
        let mut direct_offsets = vec![0];
        let mut direct_coef = Vec::new();

        let n_fft_inv = 1.0 / n_fft as f32;
        let two_pi = std::f64::consts::TAU;

        for &f_k in &center_freqs {
            // Variable-Q bandwidth. At high freq `α·f_k` dominates
            // (constant Q); below `gamma_transition_hz`, γ ramps down
            // so deep-bass windows grow long enough to fit several
            // cycles (kills the 2f AM ripple of a sub-bass sine).
            let bandwidth = alpha * f_k + gamma_at(f_k);
            let n_k_ideal = (sample_rate / bandwidth).ceil() as usize;
            let n_k = n_k_ideal.min(n_fft).max(4);

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
                let phase = two_pi * f_k as f64 * n as f64 / sample_rate as f64;
                let (s, c) = phase.sin_cos();
                let wn = w[n] * scale;
                kernel_buf[start + n] = Complex::new(wn * c as f32, wn * s as f32);
            }

            let direct_start = direct_coef.len();
            direct_coef.extend(kernel_buf[start..].iter().map(|c| c.conj()));
            let sparse_start = coef.len();

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
            // A short time-domain dot product avoids the wide spectral support
            // of a short window. Keep whichever representation has fewer terms.
            if n_k <= coef.len() - sparse_start {
                coef.truncate(sparse_start);
                col_idx.truncate(sparse_start);
            } else {
                direct_coef.truncate(direct_start);
            }
            direct_offsets.push(direct_coef.len());
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
            direct_offsets,
            direct_coef,
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

    /// Transform one N_fft-sample audio segment into complex VQT values
    /// per bin. Callers that only want magnitude take `.norm_sqr()`;
    /// callers that want synchrosqueezing need the phase too, hence
    /// we expose complex output directly.
    ///
    /// Reuses all FFT and sparse multiplication storage across hops.
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
            let direct = &self.direct_coef[self.direct_offsets[k]..self.direct_offsets[k+1]];
            if !direct.is_empty() {
                let samples = &audio[audio.len()-direct.len()..];
                let mut re = 0.0;
                let mut im = 0.0;
                for (&sample, coefficient) in samples.iter().zip(direct) {
                    re += sample * coefficient.re;
                    im += sample * coefficient.im;
                }
                *out = Complex::new(re, im);
                continue;
            }
            let lo = self.row_ptr[k] as usize;
            let hi = self.row_ptr[k + 1] as usize;
            let mut acc = Complex::new(0.0f32, 0.0f32);
            for (&index, &coefficient) in self.col_idx[lo..hi].iter().zip(&self.coef[lo..hi]) {
                acc += self.fft_buffer[index as usize] * coefficient;
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
        let mut cqt = CqtTransform::new(sr, n_fft, fmin, fmax, bpo, 0.0, 0.0, 1.0, false, 0.005);

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
        let mut cqt = CqtTransform::new(sr, 8192, 100.0, 4000.0, 12, 0.0, 0.0, 1.0, false, 0.005);
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
        let mut cqt = CqtTransform::new(sr, n_fft, 20.0, 1000.0, 12, 20.0, 20.0, 1.0, false, 0.005);
        let target = 50.0_f32;
        let audio: Vec<f32> = (0..n_fft)
            .map(|n| (2.0 * std::f32::consts::PI * target * n as f32 / sr).cos())
            .collect();
        let out = powers_db(&mut cqt, &audio);
        let peak_db = out.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
        assert!(peak_db > -3.0, "VQT peak {peak_db} dB, expected near 0");
    }

    #[test]
    fn default_vqt_keeps_high_frequency_midband_within_scalloping_budget() {
        for sr in [44_100.0, 48_000.0, 96_000.0] {
            let fmin = 10.0;
            let fmax = 22_000.0_f32.min(sr * 0.5);
            let bpo = 24;
            let gamma_lo = 10.0;
            let gamma_hi = 20.0;
            let gamma_transition = 200.0;
            let alpha = 2.0_f32.powf(1.0 / bpo as f32) - 1.0;
            let gamma_at = |frequency: f32| {
                let t = (frequency / gamma_transition).clamp(0.0, 1.0);
                gamma_lo + (gamma_hi - gamma_lo) * t
            };
            let longest = (sr / (alpha * fmin + gamma_at(fmin))).ceil() as usize;
            let n_fft = longest.max(4).next_power_of_two();
            let mut cqt = CqtTransform::new(
                sr,
                n_fft,
                fmin,
                fmax,
                bpo,
                gamma_lo,
                gamma_hi,
                gamma_transition,
                false,
                1e-6,
            );
            let center = cqt
                .center_freqs()
                .iter()
                .copied()
                .min_by(|a, b| (a - 18_000.0).abs().total_cmp(&(b - 18_000.0).abs()))
                .unwrap();
            let target = center * 2.0_f32.powf(0.5 / bpo as f32);
            let audio: Vec<f32> = (0..n_fft)
                .map(|n| (std::f32::consts::TAU * target * n as f32 / sr).cos())
                .collect();
            let peak_db = powers_db(&mut cqt, &audio)
                .into_iter()
                .fold(f32::NEG_INFINITY, f32::max);
            assert!(
                peak_db > -1.1,
                "{sr} Hz VQT peak {peak_db} dB at {target} Hz (center {center} Hz)"
            );
        }
    }
    #[test]
    fn hybrid_transform_matches_independent_time_domain_convolution() {
        for rate in [44100.0, 48000.0, 96000.0] {
            let params = crate::spectrum_gpu::cqt_build_params(rate);
            let mut cqt = CqtTransform::new(rate,params.n_fft,params.fmin,params.fmax,
                params.bpo,params.gamma_lo,params.gamma_hi,params.gamma_transition,
                params.causal,params.threshold_rel);
            assert!(!cqt.direct_coef.is_empty() && !cqt.coef.is_empty());
            let audio: Vec<f32> = (0..params.n_fft).map(|i| {
                [50.0,1000.0,18000.0].iter().map(|f|
                    0.08*(std::f64::consts::TAU*f*i as f64/rate as f64).cos()).sum::<f64>() as f32
            }).collect();
            let mut output = vec![Complex::new(0.0,0.0);cqt.num_bins()];
            cqt.process_complex(&audio,&mut output);
            for (&frequency,actual) in cqt.center_freqs().iter().zip(output) {
                let f=frequency as f64;
                let bandwidth=(2.0_f64.powf(1.0/24.0)-1.0)*f+10.0+10.0*(f/200.0).min(1.0);
                let n=(rate as f64/bandwidth).ceil() as usize;
                let mut real=0.0;let mut imag=0.0;let mut weight=0.0;
                for (i,&sample) in audio[audio.len()-n..].iter().enumerate() {
                    let phase=std::f64::consts::TAU*i as f64/(n-1) as f64;
                    let w=0.35875-0.48829*phase.cos()+0.14128*(2.0*phase).cos()-0.01168*(3.0*phase).cos();
                    let angle=std::f64::consts::TAU*f*i as f64/rate as f64;
                    real+=sample as f64*w*angle.cos();
                    imag-=sample as f64*w*angle.sin();weight+=w;
                }
                let error=((actual.re as f64-real*2.0/weight).powi(2)
                    +(actual.im as f64-imag*2.0/weight).powi(2)).sqrt();
                assert!(error<3e-6,"{rate}Hz, {frequency}Hz: complex error {error}");
            }
        }
    }

}
