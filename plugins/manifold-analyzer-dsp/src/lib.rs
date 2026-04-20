//! Real-time spectrum analysis primitives.
//!
//! Pure DSP, no GUI, no plugin glue. The same `Analyzer` is driven from the
//! VST3 plugin's audio callback and from the offline CLI — this is the
//! contract that lets the CLI verify DSP correctness without a DAW.

use rustfft::{Fft, FftPlanner, num_complex::Complex};
use std::sync::Arc;

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
    avg_alpha: f32,
    avg_time_ms: f32,
    magnitude_db: Vec<f32>,
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
        let window = hann_window(fft_size);
        let window_sum: f32 = window.iter().sum();
        let num_bins = fft_size / 2;

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
            avg_alpha: 1.0, // no averaging (instant) by default
            avg_time_ms: 0.0,
            magnitude_db: vec![MIN_DB; num_bins],
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

    /// Set exponential power-averaging time constant in milliseconds. `0.0`
    /// disables averaging (each frame replaces the previous).
    pub fn set_averaging_ms(&mut self, ms: f32) {
        self.avg_time_ms = ms.max(0.0);
        self.recompute_alpha_preserving_time();
    }

    fn recompute_alpha_preserving_time(&mut self) {
        if self.avg_time_ms <= 0.0 {
            self.avg_alpha = 1.0;
            return;
        }
        let dt_s = self.hop_size as f32 / self.sample_rate;
        let tau_s = self.avg_time_ms * 0.001;
        self.avg_alpha = 1.0 - (-dt_s / tau_s).exp();
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
        self.fft_size / 2
    }

    pub fn latest_spectrum_db(&self) -> &[f32] {
        &self.magnitude_db
    }

    pub fn reset(&mut self) {
        self.ring.fill(0.0);
        self.ring_write_pos = 0;
        self.samples_since_last_fft = 0;
        self.power_avg.fill(0.0);
        self.magnitude_db.fill(MIN_DB);
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
        for &s in samples {
            self.ring[self.ring_write_pos] = s;
            self.ring_write_pos = (self.ring_write_pos + 1) % self.fft_size;
            self.samples_since_last_fft += 1;

            if self.samples_since_last_fft >= self.hop_size {
                self.samples_since_last_fft = 0;
                self.compute_spectrum();
                on_frame(&self.magnitude_db);
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
        // Factor of 2 compensates for folding negative frequencies onto positives
        // (skipping DC and Nyquist bins, but for analyzer display this is fine).
        let norm = 2.0 / self.window_sum;
        let norm_sq = norm * norm;
        let alpha = self.avg_alpha;
        let one_minus_alpha = 1.0 - alpha;
        for bin in 0..self.num_bins() {
            let c = self.fft_buffer[bin];
            let power = (c.re * c.re + c.im * c.im) * norm_sq;
            // Exponential power averaging. alpha=1.0 → no averaging.
            self.power_avg[bin] = alpha * power + one_minus_alpha * self.power_avg[bin];
            self.magnitude_db[bin] = 10.0 * (self.power_avg[bin] + 1e-24).log10();
        }
    }
}

pub const MIN_DB: f32 = -120.0;

fn hann_window(size: usize) -> Vec<f32> {
    (0..size)
        .map(|n| {
            let x = 2.0 * std::f32::consts::PI * n as f32 / (size - 1) as f32;
            0.5 - 0.5 * x.cos()
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
    fn reset_clears_state() {
        let mut analyzer = Analyzer::new(44100.0, 1024);
        let samples: Vec<f32> = (0..4096).map(|n| (n as f32 * 0.01).sin()).collect();
        analyzer.push_mono(&samples);
        assert!(analyzer.latest_spectrum_db().iter().any(|&v| v > MIN_DB));

        analyzer.reset();
        assert!(analyzer.latest_spectrum_db().iter().all(|&v| v == MIN_DB));
    }
}
