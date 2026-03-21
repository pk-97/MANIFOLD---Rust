//! Multi-resolution waveform rendering engine with spectral coloring.
//!
//! Mechanical translation of `Assets/Scripts/UI/Timeline/WaveformRenderer.cs`.
//!
//! Builds a MIP chain of amplitude + spectral-color data from raw PCM samples.
//! Each level halves resolution via max-pooling. At render time the appropriate
//! level is selected based on zoom (frames per screen pixel).
//!
//! This module is pure computation — no GPU, no UI, no audio decoding.
//! It accepts raw interleaved PCM `&[f32]` samples and produces data that
//! `waveform_painter` can draw into pixel buffers.

use crate::color;
use crate::node::Color32;

// ── Constants (WaveformRenderer.cs lines 32-34) ──

const FINEST_FRAMES_PER_TEXEL: usize = 16;

/// A single resolution level in the MIP chain.
///
/// Unity: `WaveformRenderer.WaveformLevel` (inner class, lines 416-483).
pub struct WaveformLevel {
    /// Audio frames represented by each texel at this level.
    pub frames_per_texel: usize,
    /// Peak amplitude per texel (0.0–1.0).
    max_by_texel: Vec<f32>,
    /// Spectral color per texel.
    color_by_texel: Vec<Color32>,
}

impl WaveformLevel {
    fn new(frames_per_texel: usize, max_by_texel: Vec<f32>, color_by_texel: Vec<Color32>) -> Self {
        Self {
            frames_per_texel: frames_per_texel.max(1),
            max_by_texel,
            color_by_texel,
        }
    }

    pub fn texel_count(&self) -> usize {
        self.max_by_texel.len()
    }

    pub fn amplitude(&self, index: usize) -> f32 {
        if index < self.max_by_texel.len() {
            self.max_by_texel[index]
        } else {
            0.0
        }
    }

    pub fn color(&self, index: usize) -> Color32 {
        if index < self.color_by_texel.len() {
            self.color_by_texel[index]
        } else {
            Color32::new(160, 230, 225, 255) // fallback teal (Unity line 453)
        }
    }
}

/// Multi-resolution waveform data engine.
///
/// Unity: `WaveformRenderer` (lines 12-485).
///
/// Usage:
/// 1. Call `set_audio_data()` with raw PCM samples
/// 2. Call `select_level_for_zoom()` to pick the right resolution
/// 3. Read amplitude/color from the selected level to draw
pub struct WaveformRenderer {
    levels: Vec<WaveformLevel>,
    ready: bool,
    clip_duration_seconds: f32,
    clip_total_frames: usize,
    clip_frequency: u32,
    clip_channels: usize,
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

    /// Set audio data from raw interleaved PCM samples.
    ///
    /// Unity: `SetAudioClip(AudioClip clip)` (lines 50-70).
    ///
    /// - `samples`: interleaved PCM float data (all channels interleaved)
    /// - `channels`: number of audio channels (1=mono, 2=stereo, etc.)
    /// - `sample_rate`: sample rate in Hz (e.g. 44100, 48000)
    pub fn set_audio_data(
        &mut self,
        samples: &[f32],
        channels: usize,
        sample_rate: u32,
    ) {
        self.ready = false;
        self.clip_duration_seconds = 0.0;
        self.clip_total_frames = 0;
        self.clip_frequency = 0;
        self.clip_channels = 0;
        self.levels.clear();

        if samples.is_empty() || channels == 0 || sample_rate == 0 {
            return;
        }

        let total_frames = samples.len() / channels;
        if total_frames == 0 {
            return;
        }

        self.clip_total_frames = total_frames;
        self.clip_frequency = sample_rate;
        self.clip_channels = channels.max(1);
        self.clip_duration_seconds = (total_frames as f32 / sample_rate as f32).max(0.0001);

        if self.build_levels(samples) {
            self.ready = !self.levels.is_empty();
        }
    }

    /// Clear all waveform data.
    pub fn clear(&mut self) {
        self.levels.clear();
        self.ready = false;
        self.clip_duration_seconds = 0.0;
        self.clip_total_frames = 0;
        self.clip_frequency = 0;
        self.clip_channels = 0;
    }

    /// Select the best MIP level for the current zoom.
    ///
    /// Unity: `SelectLevelForCurrentZoom(float waveformWidth)` (lines 365-382).
    ///
    /// `waveform_width_px`: width of the waveform in screen pixels.
    /// `render_scale`: display scale factor (1.0 for 1x, 2.0 for Retina).
    pub fn select_level_for_zoom(
        &self,
        waveform_width_px: f32,
        render_scale: f32,
    ) -> Option<&WaveformLevel> {
        if self.levels.is_empty() || self.clip_total_frames == 0 || waveform_width_px <= 0.0 {
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

    // ──────────────────────────────────────
    // MIP CHAIN
    // ──────────────────────────────────────

    /// Build the full MIP chain from raw samples.
    ///
    /// Unity: `BuildLevels(AudioClip clip)` (lines 166-213).
    fn build_levels(&mut self, samples: &[f32]) -> bool {
        let finest_texel_count =
            (self.clip_total_frames as f32 / FINEST_FRAMES_PER_TEXEL as f32).ceil() as usize;
        let mut finest = vec![0.0f32; finest_texel_count];
        let mut finest_colors = vec![Color32::TRANSPARENT; finest_texel_count];

        if !self.populate_finest_level(samples, &mut finest, &mut finest_colors) {
            return false;
        }

        self.levels
            .push(WaveformLevel::new(FINEST_FRAMES_PER_TEXEL, finest.clone(), finest_colors.clone()));

        let mut frames_per_texel = FINEST_FRAMES_PER_TEXEL;
        let mut previous = finest;
        let mut prev_colors = finest_colors;

        // Unity: `while (previous.Length > 64)` (line 190)
        while previous.len() > 64 {
            let next_length = previous.len().div_ceil(2);
            let mut next = vec![0.0f32; next_length];
            let mut next_colors = vec![Color32::TRANSPARENT; next_length];

            for i in 0..next_length {
                let j = i * 2;
                let a = previous[j];
                let b = if j + 1 < previous.len() {
                    previous[j + 1]
                } else {
                    0.0
                };
                // Unity: `next[i] = a > b ? a : b;` (max-pooling)
                next[i] = if a > b { a } else { b };
                let color_a = prev_colors[j];
                let color_b = if j + 1 < prev_colors.len() {
                    prev_colors[j + 1]
                } else {
                    color_a
                };
                // Unity: `nextColors[i] = a >= b ? colorA : colorB;`
                next_colors[i] = if a >= b { color_a } else { color_b };
            }

            frames_per_texel *= 2;
            self.levels
                .push(WaveformLevel::new(frames_per_texel, next.clone(), next_colors.clone()));
            previous = next;
            prev_colors = next_colors;
        }

        !self.levels.is_empty()
    }

    /// Populate the finest MIP level from raw PCM samples.
    ///
    /// Unity: `PopulateFinestLevel(AudioClip clip, ...)` (lines 215-305).
    /// Same spectral energy analysis: total, high-freq (delta), accel (2nd-order delta).
    fn populate_finest_level(
        &self,
        samples: &[f32],
        amplitudes: &mut [f32],
        colors: &mut [Color32],
    ) -> bool {
        if amplitudes.is_empty() {
            return false;
        }

        let channels = self.clip_channels;

        let mut texel_total_energy = 0.0f32;
        let mut texel_high_energy = 0.0f32;
        let mut texel_accel_energy = 0.0f32;
        let mut prev_sample = 0.0f32;
        let mut prev_delta = 0.0f32;
        let mut current_texel: usize = 0;
        let mut frames_in_current_texel: usize = 0;

        // Unity iterates in chunks for GetData, but we have all samples in memory.
        // Process frame by frame matching Unity's exact per-frame logic (lines 252-297).
        for frame in 0..self.clip_total_frames {
            // Mix to mono + find peak amplitude (Unity lines 254-265)
            let mut mono_sample = 0.0f32;
            let mut amplitude = 0.0f32;
            let sample_index = frame * channels;
            for ch in 0..channels {
                if sample_index + ch >= samples.len() {
                    break;
                }
                let s = samples[sample_index + ch];
                mono_sample += s;
                let abs = if s < 0.0 { -s } else { s };
                if abs > amplitude {
                    amplitude = abs;
                }
            }
            mono_sample /= channels as f32;

            // Map frame to texel (Unity lines 267-272)
            let texel_index = (frame / FINEST_FRAMES_PER_TEXEL)
                .min(amplitudes.len() - 1);

            // Finalize previous texel when we advance (Unity lines 274-282)
            if texel_index != current_texel {
                finalize_texel_color(
                    colors,
                    current_texel,
                    texel_total_energy,
                    texel_high_energy,
                    texel_accel_energy,
                    frames_in_current_texel,
                );
                texel_total_energy = 0.0;
                texel_high_energy = 0.0;
                texel_accel_energy = 0.0;
                frames_in_current_texel = 0;
                current_texel = texel_index;
            }

            // Accumulate spectral energy metrics (Unity lines 284-294)
            let abs_mono = if mono_sample < 0.0 {
                -mono_sample
            } else {
                mono_sample
            };
            texel_total_energy += abs_mono;
            let delta = mono_sample - prev_sample;
            let abs_delta = if delta < 0.0 { -delta } else { delta };
            texel_high_energy += abs_delta;
            let accel = delta - prev_delta;
            texel_accel_energy += if accel < 0.0 { -accel } else { accel };
            prev_sample = mono_sample;
            prev_delta = delta;
            frames_in_current_texel += 1;

            // Peak amplitude per texel (Unity lines 295-296)
            if amplitude > amplitudes[texel_index] {
                amplitudes[texel_index] = amplitude;
            }
        }

        // Finalize last texel (Unity line 302)
        finalize_texel_color(
            colors,
            current_texel,
            texel_total_energy,
            texel_high_energy,
            texel_accel_energy,
            frames_in_current_texel,
        );

        true
    }
}

// ──────────────────────────────────────
// SPECTRAL COLORING
// ──────────────────────────────────────

/// Compute spectral color for a texel based on energy ratios.
///
/// Unity: `FinalizeTexelColor(...)` (lines 311-350).
///
/// Three energy metrics determine frequency content:
/// - `total_energy`: sum of |mono| — overall loudness
/// - `high_energy`: sum of |delta| — high-frequency content
/// - `accel_energy`: sum of |delta²| — transient sharpness
///
/// Color bands:
/// - Sub-bass (red):   highRatio < 0.06 && accelRatio < 0.5
/// - Bass (orange):    highRatio < 0.18
/// - Mid (green):      highRatio < 0.45
/// - High (blue):      highRatio >= 0.45
fn finalize_texel_color(
    colors: &mut [Color32],
    texel_index: usize,
    total_energy: f32,
    high_energy: f32,
    accel_energy: f32,
    frame_count: usize,
) {
    if texel_index >= colors.len() || frame_count == 0 {
        return;
    }

    const EPS: f32 = 0.0001;

    // Unity lines 320-326
    let high_ratio = if total_energy > EPS {
        (high_energy / (total_energy + EPS)).clamp(0.0, 1.0)
    } else {
        0.5
    };

    let accel_ratio = if high_energy > EPS {
        (accel_energy / (high_energy + EPS)).clamp(0.0, 1.0)
    } else {
        0.5
    };

    // Unity lines 328-348
    let c = if high_ratio < 0.06 && accel_ratio < 0.5 {
        let t = (high_ratio / 0.06).clamp(0.0, 1.0);
        lerp_color32(color::SPEC_SUB, color::SPEC_LOW, t)
    } else if high_ratio < 0.18 {
        let t = ((high_ratio - 0.06) / 0.12).clamp(0.0, 1.0);
        lerp_color32(color::SPEC_LOW, color::SPEC_MID, t)
    } else if high_ratio < 0.45 {
        let t = ((high_ratio - 0.18) / 0.27).clamp(0.0, 1.0);
        lerp_color32(color::SPEC_MID, color::SPEC_HIGH, t)
    } else {
        color::SPEC_HIGH
    };

    colors[texel_index] = c;
}

/// Linear interpolation between two Color32 values.
///
/// Unity: `LerpColor32(Color32 a, Color32 b, float t)` (lines 352-359).
fn lerp_color32(a: Color32, b: Color32, t: f32) -> Color32 {
    Color32::new(
        (a.r as f32 + (b.r as f32 - a.r as f32) * t) as u8,
        (a.g as f32 + (b.g as f32 - a.g as f32) * t) as u8,
        (a.b as f32 + (b.b as f32 - a.b as f32) * t) as u8,
        (a.a as f32 + (b.a as f32 - a.a as f32) * t) as u8,
    )
}

// ──────────────────────────────────────
// TESTS
// ──────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_data_not_ready() {
        let mut r = WaveformRenderer::new();
        r.set_audio_data(&[], 2, 44100);
        assert!(!r.is_ready());
        assert_eq!(r.level_count(), 0);
    }

    #[test]
    fn basic_mono_builds_levels() {
        let mut r = WaveformRenderer::new();
        // 44100 frames of silence — enough for multiple MIP levels
        let samples = vec![0.0f32; 44100];
        r.set_audio_data(&samples, 1, 44100);
        assert!(r.is_ready());
        assert!(r.level_count() > 1);
        assert!((r.clip_duration_seconds() - 1.0).abs() < 0.01);
    }

    #[test]
    fn finest_level_has_correct_texel_count() {
        let mut r = WaveformRenderer::new();
        let total_frames = 1600;
        let samples = vec![0.0f32; total_frames];
        r.set_audio_data(&samples, 1, 44100);
        assert!(r.is_ready());
        let level0 = r.get_level(0).unwrap();
        // 1600 / 16 = 100 texels
        assert_eq!(level0.texel_count(), 100);
        assert_eq!(level0.frames_per_texel, FINEST_FRAMES_PER_TEXEL);
    }

    #[test]
    fn mip_chain_halves_correctly() {
        let mut r = WaveformRenderer::new();
        // Need enough texels that downsampling produces multiple levels
        // 16384 frames → 1024 texels at finest → 512 → 256 → 128 → 64 → stop
        let samples = vec![0.5f32; 16384];
        r.set_audio_data(&samples, 1, 44100);
        assert!(r.is_ready());
        // Finest: 1024, then 512, 256, 128, 64 (stops when <= 64)
        assert_eq!(r.level_count(), 5);
        assert_eq!(r.get_level(0).unwrap().texel_count(), 1024);
        assert_eq!(r.get_level(1).unwrap().texel_count(), 512);
        assert_eq!(r.get_level(2).unwrap().texel_count(), 256);
        assert_eq!(r.get_level(3).unwrap().texel_count(), 128);
        assert_eq!(r.get_level(4).unwrap().texel_count(), 64);
    }

    #[test]
    fn stereo_samples_processed() {
        let mut r = WaveformRenderer::new();
        // Stereo: 800 frames × 2 channels = 1600 samples
        let mut samples = vec![0.0f32; 1600];
        // Put a loud signal in left channel at frame 0
        samples[0] = 0.9;
        r.set_audio_data(&samples, 2, 44100);
        assert!(r.is_ready());
        let level0 = r.get_level(0).unwrap();
        // Frame 0 is in texel 0, amplitude should be 0.9
        assert!((level0.amplitude(0) - 0.9).abs() < 0.01);
    }

    #[test]
    fn select_level_picks_appropriate() {
        let mut r = WaveformRenderer::new();
        let samples = vec![0.5f32; 16384];
        r.set_audio_data(&samples, 1, 44100);
        assert!(r.is_ready());

        // Very wide display (high zoom) → finest level
        let level = r.select_level_for_zoom(100000.0, 1.0).unwrap();
        assert_eq!(level.frames_per_texel, FINEST_FRAMES_PER_TEXEL);

        // Very narrow display (low zoom) → coarsest level
        let level = r.select_level_for_zoom(10.0, 1.0).unwrap();
        assert!(level.frames_per_texel > FINEST_FRAMES_PER_TEXEL);
    }

    #[test]
    fn spectral_coloring_loud_sine() {
        let mut r = WaveformRenderer::new();
        // Generate a 440Hz sine wave for spectral analysis
        let sample_rate = 44100;
        let total_frames = 4410; // 0.1 seconds
        let mut samples = Vec::with_capacity(total_frames);
        for i in 0..total_frames {
            let t = i as f32 / sample_rate as f32;
            samples.push((t * 440.0 * std::f32::consts::TAU).sin() * 0.8);
        }
        r.set_audio_data(&samples, 1, sample_rate);
        assert!(r.is_ready());

        // Check that texels have non-transparent colors
        let level0 = r.get_level(0).unwrap();
        let mid_texel = level0.texel_count() / 2;
        let c = level0.color(mid_texel);
        assert!(c.a > 0, "Spectral color should not be transparent");
    }

    #[test]
    fn lerp_color32_endpoints() {
        let a = Color32::new(0, 0, 0, 255);
        let b = Color32::new(255, 255, 255, 255);
        let mid = lerp_color32(a, b, 0.5);
        assert!((mid.r as i32 - 127).abs() <= 1);
        assert!((mid.g as i32 - 127).abs() <= 1);

        let at_a = lerp_color32(a, b, 0.0);
        assert_eq!(at_a.r, 0);
        let at_b = lerp_color32(a, b, 1.0);
        assert_eq!(at_b.r, 255);
    }
}
