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
mod scope;
mod window;
#[cfg(feature = "gpu")]
mod spectrogram;

pub use cqt::{CqtTransform, num_bins};
pub use scope::{MAX_ONSET_LANES, SCOPE_CENTROID_COUNT, ScopeColumn, ScopeOnsets};
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

/// Sample rate the default `hop`/`n_fft` durations are defined at (Hz). At this
/// rate `hop = 256` is a ~5.33 ms column and `n_fft = 4096` a ~85 ms window;
/// [`SpectrogramConfig::with_time_grid_for`] rescales both to hold those
/// DURATIONS fixed at any other device rate (BUG-052).
pub const REFERENCE_RATE_HZ: f32 = 48_000.0;

impl SpectrogramConfig {
    /// `fmax` capped to just under the device Nyquist (and kept above `fmin`).
    pub fn effective_fmax(&self, sample_rate: f32) -> f32 {
        self.fmax.min(sample_rate * 0.5 * 0.98).max(self.fmin * 2.0)
    }

    /// Rescale the TIME grid (`hop`, `n_fft`) from [`REFERENCE_RATE_HZ`] so a hop
    /// is always ~5.33 ms and the window ~85 ms regardless of `sample_rate`. The
    /// frequency axis needs no adjustment — bins are geometric (`bpo·log2(f/fmin)`)
    /// with `fmin`/`fmax` fixed in Hz, so they're already SR-invariant; only the
    /// sample-counted time grid drifts with the rate. Holding hop/window durations
    /// fixed keeps every hop-count tuning constant (kick descent window, ODF
    /// median, refractories, tracker slew) valid at 44.1/48/88.2/96/192 kHz
    /// without resampling the audio. `n_fft` rounds to a power of two (FFT
    /// requirement); the column rate `sample_rate / hop` stays ~constant too, so
    /// the scope scrolls at the same speed at every rate. At 48 kHz this is a
    /// no-op (returns the reference 256/4096). See BUG-052.
    #[must_use]
    pub fn with_time_grid_for(mut self, sample_rate: f32) -> Self {
        let ratio = (sample_rate / REFERENCE_RATE_HZ).max(1e-3);
        self.hop = ((self.hop as f32 * ratio).round() as usize).max(1);
        self.n_fft = ((self.n_fft as f32 * ratio).round() as usize)
            .max(1)
            .next_power_of_two();
        self
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

#[cfg(test)]
mod tests {
    use super::*;

    /// BUG-052: the hop and window DURATIONS must stay fixed across device rates,
    /// so every hop-count tuning constant in the analyzer keeps its wall-clock
    /// meaning. Frequency (bins) is already SR-invariant, so it's not retested here.
    #[test]
    fn time_grid_holds_hop_and_window_duration_across_rates() {
        let base = SpectrogramConfig::default();
        let hop_secs = base.hop as f32 / REFERENCE_RATE_HZ; // ~5.33 ms
        let win_secs = base.n_fft as f32 / REFERENCE_RATE_HZ; // ~85 ms

        // 48 kHz is the reference — must be an exact no-op.
        let at48 = base.with_time_grid_for(48_000.0);
        assert_eq!(at48.hop, base.hop);
        assert_eq!(at48.n_fft, base.n_fft);

        for &sr in &[44_100.0_f32, 88_200.0, 96_000.0, 192_000.0] {
            let c = base.with_time_grid_for(sr);
            // Hop duration within one sample of the reference.
            let hop_err = (c.hop as f32 / sr - hop_secs).abs();
            assert!(
                hop_err <= 1.0 / sr,
                "hop duration drifted at {sr} Hz: {} ms vs {} ms",
                c.hop as f32 / sr * 1e3,
                hop_secs * 1e3
            );
            // n_fft is power-of-two-rounded, so allow ±1 octave of window slack.
            assert!(c.n_fft.is_power_of_two(), "n_fft not pow2 at {sr} Hz");
            let win_ratio = (c.n_fft as f32 / sr) / win_secs;
            assert!(
                (0.5..=2.0).contains(&win_ratio),
                "window duration off by >1 octave at {sr} Hz: ratio {win_ratio}"
            );
        }

        // Doubling the rate doubles the sample-counted grid exactly.
        let at96 = base.with_time_grid_for(96_000.0);
        assert_eq!(at96.hop, base.hop * 2);
        assert_eq!(at96.n_fft, base.n_fft * 2);
    }
}
