//! Offline modulation curves for audio layers (Phase 2 of the Audio Layer
//! feature — see `docs/AUDIO_LAYER_DESIGN.md` §3).
//!
//! A send fed by a timeline audio layer (`AudioSendSource::Layer`) does not run
//! through the live capture worker. Instead its clip's file is decoded once and
//! analysed into a per-hop [`FeatureCurve`] (the SAME transform + reductions the
//! live worker uses), cached by `ClipId`. Each tick the content thread samples
//! the curve at the playhead and writes the result into the engine's
//! `AudioFeatureSnapshot`, byte-identical to a live send. This makes the
//! modulation deterministic, look-ahead capable, and immune to content-thread
//! hitches (it is a table lookup, not a realtime producer).

use ahash::AHashMap;

use manifold_audio::analysis::{FeatureCurve, OfflineSendAnalyzer};
use manifold_core::clip::TimelineClip;
use manifold_core::id::ClipId;
use manifold_core::SendFeatures;

/// Downmix interleaved PCM to mono (mean of channels). `channels` 0 or 1 is a
/// passthrough.
fn downmix_to_mono(samples: &[f32], channels: usize) -> Vec<f32> {
    if channels <= 1 {
        return samples.to_vec();
    }
    samples
        .chunks_exact(channels)
        .map(|frame| frame.iter().sum::<f32>() / channels as f32)
        .collect()
}

/// Decode an audio file and analyse it into a feature curve at the given
/// crossovers. `None` (with a log) on a missing path or decode failure — a
/// genuine "no audio" rather than a silent stand-in.
fn decode_and_analyze(path: &str, low_hz: f32, mid_hz: f32) -> Option<FeatureCurve> {
    if path.is_empty() {
        return None;
    }
    let decoded = match manifold_playback::audio_decoder::decode_audio_to_pcm(path) {
        Ok(d) => d,
        Err(e) => {
            log::warn!("[AudioLayerCurves] decode failed for '{path}': {e}");
            return None;
        }
    };
    let mono = downmix_to_mono(&decoded.samples, decoded.channels);
    let mut analyzer = OfflineSendAnalyzer::new(decoded.sample_rate, low_hz, mid_hz);
    Some(analyzer.analyze(&mono))
}

/// One cached curve plus the inputs it was built from, so a path or crossover
/// change rebuilds it.
struct Cached {
    path: String,
    low_hz: f32,
    mid_hz: f32,
    curve: FeatureCurve,
}

/// Per-`ClipId` cache of decoded-and-analysed feature curves for audio layers.
/// Owned by the content-thread audio-mod runtime. Lazy: a clip's curve is built
/// the first time it is sampled and reused until its file path or the project
/// crossovers change.
#[derive(Default)]
pub struct AudioLayerCurves {
    cache: AHashMap<ClipId, Cached>,
}

impl AudioLayerCurves {
    /// Sample the feature curve for an audio `clip` at `clip_local_seconds` (the
    /// offset into the source file the playhead is currently over). Decodes +
    /// analyses on first use and caches; rebuilds if the path or crossovers
    /// changed. `None` if the clip has no usable audio (decode failure).
    pub fn sample_clip(
        &mut self,
        clip: &TimelineClip,
        low_hz: f32,
        mid_hz: f32,
        clip_local_seconds: f32,
    ) -> Option<SendFeatures> {
        self.get_or_build(clip, low_hz, mid_hz)
            .map(|curve| curve.at_seconds(clip_local_seconds, 0.0))
    }

    fn get_or_build(&mut self, clip: &TimelineClip, low_hz: f32, mid_hz: f32) -> Option<&FeatureCurve> {
        let key = clip.id.clone();
        let stale = self.cache.get(&key).is_none_or(|c| {
            c.path != clip.audio_file_path || c.low_hz != low_hz || c.mid_hz != mid_hz
        });
        if stale {
            match decode_and_analyze(&clip.audio_file_path, low_hz, mid_hz) {
                Some(curve) => {
                    self.cache.insert(
                        key.clone(),
                        Cached { path: clip.audio_file_path.clone(), low_hz, mid_hz, curve },
                    );
                }
                None => {
                    self.cache.remove(&key);
                    return None;
                }
            }
        }
        self.cache.get(&key).map(|c| &c.curve)
    }

    /// Drop cached curves for clips no longer present, bounding memory. Called
    /// when the project changes (not per tick). A no-op when `live` contains
    /// every cached key. Also reclaims when an audio clip's file path changes
    /// (the path mismatch already triggers a rebuild on next sample).
    pub fn retain_live(&mut self, live: &std::collections::HashSet<ClipId>) {
        self.cache.retain(|id, _| live.contains(id));
    }

    /// Number of cached curves (test/diagnostic).
    #[cfg(test)]
    pub fn cached_len(&self) -> usize {
        self.cache.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_core::units::{Beats, Seconds};

    #[test]
    fn downmix_to_mono_averages_channels() {
        // Stereo: [L,R, L,R] → mean per frame.
        assert_eq!(downmix_to_mono(&[1.0, 0.0, 0.5, 0.5], 2), vec![0.5, 0.5]);
        // Mono passthrough.
        assert_eq!(downmix_to_mono(&[0.1, 0.2], 1), vec![0.1, 0.2]);
    }

    #[test]
    fn missing_path_yields_no_curve() {
        let mut curves = AudioLayerCurves::default();
        let clip = TimelineClip::new_audio(String::new(), Beats(0.0), Beats(1.0), Seconds(0.0));
        assert!(curves.sample_clip(&clip, 250.0, 2000.0, 0.0).is_none());
        assert_eq!(curves.cached_len(), 0);
    }
}
