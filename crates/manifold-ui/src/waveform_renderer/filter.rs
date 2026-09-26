//! Zero-phase, sample-rate-aware filters used by waveform analysis.

use super::{FINEST_FRAMES_PER_TEXEL, PackedSample, WaveformSample};

const CORE_BLOCK_FRAMES: usize = 8192;
const Q_BUTTERWORTH: f64 = std::f64::consts::FRAC_1_SQRT_2;

#[derive(Clone, Copy, Debug)]
struct Biquad {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
    z1: f64,
    z2: f64,
}

impl Biquad {
    fn low_pass(sample_rate: u32, cutoff: f64) -> Self {
        let cutoff = cutoff.min(sample_rate as f64 * 0.45).max(0.000_001);
        let w0 = std::f64::consts::TAU * cutoff / sample_rate as f64;
        let cos = w0.cos();
        let alpha = w0.sin() / (2.0 * Q_BUTTERWORTH);
        let a0 = 1.0 + alpha;
        Self {
            b0: ((1.0 - cos) * 0.5) / a0,
            b1: (1.0 - cos) / a0,
            b2: ((1.0 - cos) * 0.5) / a0,
            a1: (-2.0 * cos) / a0,
            a2: (1.0 - alpha) / a0,
            z1: 0.0,
            z2: 0.0,
        }
    }

    fn reset(&mut self) {
        self.z1 = 0.0;
        self.z2 = 0.0;
    }

    fn process(&mut self, input: f64) -> f64 {
        let output = self.b0 * input + self.z1;
        self.z1 = self.b1 * input - self.a1 * output + self.z2;
        self.z2 = self.b2 * input - self.a2 * output;
        output
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct Accumulator {
    peak: f32,
    band_peaks: [f32; 3],
    band_sums: [f64; 3],
}

pub(super) fn build_finest(
    samples: &[f32],
    total_frames: usize,
    channels: usize,
    sample_rate: u32,
) -> Vec<PackedSample> {
    if total_frames == 0 || channels == 0 || sample_rate == 0 {
        return Vec::new();
    }

    let bin_count = total_frames.div_ceil(FINEST_FRAMES_PER_TEXEL);
    let mut packed = Vec::with_capacity(bin_count);
    let mut raw = Vec::new();
    let mut low = Vec::new();
    let mut low_mid = Vec::new();
    let halo = ((sample_rate as f64 * 0.05).ceil() as usize).max(16);
    let low_cutoff = 250.0f64.min(sample_rate as f64 * 0.45);
    let high_cutoff = 2000.0f64.min(sample_rate as f64 * 0.45);

    for core_start in (0..total_frames).step_by(CORE_BLOCK_FRAMES) {
        let core_end = (core_start + CORE_BLOCK_FRAMES).min(total_frames);
        let core_frames = core_end - core_start;
        let mut bins = vec![Accumulator::default(); core_frames.div_ceil(FINEST_FRAMES_PER_TEXEL)];

        // Keep a signed origin so both edges receive a real zero halo. In
        // particular, the trailing halo lets the reverse pass see the full
        // forward-filter tail at the end of the file.
        let padded_start = core_start as isize - halo as isize;
        let padded_end = core_end as isize + halo as isize;
        let padded_len = (padded_end - padded_start) as usize;
        raw.resize(padded_len, 0.0);
        low.resize(padded_len, 0.0);
        low_mid.resize(padded_len, 0.0);

        for channel in 0..channels {
            for (offset, frame) in (padded_start..padded_end).enumerate() {
                let value = if frame >= 0 && (frame as usize) < total_frames {
                    let source_index = frame as usize * channels + channel;
                    samples.get(source_index).copied().unwrap_or(0.0)
                } else {
                    0.0
                };
                raw[offset] = if value.is_finite() { value } else { 0.0 };
            }

            zero_phase(&raw, &mut low, sample_rate, low_cutoff);
            zero_phase(&raw, &mut low_mid, sample_rate, high_cutoff);

            let interior_start = (core_start as isize - padded_start) as usize;
            for frame in core_start..core_end {
                let offset = interior_start + frame - core_start;
                let original = raw[offset];
                let low_value = low[offset];
                let low_mid_value = low_mid[offset];
                let values = [
                    low_value,
                    low_mid_value - low_value,
                    original - low_mid_value,
                ];
                let bin = (frame - core_start) / FINEST_FRAMES_PER_TEXEL;
                let accumulator = &mut bins[bin];
                accumulator.peak = accumulator.peak.max(original.abs());
                for (band, value) in values.into_iter().enumerate() {
                    let value = if value.is_finite() { value } else { 0.0 };
                    accumulator.band_peaks[band] = accumulator.band_peaks[band].max(value.abs());
                    accumulator.band_sums[band] += value as f64 * value as f64;
                }
            }
        }

        // The core size is bin-aligned, so this block is the only accumulator
        // storage needed while the source is analysed.
        for (index, accumulator) in bins.into_iter().enumerate() {
            let frame_count =
                (core_frames - index * FINEST_FRAMES_PER_TEXEL).min(FINEST_FRAMES_PER_TEXEL);
            let denominator = (frame_count * channels) as f64;
            let mut sample = WaveformSample {
                peak: accumulator.peak,
                band_peaks: accumulator.band_peaks,
                band_rms: [0.0; 3],
            };
            for band in 0..3 {
                sample.band_rms[band] = (accumulator.band_sums[band] / denominator).sqrt() as f32;
            }
            packed.push(PackedSample::from_sample(sample));
        }
    }

    packed
}

fn zero_phase(input: &[f32], output: &mut [f32], sample_rate: u32, cutoff: f64) {
    debug_assert_eq!(input.len(), output.len());
    let mut filter = Biquad::low_pass(sample_rate, cutoff);
    filter.reset();
    for (input, output) in input.iter().zip(output.iter_mut()) {
        *output = filter.process(*input as f64) as f32;
    }

    filter.reset();
    for index in (0..output.len()).rev() {
        output[index] = filter.process(output[index] as f64) as f32;
    }
}
