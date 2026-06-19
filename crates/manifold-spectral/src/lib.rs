//! Spectral DSP + spectrogram rendering for Manifold.
//!
//! Two reusable pieces, plus the config that ties them together:
//! - [`CqtTransform`] — a CPU constant-Q / variable-Q transform (ported from the
//!   Analyzer VST) that turns one FFT-window of samples into per-bin magnitudes.
//!   Used by the audio worker to produce **columns** for one calibration send.
//! - [`Spectrogram`] — a GPU waterfall renderer that scrolls those columns into a
//!   texture. Used by the Audio Setup scope.
//! - [`SpectrogramConfig`] — the shared parameters (FFT size, frequency range,
//!   bins/octave, hop, history depth, dB range) so the producer and the renderer
//!   agree on one layout without passing a transform across threads.
//!
//! No `manifold-core` dependency: this crate speaks raw `f32` samples and
//! magnitudes, so the worker (no GPU) and the panel (GPU) can each use the half
//! they need.

mod cqt;
mod window;
#[cfg(feature = "gpu")]
mod spectrogram;

pub use cqt::{CqtTransform, num_bins};
#[cfg(feature = "gpu")]
pub use spectrogram::Spectrogram;

/// Parameters for a calibration spectrogram. Defaults mirror the Analyzer
/// VST's *look* — 10 Hz–22 kHz over 24 bins/octave (~266 bins), a −59…0 dB
/// colour range — on a deliberately lighter single-send CPU transform: a
/// 16384-pt FFT (~341 ms) with a 256-sample hop (~188 columns/s, matching the
/// VST). The high column rate is what makes the sweep head advance smoothly
/// (~3 px/frame at 60 Hz, 1 column = 1 pixel) instead of lurching. The FFT is
/// still the light 16384-pt one — the VST's heavier 65536-pt FFT is for a
/// full-mix GPU pipeline we don't need here (see `cqt.rs`). `history_len` no
/// longer sizes the on-screen ring (the renderer sizes that to the scope's
/// pixel width for a crisp 1:1 sweep); it is retained only as a buffering hint.
#[derive(Clone, Copy, Debug)]
pub struct SpectrogramConfig {
    /// FFT size (power of two). Must exceed the longest VQT kernel.
    pub n_fft: usize,
    /// Lowest analysed frequency (Hz).
    pub fmin: f32,
    /// Highest analysed frequency (Hz), capped to just under Nyquist at build.
    pub fmax: f32,
    /// Bins per octave (frequency resolution).
    pub bpo: usize,
    /// Variable-Q bandwidth floor (Hz), frequency-ramped as
    /// `γ(f) = lo + (hi − lo)·min(1, f/transition)`. A smaller `lo` in deep bass
    /// grows the kernel longer for finer low-end resolution; `hi` governs mids
    /// and highs. `lo == hi` = constant γ; `0` = classical CQT.
    pub gamma_lo_hz: f32,
    pub gamma_hi_hz: f32,
    pub gamma_transition_hz: f32,
    /// Per-bin kernel length floor (samples).
    pub min_kernel_len: usize,
    /// Sparse-kernel prune threshold (relative to each row's peak).
    pub threshold_rel: f32,
    /// Samples between columns. Column rate = sample_rate / hop.
    pub hop: usize,
    /// Number of columns of scroll-back the waterfall keeps.
    pub history_len: usize,
    /// Colour-ramp dynamic range floor (dB).
    pub db_min: f32,
    /// Colour-ramp dynamic range ceiling (dB).
    pub db_max: f32,
    /// Pink-noise spectral tilt (dB/octave) applied to the colourmap and the
    /// feature reductions alike: pink noise reads as a flat field so a real-world
    /// mix isn't dominated by its bass. Auto-centred over the displayed range
    /// (mean 0). This is the SINGLE definition shared by the display shader and
    /// the detector's `tilt_weights` — they must tilt by the same slope or "what
    /// you see" and "what triggers" diverge. `0.0` is the raw "Flat" look.
    pub tilt_slope: f32,
}

impl Default for SpectrogramConfig {
    fn default() -> Self {
        Self {
            // 4096-pt window (~85 ms): the scope AND the per-send feature
            // detection share this one transform, so they stay in sync — the
            // user sees exactly the bands that modulate. The deep-bass kernels
            // clamp to n_fft (≈12 Hz/bin at the low end), trading a little
            // low-frequency resolution for transient latency low enough to be
            // usable as a live modulation source.
            n_fft: 4096,
            fmin: 10.0,
            fmax: 22_000.0,
            bpo: 24,
            // Ramped γ (matches the Analyzer VST): 20 Hz above 200 Hz keeps mids
            // and highs unchanged; 10 Hz in deep bass doubles the bass kernel
            // length for finer low-end resolution — still well within n_fft.
            gamma_lo_hz: 10.0,
            gamma_hi_hz: 20.0,
            gamma_transition_hz: 200.0,
            min_kernel_len: 256,
            threshold_rel: 1e-4,
            hop: 256,
            history_len: 2048,
            db_min: -59.0,
            db_max: 0.0,
            tilt_slope: 3.0,
        }
    }
}

impl SpectrogramConfig {
    /// `fmax` capped to just under the device Nyquist (and kept above `fmin`).
    pub fn effective_fmax(&self, sample_rate: f32) -> f32 {
        self.fmax.min(sample_rate * 0.5 * 0.98).max(self.fmin * 2.0)
    }

    /// Bin count for this config at `sample_rate` — the column length the
    /// producer and renderer must agree on.
    pub fn num_bins(&self, sample_rate: f32) -> usize {
        num_bins(self.fmin, self.effective_fmax(sample_rate), self.bpo)
    }

    /// Build the CPU transform for this config at `sample_rate`.
    pub fn build_transform(&self, sample_rate: f32) -> CqtTransform {
        CqtTransform::new(
            sample_rate,
            self.n_fft,
            self.fmin,
            self.effective_fmax(sample_rate),
            self.bpo,
            self.gamma_lo_hz,
            self.gamma_hi_hz,
            self.gamma_transition_hz,
            self.min_kernel_len,
            self.threshold_rel,
        )
    }
}
