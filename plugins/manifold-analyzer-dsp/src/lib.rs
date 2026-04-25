//! Real-time spectrum analysis primitives.
//!
//! Pure DSP, no GUI, no plugin glue. The same `Analyzer` is driven from the
//! VST3 plugin's audio callback and from the offline CLI — this is the
//! contract that lets the CLI verify DSP correctness without a DAW.

mod loudness;
pub mod reference;
mod stereo_analyzer;

pub use loudness::{
    IntegratedScratch, LoudnessMeter, LoudnessSnapshot, compute_integrated_and_lra,
};
pub use reference::{
    REF_FREQ_MAX, REF_FREQ_MIN, REF_POINTS, RefAnalysis, RefEnvelope, RefEnvelopeAtFft, RefError,
    analyze_ref_file,
};
pub use stereo_analyzer::StereoAnalyzer;

use rustfft::{Fft, FftPlanner, num_complex::Complex};
use std::sync::Arc;

pub(crate) fn ms_to_alpha(ms: f32, dt_s: f32) -> f32 {
    if ms <= 0.0 {
        1.0
    } else {
        1.0 - (-dt_s / (ms * 0.001)).exp()
    }
}

pub struct Analyzer {
    fft_size: usize,
    hop_size: usize,
    sample_rate: f32,
    fft: Arc<dyn Fft<f32>>,
    window: Vec<f32>,
    window_sum: f32,
    ring: Vec<f32>,
    ring_write_pos: usize,
    samples_since_last_fft: usize,
    fft_scratch: Vec<Complex<f32>>,
    fft_buffer: Vec<Complex<f32>>,
    power_avg: Vec<f32>,
    /// Per-direction EMA coefficients. Peak-style metering uses
    /// `attack_alpha = 1.0` (instant rise) and a slower release so peaks
    /// read cleanly while decay stays visible. Set equal for symmetric
    /// averaging (offline reference analysis).
    attack_alpha: f32,
    release_alpha: f32,
    attack_ms: f32,
    release_ms: f32,
    magnitude_db: Vec<f32>,
    // Un-averaged per-frame dB. Spectrograms want instantaneous frames so
    // transients stay sharp; the averaged `magnitude_db` is what the
    // attack/release-smoothed curve display consumes.
    raw_magnitude_db: Vec<f32>,
    // Reassignment state — previous frame's phase per bin and the derived
    // instantaneous frequency per bin. The frequency is where a bin's
    // energy "really is" (derived from phase-advance between frames), not
    // just the nominal bin frequency. Scatter-plotting power at these
    // frequencies collapses FFT main-lobe smears into near-delta peaks.
    prev_phase: Vec<f32>,
    instantaneous_freqs: Vec<f32>,
    have_prev_phase: bool,
}

impl Analyzer {
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
        // Include Nyquist (bin index fft_size/2). DC + Nyquist are
        // single-sided (no negative-frequency twin) and get a /4 power
        // correction inside `compute_spectrum` to read at the same dB
        // scale as the folded bins between them.
        let num_bins = fft_size / 2 + 1;

        Self {
            fft_size,
            hop_size,
            sample_rate,
            fft,
            window,
            window_sum,
            ring: vec![0.0; fft_size],
            ring_write_pos: 0,
            samples_since_last_fft: 0,
            fft_scratch: vec![Complex::new(0.0, 0.0); scratch_len],
            fft_buffer: vec![Complex::new(0.0, 0.0); fft_size],
            power_avg: vec![0.0; num_bins],
            // No averaging by default (each frame replaces previous).
            attack_alpha: 1.0,
            release_alpha: 1.0,
            attack_ms: 0.0,
            release_ms: 0.0,
            magnitude_db: vec![MIN_DB; num_bins],
            raw_magnitude_db: vec![MIN_DB; num_bins],
            prev_phase: vec![0.0; num_bins],
            // Default to nominal bin frequencies; first frame has no phase
            // history so we fall back to bin-nominal for one hop.
            instantaneous_freqs: (0..num_bins)
                .map(|k| k as f32 * sample_rate / fft_size as f32)
                .collect(),
            have_prev_phase: false,
        }
    }

    /// Set FFT frame overlap as a ratio in `[0.0, 0.99]`. `0.95` means
    /// 95 % overlap (hop = 5 % of `fft_size`). Also re-derives the averaging
    /// coefficient since its time constant is relative to the new frame rate.
    pub fn set_overlap_ratio(&mut self, ratio: f32) {
        let ratio = ratio.clamp(0.0, 0.99);
        let hop = ((1.0 - ratio) * self.fft_size as f32).round() as usize;
        self.hop_size = hop.max(1);
        self.samples_since_last_fft = 0;
        self.recompute_alpha_preserving_time();
    }

    /// Set symmetric exponential power-averaging time constant in
    /// milliseconds. `0.0` disables averaging (each frame replaces the
    /// previous). Equivalent to calling `set_attack_release_ms(ms, ms)`.
    pub fn set_averaging_ms(&mut self, ms: f32) {
        self.set_attack_release_ms(ms, ms);
    }

    /// Set separate attack (rise) and release (fall) time constants in
    /// milliseconds. Peak-style metering typically uses
    /// `attack_ms = 0.0` (instant rise) with a slow release so peaks
    /// register immediately and then decay visibly.
    pub fn set_attack_release_ms(&mut self, attack_ms: f32, release_ms: f32) {
        self.attack_ms = attack_ms.max(0.0);
        self.release_ms = release_ms.max(0.0);
        self.recompute_alpha_preserving_time();
    }

    fn recompute_alpha_preserving_time(&mut self) {
        let dt_s = self.hop_size as f32 / self.sample_rate;
        self.attack_alpha = ms_to_alpha(self.attack_ms, dt_s);
        self.release_alpha = ms_to_alpha(self.release_ms, dt_s);
    }

    pub fn fft_size(&self) -> usize {
        self.fft_size
    }

    pub fn sample_rate(&self) -> f32 {
        self.sample_rate
    }

    pub fn bin_frequency(&self, bin: usize) -> f32 {
        bin as f32 * self.sample_rate / self.fft_size as f32
    }

    pub fn num_bins(&self) -> usize {
        self.fft_size / 2 + 1
    }

    pub fn latest_spectrum_db(&self) -> &[f32] {
        &self.magnitude_db
    }

    /// Un-averaged dB spectrum of the most recent FFT frame. Intended for
    /// spectrograms where short-time detail must survive; if the plugin is
    /// configured with `set_averaging_ms(0.0)` this equals
    /// `latest_spectrum_db`.
    pub fn latest_raw_spectrum_db(&self) -> &[f32] {
        &self.raw_magnitude_db
    }

    /// Per-bin instantaneous frequency (Hz) derived from STFT phase
    /// advance. Use with `latest_raw_spectrum_db` to reassign power to
    /// where it "really is" in frequency (collapses FFT main-lobe width
    /// on sinusoidal content to near-delta peaks).
    pub fn latest_instantaneous_freqs(&self) -> &[f32] {
        &self.instantaneous_freqs
    }

    pub fn reset(&mut self) {
        self.ring.fill(0.0);
        self.ring_write_pos = 0;
        self.samples_since_last_fft = 0;
        self.power_avg.fill(0.0);
        self.magnitude_db.fill(MIN_DB);
        self.raw_magnitude_db.fill(MIN_DB);
        self.prev_phase.fill(0.0);
        self.have_prev_phase = false;
    }

    /// Push mono samples; updates `latest_spectrum_db` as frames complete.
    /// Returns `true` if at least one new FFT frame was computed.
    /// Allocation-free after construction.
    pub fn push_mono(&mut self, samples: &[f32]) -> bool {
        let mut new_frame = false;
        self.process_mono(samples, |_| {
            new_frame = true;
        });
        new_frame
    }

    /// Push mono samples; invokes `on_frame` with the dB spectrum each time
    /// a full hop-aligned FFT frame completes.
    pub fn process_mono<F: FnMut(&[f32])>(&mut self, samples: &[f32], mut on_frame: F) {
        self.process_mono_with_raw(samples, |avg, _raw, _freq| on_frame(avg));
    }

    /// Like `process_mono`, but the callback receives:
    /// - `avg`: averaged dB spectrum (for the smoothed display curves)
    /// - `raw`: un-averaged dB spectrum (for spectrograms)
    /// - `inst_freqs`: instantaneous frequency (Hz) per bin derived from
    ///   phase advance between consecutive frames (for spectral reassignment)
    pub fn process_mono_with_raw<F: FnMut(&[f32], &[f32], &[f32])>(
        &mut self,
        samples: &[f32],
        mut on_frame: F,
    ) {
        for &s in samples {
            self.ring[self.ring_write_pos] = s;
            self.ring_write_pos = (self.ring_write_pos + 1) % self.fft_size;
            self.samples_since_last_fft += 1;

            if self.samples_since_last_fft >= self.hop_size {
                self.samples_since_last_fft = 0;
                self.compute_spectrum();
                on_frame(
                    &self.magnitude_db,
                    &self.raw_magnitude_db,
                    &self.instantaneous_freqs,
                );
            }
        }
    }

    fn compute_spectrum(&mut self) {
        // Copy ring contents (oldest-first) into fft_buffer, applying window.
        let start = self.ring_write_pos;
        for i in 0..self.fft_size {
            let sample = self.ring[(start + i) % self.fft_size];
            self.fft_buffer[i] = Complex::new(sample * self.window[i], 0.0);
        }

        self.fft
            .process_with_scratch(&mut self.fft_buffer, &mut self.fft_scratch);

        // Single-sided magnitude spectrum, normalized by window energy.
        // Factor of 2 folds the negative-frequency twins onto the
        // positive bins (1 ≤ k ≤ N/2-1). DC (k=0) and Nyquist (k=N/2)
        // have no twin and use a /2 amplitude (= /4 power) correction
        // applied as `power *= dc_nyquist_scale` below — without this
        // they would read +6 dB high.
        let norm = 2.0 / self.window_sum;
        let norm_sq = norm * norm;
        let last_bin = self.num_bins() - 1; // Nyquist
        let attack_alpha = self.attack_alpha;
        let release_alpha = self.release_alpha;

        // Reassignment constants. `bin_hz` is the nominal frequency step
        // per FFT bin. `expected_per_bin` is the phase advance (rad) a
        // pure sinusoid at bin k would accrue over one hop:
        //   2π · k · hop / fft_size.
        // `freq_per_rad_per_hop` converts the deviation (rad/hop) to Hz:
        //   sample_rate / (2π · hop).
        let bin_hz = self.sample_rate / self.fft_size as f32;
        let two_pi = 2.0 * std::f32::consts::PI;
        let expected_per_bin_k = two_pi * self.hop_size as f32 / self.fft_size as f32;
        let freq_per_rad_per_hop = self.sample_rate / (two_pi * self.hop_size as f32);
        let have_prev = self.have_prev_phase;

        for bin in 0..self.num_bins() {
            let c = self.fft_buffer[bin];
            let mut power = (c.re * c.re + c.im * c.im) * norm_sq;
            if bin == 0 || bin == last_bin {
                // No negative-frequency twin to fold in.
                power *= 0.25;
            }
            self.raw_magnitude_db[bin] = 10.0 * (power + 1e-24).log10();
            // Asymmetric EMA: fast alpha when the new sample exceeds the
            // running average (attack), slow alpha when it's below
            // (release). Equal alphas → symmetric averaging.
            let prev = self.power_avg[bin];
            let alpha = if power > prev {
                attack_alpha
            } else {
                release_alpha
            };
            self.power_avg[bin] = alpha * power + (1.0 - alpha) * prev;
            self.magnitude_db[bin] = 10.0 * (self.power_avg[bin] + 1e-24).log10();

            let phase = c.im.atan2(c.re);
            if have_prev {
                let expected = expected_per_bin_k * bin as f32;
                let raw_dev = phase - self.prev_phase[bin] - expected;
                // Principal-value wrap to (-π, π]. For a bin whose signal
                // is close to nominal, `raw_dev` is already small; for
                // far-off signals the 2π ambiguity resolves here.
                let wrapped = raw_dev - two_pi * (raw_dev / two_pi).round();
                self.instantaneous_freqs[bin] = bin as f32 * bin_hz + wrapped * freq_per_rad_per_hop;
            }
            self.prev_phase[bin] = phase;
        }
        self.have_prev_phase = true;
    }
}

pub const MIN_DB: f32 = -120.0;

/// Equivalent Noise Bandwidth of the 4-term Blackman-Harris window in
/// units of FFT bins. Defined as `N · Σwₙ² / (Σwₙ)²`, this is the width
/// of an ideal rectangular filter that passes the same broadband noise
/// power as the windowed FFT bin. Used by callers that want to compare
/// noise floors across windows or convert to power-spectral-density:
///
///   PSD (per Hz) = bin_power / (ENBW · sample_rate / fft_size)
///
/// The analyzer's default normalization (`norm = 2/Σwₙ`) is *peak-correct*
/// — a unit sine reads 0 dBFS regardless of window choice — so the
/// noise-floor reading on broadband content sits at:
///
///   noise_floor_dB - 10·log10(ENBW · sr/fft_size)   (vs PSD at the bin freq)
///
/// Compared to a Hann-windowed analyzer (ENBW ≈ 1.5005), the BH noise
/// floor reads ≈ 10·log10(2.0044/1.5005) ≈ +1.26 dB higher. This is
/// expected behavior, not a calibration error.
pub const BH_WINDOW_ENBW_BINS: f32 = 2.0044;

/// ENBW of the Hann window (`N · Σwₙ² / (Σwₙ)²`). Provided for cross-
/// analyzer comparisons; not used internally.
pub const HANN_WINDOW_ENBW_BINS: f32 = 1.5005;

#[allow(dead_code)]
fn hann_window(size: usize) -> Vec<f32> {
    (0..size)
        .map(|n| {
            let x = 2.0 * std::f32::consts::PI * n as f32 / (size - 1) as f32;
            0.5 - 0.5 * x.cos()
        })
        .collect()
}

/// Blackman-Harris 4-term window. First sidelobe at ~−92 dB vs Hann's
/// −31 dB — drastically cleaner display between tones at the cost of a
/// wider main lobe (ENBW ≈ 2.00 bins vs Hann's 1.50). See
/// [`BH_WINDOW_ENBW_BINS`] for noise-floor calibration math.
pub fn blackman_harris_window(size: usize) -> Vec<f32> {
    const A0: f32 = 0.35875;
    const A1: f32 = 0.48829;
    const A2: f32 = 0.14128;
    const A3: f32 = 0.01168;
    let denom = (size - 1) as f32;
    (0..size)
        .map(|n| {
            let x = 2.0 * std::f32::consts::PI * n as f32 / denom;
            A0 - A1 * x.cos() + A2 * (2.0 * x).cos() - A3 * (3.0 * x).cos()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hann_is_zero_at_endpoints() {
        let w = hann_window(4096);
        assert!(w[0].abs() < 1e-6);
        assert!(w[w.len() - 1].abs() < 1e-6);
        assert!((w[w.len() / 2] - 1.0).abs() < 1e-3);
    }

    #[test]
    fn peak_bin_matches_sine_frequency() {
        let sample_rate = 44100.0;
        let fft_size = 4096;
        let mut analyzer = Analyzer::new(sample_rate, fft_size);

        let target_freq = 1000.0;
        let samples: Vec<f32> = (0..fft_size * 4)
            .map(|n| {
                (2.0 * std::f32::consts::PI * target_freq * n as f32 / sample_rate).sin()
            })
            .collect();

        analyzer.push_mono(&samples);

        let spectrum = analyzer.latest_spectrum_db();
        let peak_bin = spectrum
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
            .unwrap()
            .0;
        let peak_freq = analyzer.bin_frequency(peak_bin);
        let bin_width = sample_rate / fft_size as f32;

        assert!(
            (peak_freq - target_freq).abs() < bin_width,
            "peak at {peak_freq}Hz, expected {target_freq}Hz (bin width {bin_width}Hz)"
        );
    }

    #[test]
    fn peak_amplitude_is_roughly_0db_for_unit_sine() {
        let sample_rate = 48000.0;
        let fft_size = 8192;
        let mut analyzer = Analyzer::new(sample_rate, fft_size);

        let target_freq = 2000.0;
        let samples: Vec<f32> = (0..fft_size * 4)
            .map(|n| {
                (2.0 * std::f32::consts::PI * target_freq * n as f32 / sample_rate).sin()
            })
            .collect();

        analyzer.push_mono(&samples);

        let peak_db = analyzer
            .latest_spectrum_db()
            .iter()
            .cloned()
            .fold(MIN_DB, f32::max);

        // A unit-amplitude sine should read close to 0 dBFS (within ~1 dB of window scalloping).
        assert!(
            peak_db > -1.5 && peak_db < 1.5,
            "peak dB was {peak_db}, expected near 0"
        );
    }

    #[test]
    fn num_bins_includes_nyquist() {
        let analyzer = Analyzer::new(48000.0, 4096);
        // Positive half + Nyquist = N/2 + 1.
        assert_eq!(analyzer.num_bins(), 4096 / 2 + 1);
        // Last bin's nominal frequency is Nyquist (sr/2).
        let last = analyzer.num_bins() - 1;
        let nyquist = analyzer.bin_frequency(last);
        assert!((nyquist - 24_000.0).abs() < 1e-3, "Nyquist bin reads {nyquist} Hz");
    }

    #[test]
    fn dc_input_reads_at_zero_db() {
        // A pure DC offset of 1.0 should land at 0 dBFS in the DC bin.
        // Without the /4 single-sided correction it would read +6 dB.
        let mut analyzer = Analyzer::new(48000.0, 4096);
        let samples = vec![1.0_f32; 4096 * 4];
        analyzer.push_mono(&samples);
        let dc_db = analyzer.latest_raw_spectrum_db()[0];
        assert!(
            dc_db > -1.0 && dc_db < 1.0,
            "DC bin reads {dc_db} dB, expected near 0 (single-sided correction missing?)"
        );
    }

    #[test]
    fn nyquist_input_reads_at_zero_db() {
        // ±1 alternating = a Nyquist-frequency cosine of amplitude 1.0.
        // Without the Nyquist /4 correction it would read +6 dB.
        let fft_size = 4096;
        let mut analyzer = Analyzer::new(48000.0, fft_size);
        let samples: Vec<f32> = (0..fft_size * 4)
            .map(|n| if n % 2 == 0 { 1.0 } else { -1.0 })
            .collect();
        analyzer.push_mono(&samples);
        let nyquist_db = *analyzer.latest_raw_spectrum_db().last().unwrap();
        assert!(
            nyquist_db > -1.0 && nyquist_db < 1.0,
            "Nyquist bin reads {nyquist_db} dB, expected near 0"
        );
    }

    #[test]
    fn reset_clears_state() {
        let mut analyzer = Analyzer::new(44100.0, 1024);
        let samples: Vec<f32> = (0..4096).map(|n| (n as f32 * 0.01).sin()).collect();
        analyzer.push_mono(&samples);
        assert!(analyzer.latest_spectrum_db().iter().any(|&v| v > MIN_DB));

        analyzer.reset();
        assert!(analyzer.latest_spectrum_db().iter().all(|&v| v == MIN_DB));
    }
}
