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
            magnitude_db: vec![MIN_DB; fft_size / 2],
        }
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
        for bin in 0..self.num_bins() {
            let c = self.fft_buffer[bin];
            let mag = (c.re * c.re + c.im * c.im).sqrt() * norm;
            self.magnitude_db[bin] = 20.0 * (mag + 1e-12).log10();
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
