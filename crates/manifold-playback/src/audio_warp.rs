//! Pitch-preserving time-stretch (warp) for the Audio Layer feature — see
//! `docs/AUDIO_LAYER_DESIGN.md` §4.1.
//!
//! This is the `warp(samples, ratio)` seam. It stretches a decoded interleaved
//! buffer **offline** (ahead of playback) via the vendored Signalsmith Stretch
//! library, preserving pitch, so the warped samples can be handed to kira at
//! `playback_rate = 1.0` — Signalsmith *replaces* the varispeed resample rather
//! than fighting kira's rate. Callers fall back to kira varispeed when this
//! returns `None` (no warp needed, degenerate input, or clip too short).

use std::os::raw::c_int;

unsafe extern "C" {
    /// Vendored Signalsmith wrapper (native/signalsmith_stretch.cpp). Stretches
    /// interleaved f32 `input` (`in_frames`×`channels`) into caller-allocated
    /// interleaved `output` (`out_frames`×`channels`); pitch preserved. Returns
    /// 1 on success, 0 on invalid/too-short input (output zeroed).
    fn manifold_signalsmith_stretch(
        input: *const f32,
        in_frames: c_int,
        output: *mut f32,
        out_frames: c_int,
        channels: c_int,
        sample_rate: f32,
    ) -> c_int;
}

/// Warp ratios outside this band are clamped: Signalsmith stays clean to ~2×,
/// and a clip warped past 4× either direction is almost certainly a mistaken
/// clip-BPM rather than an intended effect.
const MIN_RATIO: f32 = 0.25;
const MAX_RATIO: f32 = 4.0;
/// Closer than this to 1.0 and warp is a no-op (sub-cent tempo difference).
const RATIO_DEADZONE: f32 = 1.0e-3;

/// Time-stretch an interleaved f32 buffer by the warp `ratio` (project/clip BPM)
/// **without changing pitch**. The clip plays the same audio in `1/ratio` the
/// time, so the result has `in_frames / ratio` frames and is played at rate 1.0.
///
/// Returns `None` when no warp is needed (`ratio ≈ 1`), the input is degenerate,
/// or the clip is shorter than the stretch engine's minimum block — the caller
/// then keeps varispeed. The clamped ratio actually applied is returned alongside
/// the samples so the caller's position math matches what was rendered.
pub fn warp_interleaved(
    samples: &[f32],
    channels: usize,
    sample_rate: u32,
    ratio: f32,
) -> Option<(Vec<f32>, f32)> {
    if channels == 0 || sample_rate == 0 || samples.is_empty() || !ratio.is_finite() {
        return None;
    }
    let ratio = ratio.clamp(MIN_RATIO, MAX_RATIO);
    if (ratio - 1.0).abs() < RATIO_DEADZONE {
        return None;
    }

    let in_frames = samples.len() / channels;
    if in_frames == 0 {
        return None;
    }
    // Output is shorter when the clip must speed up (ratio > 1) and longer when
    // it must slow down. Round to the nearest frame; guard against zero.
    let out_frames = ((in_frames as f32 / ratio).round() as usize).max(1);

    let mut out = vec![0.0f32; out_frames * channels];
    // SAFETY: `samples` has `in_frames * channels` elements and `out` has
    // `out_frames * channels`; both lengths are passed exactly. The C wrapper
    // only reads/writes within those bounds and does not retain the pointers.
    let ok = unsafe {
        manifold_signalsmith_stretch(
            samples.as_ptr(),
            in_frames as c_int,
            out.as_mut_ptr(),
            out_frames as c_int,
            channels as c_int,
            sample_rate as f32,
        )
    };
    if ok == 1 { Some((out, ratio)) } else { None }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Dominant frequency of a mono f32 buffer via zero-crossing rate (Hz).
    /// Cheap and robust for a single sine — frequency, not duration, is what we
    /// assert warp preserves.
    fn zero_crossing_freq(samples: &[f32], sample_rate: u32) -> f32 {
        let mut crossings = 0usize;
        for w in samples.windows(2) {
            if (w[0] <= 0.0 && w[1] > 0.0) || (w[0] > 0.0 && w[1] <= 0.0) {
                crossings += 1;
            }
        }
        let secs = samples.len() as f32 / sample_rate as f32;
        (crossings as f32 / 2.0) / secs.max(1e-6)
    }

    fn sine(freq: f32, sample_rate: u32, frames: usize) -> Vec<f32> {
        (0..frames)
            .map(|i| (i as f32 / sample_rate as f32 * freq * std::f32::consts::TAU).sin())
            .collect()
    }

    #[test]
    fn deadzone_and_degenerate_return_none() {
        let s = sine(440.0, 44100, 44100);
        assert!(warp_interleaved(&s, 1, 44100, 1.0).is_none());
        assert!(warp_interleaved(&s, 1, 44100, 1.0001).is_none());
        assert!(warp_interleaved(&[], 1, 44100, 2.0).is_none());
        assert!(warp_interleaved(&s, 0, 44100, 2.0).is_none());
        assert!(warp_interleaved(&s, 1, 0, 2.0).is_none());
    }

    #[test]
    fn speed_up_halves_length_keeps_pitch() {
        // ratio 2.0 ⇒ play 2× faster ⇒ half the frames, same 440 Hz pitch.
        let sr = 44100;
        let input = sine(440.0, sr, sr as usize); // 1 second
        let (out, applied) = warp_interleaved(&input, 1, sr, 2.0).expect("warp ran");
        assert!((applied - 2.0).abs() < 1e-6);
        let expected = (input.len() as f32 / 2.0).round() as usize;
        assert_eq!(out.len(), expected, "output frame count = in/ratio");

        let f_in = zero_crossing_freq(&input, sr);
        let f_out = zero_crossing_freq(&out, sr);
        // Pitch preserved: dominant frequency unchanged within ~3%.
        assert!(
            (f_out - f_in).abs() / f_in < 0.03,
            "pitch drifted: in {f_in:.1} Hz, out {f_out:.1} Hz"
        );
    }

    #[test]
    fn slow_down_extends_length_keeps_pitch() {
        let sr = 44100;
        let input = sine(330.0, sr, sr as usize);
        let (out, applied) = warp_interleaved(&input, 1, sr, 0.5).expect("warp ran");
        assert!((applied - 0.5).abs() < 1e-6);
        let expected = (input.len() as f32 / 0.5).round() as usize;
        assert_eq!(out.len(), expected, "slower ⇒ more frames");
        assert!(out.iter().all(|s| s.is_finite()), "no NaNs/Infs");

        let f_in = zero_crossing_freq(&input, sr);
        let f_out = zero_crossing_freq(&out, sr);
        assert!(
            (f_out - f_in).abs() / f_in < 0.03,
            "pitch drifted: in {f_in:.1} Hz, out {f_out:.1} Hz"
        );
    }

    #[test]
    fn too_short_clip_returns_none() {
        // Below Signalsmith's seek window (~0.15·sr frames) ⇒ exact() fails,
        // wrapper returns 0 ⇒ None (caller keeps varispeed).
        let s = sine(440.0, 44100, 256);
        assert!(warp_interleaved(&s, 1, 44100, 2.0).is_none());
    }
}
