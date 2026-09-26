//! Compact, multi-resolution waveform data for offline display analysis.
//!
//! The renderer stores quantised peak and RMS measurements rather than the
//! source audio. Analysis is deliberately separate from playback processing:
//! it uses zero-phase filters so the display remains aligned with the source.

use ahash::AHasher;
use std::hash::Hasher;

mod filter;

const FINEST_FRAMES_PER_TEXEL: usize = 16;
const COARSEST_BIN_LIMIT: usize = 64;

/// A display sample containing full-band and three-band measurements.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct WaveformSample {
    pub peak: f32,
    pub band_peaks: [f32; 3],
    pub band_rms: [f32; 3],
}

/// Seven unsigned 16-bit values occupy exactly 14 bytes per waveform bin.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct PackedSample {
    values: [u16; 7],
}

impl PackedSample {
    fn from_sample(sample: WaveformSample) -> Self {
        let mut values = [0; 7];
        values[0] = quantise(sample.peak);
        for i in 0..3 {
            values[1 + i] = quantise(sample.band_peaks[i]);
            values[4 + i] = quantise(sample.band_rms[i]);
        }
        Self { values }
    }

    fn sample(self) -> WaveformSample {
        let scale = 1.0 / u16::MAX as f32;
        WaveformSample {
            peak: self.values[0] as f32 * scale,
            band_peaks: [
                self.values[1] as f32 * scale,
                self.values[2] as f32 * scale,
                self.values[3] as f32 * scale,
            ],
            band_rms: [
                self.values[4] as f32 * scale,
                self.values[5] as f32 * scale,
                self.values[6] as f32 * scale,
            ],
        }
    }
}

fn quantise(value: f32) -> u16 {
    (value.clamp(0.0, 1.0) * u16::MAX as f32).round() as u16
}

/// One level of the resolution pyramid.
#[derive(Debug)]
pub struct WaveformLevel {
    /// Source frames represented by each bin (the last bin may be shorter).
    pub frames_per_texel: usize,
    total_frames: usize,
    samples: Vec<PackedSample>,
}

impl WaveformLevel {
    fn new(frames_per_texel: usize, total_frames: usize, samples: Vec<PackedSample>) -> Self {
        Self {
            frames_per_texel: frames_per_texel.max(1),
            total_frames,
            samples,
        }
    }

    pub fn texel_count(&self) -> usize {
        self.samples.len()
    }

    /// Return a decoded bin, or silence when the index is out of bounds.
    pub fn sample(&self, index: usize) -> WaveformSample {
        self.samples
            .get(index)
            .copied()
            .map(PackedSample::sample)
            .unwrap_or_default()
    }

    /// Pool a source-file fraction without allocating.
    ///
    /// Peaks include every bin intersecting the interval. RMS values are
    /// weighted by the number of source frames represented by each overlap.
    /// A bin does not retain per-frame RMS, so a partial-bin query uses the
    /// bin's RMS for its overlapping frames.
    pub fn sample_range(&self, start: f64, end: f64) -> WaveformSample {
        if self.total_frames == 0 || !start.is_finite() || !end.is_finite() || end <= start {
            return WaveformSample::default();
        }

        let start = start.clamp(0.0, 1.0);
        let end = end.clamp(0.0, 1.0);
        if end <= start {
            return WaveformSample::default();
        }

        let frame_start =
            ((start * self.total_frames as f64).floor() as usize).min(self.total_frames);
        let frame_end = ((end * self.total_frames as f64).ceil() as usize).min(self.total_frames);
        if frame_end <= frame_start {
            return WaveformSample::default();
        }

        let first = frame_start / self.frames_per_texel;
        let last = (frame_end - 1) / self.frames_per_texel;
        let mut result = WaveformSample::default();
        let mut rms_sums = [0.0f64; 3];
        let mut weighted_frames = 0usize;

        for index in first..=last {
            let bin_start = index * self.frames_per_texel;
            let bin_end = (bin_start + self.frames_per_texel).min(self.total_frames);
            let overlap_start = frame_start.max(bin_start);
            let overlap_end = frame_end.min(bin_end);
            if overlap_end <= overlap_start {
                continue;
            }
            let overlap = overlap_end - overlap_start;
            let sample = self.sample(index);
            result.peak = result.peak.max(sample.peak);
            for (band, sum) in rms_sums.iter_mut().enumerate() {
                result.band_peaks[band] = result.band_peaks[band].max(sample.band_peaks[band]);
                *sum +=
                    sample.band_rms[band] as f64 * sample.band_rms[band] as f64 * overlap as f64;
            }
            weighted_frames += overlap;
        }

        if weighted_frames != 0 {
            for (band, sum) in rms_sums.into_iter().enumerate() {
                result.band_rms[band] = (sum / weighted_frames as f64).sqrt() as f32;
            }
        }
        result
    }
}

/// Offline waveform analysis and its compact resolution pyramid.
#[derive(Debug)]
pub struct WaveformRenderer {
    levels: Vec<WaveformLevel>,
    ready: bool,
    clip_duration_seconds: f32,
    clip_total_frames: usize,
    clip_frequency: u32,
    clip_channels: usize,
    content_fingerprint: u64,
}

impl Default for WaveformRenderer {
    fn default() -> Self {
        Self::new()
    }
}

impl WaveformRenderer {
    pub fn new() -> Self {
        Self {
            levels: Vec::with_capacity(8),
            ready: false,
            clip_duration_seconds: 0.0,
            clip_total_frames: 0,
            clip_frequency: 0,
            clip_channels: 0,
            content_fingerprint: 0,
        }
    }

    pub fn is_ready(&self) -> bool {
        self.ready
    }

    pub fn clip_duration_seconds(&self) -> f32 {
        self.clip_duration_seconds
    }

    pub fn clip_total_frames(&self) -> usize {
        self.clip_total_frames
    }

    pub fn level_count(&self) -> usize {
        self.levels.len()
    }

    pub fn get_level(&self, index: usize) -> Option<&WaveformLevel> {
        self.levels.get(index)
    }

    /// Number of bytes used by packed waveform bins.
    pub fn storage_bytes(&self) -> usize {
        self.levels
            .iter()
            .map(|level| level.samples.len() * std::mem::size_of::<PackedSample>())
            .sum()
    }

    /// Stable identity of the clip metadata and finest packed waveform data.
    pub fn content_fingerprint(&self) -> u64 {
        self.content_fingerprint
    }

    pub fn set_audio_data(&mut self, samples: &[f32], channels: usize, sample_rate: u32) {
        self.clear();
        if samples.is_empty() || channels == 0 || sample_rate == 0 {
            return;
        }

        let total_frames = samples.len() / channels;
        if total_frames == 0 {
            return;
        }

        self.clip_total_frames = total_frames;
        self.clip_frequency = sample_rate;
        self.clip_channels = channels;
        self.clip_duration_seconds = total_frames as f32 / sample_rate as f32;
        self.build_levels(samples);
        if let Some(level) = self.levels.first() {
            self.content_fingerprint =
                fingerprint(sample_rate, channels, total_frames, &level.samples);
        }
        self.ready = !self.levels.is_empty();
    }

    pub fn clear(&mut self) {
        self.levels.clear();
        self.ready = false;
        self.clip_duration_seconds = 0.0;
        self.clip_total_frames = 0;
        self.clip_frequency = 0;
        self.clip_channels = 0;
        self.content_fingerprint = 0;
    }

    /// Pick the finest level that does not exceed the current source density.
    pub fn select_level_for_zoom(
        &self,
        waveform_width_px: f32,
        render_scale: f32,
    ) -> Option<&WaveformLevel> {
        if self.levels.is_empty()
            || self.clip_total_frames == 0
            || waveform_width_px <= 0.0
            || render_scale <= 0.0
        {
            return None;
        }

        let frames_per_screen_pixel =
            self.clip_total_frames as f32 / (waveform_width_px * render_scale);
        let mut best = &self.levels[0];
        for level in &self.levels {
            if level.frames_per_texel as f32 > frames_per_screen_pixel {
                break;
            }
            best = level;
        }
        Some(best)
    }

    fn build_levels(&mut self, samples: &[f32]) {
        let finest = filter::build_finest(
            samples,
            self.clip_total_frames,
            self.clip_channels,
            self.clip_frequency,
        );
        if finest.is_empty() {
            return;
        }

        let mut frames_per_texel = FINEST_FRAMES_PER_TEXEL;
        let mut current = finest;
        loop {
            let next = if current.len() > COARSEST_BIN_LIMIT {
                Some(build_next_level(
                    &current,
                    frames_per_texel,
                    self.clip_total_frames,
                ))
            } else {
                None
            };
            self.levels.push(WaveformLevel::new(
                frames_per_texel,
                self.clip_total_frames,
                current,
            ));
            let Some(next) = next else { break };
            current = next;
            frames_per_texel *= 2;
        }
    }
}

fn build_next_level(
    previous: &[PackedSample],
    previous_frames_per_texel: usize,
    total_frames: usize,
) -> Vec<PackedSample> {
    let next_len = previous.len().div_ceil(2);
    let mut next = Vec::with_capacity(next_len);
    for index in 0..next_len {
        let first = index * 2;
        let a = previous[first].sample();
        let b = previous
            .get(first + 1)
            .copied()
            .unwrap_or_default()
            .sample();
        let a_start = (first * previous_frames_per_texel).min(total_frames);
        let a_frames = total_frames
            .min(a_start + previous_frames_per_texel)
            .saturating_sub(a_start);
        let b_start = ((first + 1) * previous_frames_per_texel).min(total_frames);
        let b_frames = total_frames
            .min(b_start + previous_frames_per_texel)
            .saturating_sub(b_start);
        let frame_count = a_frames + b_frames;
        let mut sample = WaveformSample {
            peak: a.peak.max(b.peak),
            band_peaks: [0.0; 3],
            band_rms: [0.0; 3],
        };
        for band in 0..3 {
            sample.band_peaks[band] = a.band_peaks[band].max(b.band_peaks[band]);
            let sum = a.band_rms[band] as f64 * a.band_rms[band] as f64 * a_frames as f64
                + b.band_rms[band] as f64 * b.band_rms[band] as f64 * b_frames as f64;
            sample.band_rms[band] = if frame_count == 0 {
                0.0
            } else {
                (sum / frame_count as f64).sqrt() as f32
            };
        }
        next.push(PackedSample::from_sample(sample));
    }
    next
}

fn fingerprint(
    sample_rate: u32,
    channels: usize,
    total_frames: usize,
    finest: &[PackedSample],
) -> u64 {
    let mut hasher = AHasher::default();
    hasher.write_u32(sample_rate);
    hasher.write_usize(channels);
    hasher.write_usize(total_frames);
    for sample in finest {
        for value in sample.values {
            hasher.write_u16(value);
        }
    }
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(rate: usize, frames: usize, frequency: f32, amplitude: f32) -> Vec<f32> {
        (0..frames)
            .map(|frame| {
                (frame as f32 * frequency * std::f32::consts::TAU / rate as f32).sin() * amplitude
            })
            .collect()
    }

    fn assert_sample_close(a: WaveformSample, b: WaveformSample, tolerance: f32) {
        assert!((a.peak - b.peak).abs() <= tolerance);
        for band in 0..3 {
            assert!((a.band_peaks[band] - b.band_peaks[band]).abs() <= tolerance);
            assert!((a.band_rms[band] - b.band_rms[band]).abs() <= tolerance);
        }
    }

    #[test]
    fn empty_invalid_and_short_inputs() {
        let mut renderer = WaveformRenderer::new();
        renderer.set_audio_data(&[], 1, 44_100);
        assert!(!renderer.is_ready());
        renderer.set_audio_data(&[0.5], 1, 0);
        assert!(!renderer.is_ready());
        renderer.set_audio_data(&[0.5], 1, 44_100);
        assert!(renderer.is_ready());
        assert_eq!(renderer.clip_total_frames(), 1);
        assert_eq!(renderer.get_level(0).unwrap().texel_count(), 1);
    }

    #[test]
    fn finest_bins_and_pyramid_are_compact() {
        let mut renderer = WaveformRenderer::new();
        renderer.set_audio_data(&vec![0.5; 16_384], 1, 44_100);
        assert_eq!(renderer.level_count(), 5);
        assert_eq!(renderer.get_level(0).unwrap().texel_count(), 1024);
        assert_eq!(renderer.get_level(4).unwrap().texel_count(), 64);
        assert_eq!(renderer.storage_bytes(), (1024 + 512 + 256 + 128 + 64) * 14);
    }

    #[test]
    fn isolated_tones_land_in_expected_bands() {
        for (frequency, band) in [(60.0, 0), (800.0, 1), (6000.0, 2)] {
            let mut renderer = WaveformRenderer::new();
            renderer.set_audio_data(&sine(48_000, 48_000, frequency, 0.8), 1, 48_000);
            let sample = renderer.get_level(0).unwrap().sample_range(0.2, 0.8);
            let chosen = sample.band_rms[band];
            let other = sample
                .band_rms
                .iter()
                .enumerate()
                .filter(|(index, _)| *index != band)
                .map(|(_, value)| *value)
                .fold(0.0, f32::max);
            assert!(chosen > other * 1.5, "{frequency}Hz: {sample:?}");
        }
    }

    #[test]
    fn dominant_tones_have_a_clear_band_at_all_supported_rates() {
        for rate in [44_100usize, 48_000, 96_000] {
            for (frequency, band) in [(60.0, 0), (800.0, 1), (6000.0, 2)] {
                let mut renderer = WaveformRenderer::new();
                renderer.set_audio_data(&sine(rate, rate, frequency, 0.8), 1, rate as u32);
                let sample = renderer.get_level(0).unwrap().sample_range(0.2, 0.8);
                let other = sample
                    .band_rms
                    .iter()
                    .enumerate()
                    .filter(|(index, _)| *index != band)
                    .map(|(_, value)| *value)
                    .fold(0.0, f32::max);
                assert!(
                    sample.band_rms[band] > other * 5.0,
                    "{rate}Hz {frequency}Hz: {sample:?}"
                );
            }
        }
    }

    #[test]
    fn opposite_phase_stereo_matches_mono_analysis() {
        let mono = sine(48_000, 20_000, 440.0, 0.7);
        let stereo: Vec<f32> = mono.iter().flat_map(|value| [*value, -*value]).collect();
        let mut mono_renderer = WaveformRenderer::new();
        let mut stereo_renderer = WaveformRenderer::new();
        mono_renderer.set_audio_data(&mono, 1, 48_000);
        stereo_renderer.set_audio_data(&stereo, 2, 48_000);
        for level in 0..mono_renderer.level_count() {
            let mono_level = mono_renderer.get_level(level).unwrap();
            let stereo_level = stereo_renderer.get_level(level).unwrap();
            assert_eq!(mono_level.texel_count(), stereo_level.texel_count());
            for index in 0..mono_level.texel_count() {
                assert_sample_close(mono_level.sample(index), stereo_level.sample(index), 2e-4);
            }
        }
    }

    #[test]
    fn nonfinite_samples_are_silence_and_invalid_inputs_stay_empty() {
        let mut renderer = WaveformRenderer::new();
        renderer.set_audio_data(&[f32::NAN, f32::INFINITY, f32::NEG_INFINITY], 1, 44_100);
        let sample = renderer.get_level(0).unwrap().sample(0);
        assert_eq!(sample, WaveformSample::default());
        renderer.set_audio_data(&[0.5], 0, 44_100);
        assert!(!renderer.is_ready());
        renderer.set_audio_data(&[], 1, 44_100);
        assert!(!renderer.is_ready());
    }

    #[test]
    fn partial_and_odd_pyramid_bins_use_true_frame_weights() {
        let previous = vec![
            PackedSample::from_sample(WaveformSample {
                band_rms: [0.25; 3],
                ..WaveformSample::default()
            }),
            PackedSample::from_sample(WaveformSample {
                band_rms: [0.75; 3],
                ..WaveformSample::default()
            }),
            PackedSample::from_sample(WaveformSample {
                band_rms: [0.5; 3],
                ..WaveformSample::default()
            }),
        ];
        let next = build_next_level(&previous[..2], 16, 20);
        let expected = ((16.0 * 0.25_f32.powi(2) + 4.0 * 0.75_f32.powi(2)) / 20.0).sqrt();
        assert!((next[0].sample().band_rms[0] - expected).abs() < 2e-4);
        let next = build_next_level(&previous, 16, 40);
        assert!((next[1].sample().band_rms[0] - 0.5).abs() < 2e-4);
        let level = WaveformLevel::new(16, 20, previous[..2].to_vec());
        let expected = ((8.0 * 0.25_f32.powi(2) + 4.0 * 0.75_f32.powi(2)) / 12.0).sqrt();
        assert!((level.sample_range(0.4, 1.0).band_rms[0] - expected).abs() < 2e-4);
    }

    #[test]
    fn narrow_impulse_peak_survives_every_level() {
        let mut samples = vec![0.0; 16_384];
        samples[8_192] = 1.0;
        let mut renderer = WaveformRenderer::new();
        renderer.set_audio_data(&samples, 1, 44_100);
        for level_index in 0..renderer.level_count() {
            let level = renderer.get_level(level_index).unwrap();
            let bin = 8_192 / level.frames_per_texel;
            assert!(level.sample(bin).peak > 0.999, "level {level_index}");
        }
    }

    #[test]
    fn impulse_band_energy_is_present_at_edges_and_block_seams() {
        let frames = 16_384;
        let positions = [0, 8_192, 10_000, frames - 1];
        let mut measurements = Vec::new();
        for position in positions {
            let mut samples = vec![0.0; frames];
            samples[position] = 1.0;
            let mut renderer = WaveformRenderer::new();
            renderer.set_audio_data(&samples, 1, 44_100);
            let level = renderer.get_level(0).unwrap();
            let sample = level.sample(position / level.frames_per_texel);
            assert!(sample.band_peaks.iter().all(|peak| *peak > 0.0001));
            assert!(sample.band_rms.iter().all(|rms| rms.is_finite()));
            measurements.push(sample);
        }

        // The zero-padded edge and block-boundary passes should retain the
        // same order of band energy as an interior impulse.
        let interior = measurements[2];
        for sample in measurements {
            for band in 0..3 {
                assert!(sample.band_peaks[band] > interior.band_peaks[band] * 0.1);
                assert!(sample.band_peaks[band] < interior.band_peaks[band] * 10.0);
            }
        }
    }

    #[test]
    fn zero_phase_bands_are_symmetric_across_block_and_file_edges() {
        let frames = 16_384;
        let mut pair = vec![0.0; frames];
        pair[8191] = 0.5;
        pair[8192] = 0.5;
        let mut renderer = WaveformRenderer::new();
        renderer.set_audio_data(&pair, 1, 48_000);
        let level = renderer.get_level(0).unwrap();
        for offset in 0..24 {
            assert_sample_close(level.sample(511 - offset), level.sample(512 + offset), 3e-5);
        }
        let mut edge = vec![0.0; frames];
        edge[0] = 1.0;
        let mut left = WaveformRenderer::new();
        left.set_audio_data(&edge, 1, 48_000);
        edge[0] = 0.0;
        edge[frames - 1] = 1.0;
        let mut right = WaveformRenderer::new();
        right.set_audio_data(&edge, 1, 48_000);
        for offset in 0..24 {
            assert_sample_close(
                left.get_level(0).unwrap().sample(offset),
                right.get_level(0).unwrap().sample(1023 - offset),
                3e-5,
            );
        }
    }

    #[test]
    fn fingerprint_is_stable_and_content_sensitive() {
        let mut a = WaveformRenderer::new();
        let mut b = WaveformRenderer::new();
        a.set_audio_data(&sine(44_100, 1000, 440.0, 0.5), 1, 44_100);
        b.set_audio_data(&sine(44_100, 1000, 440.0, 0.5), 1, 44_100);
        assert_eq!(a.content_fingerprint(), b.content_fingerprint());
        b.set_audio_data(&sine(44_100, 1000, 880.0, 0.5), 1, 44_100);
        assert_ne!(a.content_fingerprint(), b.content_fingerprint());
    }
}
