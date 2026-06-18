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
//!
//! Decode + analysis of a full audio file is expensive (seconds for a song), so
//! it runs on a **background thread** — never on the content tick (mirroring
//! `AudioWaveformCache` / `spawn_background_audio_load`). `sample_clip` requests
//! a decode once per clip, returns `None` until the curve lands, then samples
//! it. Doing the analysis inline on the tick would freeze the whole app the
//! first time the playhead crossed a layer-fed clip — see `AUDIO_LAYER_DESIGN`
//! §3 ("analyze once, off the realtime path").

use std::collections::HashSet;
use std::sync::mpsc::{Receiver, Sender, channel};

use ahash::AHashMap;

use manifold_audio::analysis::{FeatureCurve, OfflineSendAnalyzer};
use manifold_core::SendFeatures;
use manifold_core::clip::TimelineClip;
use manifold_core::id::ClipId;

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

/// The decode-input fingerprint a curve was (or is being) built from. A change
/// here (clip re-pointed, crossovers moved) re-requests the analysis.
type CurveKey = (String, f32, f32);

/// Per-`ClipId` cache of decoded-and-analysed feature curves for audio layers.
/// Owned by the content-thread audio-mod runtime. Lazy and **non-blocking**: a
/// clip's curve is requested the first time it is sampled, analysed on a
/// background thread, and reused until its file path or the project crossovers
/// change. `sample_clip` returns `None` until the curve is ready — the content
/// tick never decodes.
pub struct AudioLayerCurves {
    ready: AHashMap<ClipId, Cached>,
    /// Clips whose analysis has been requested, with the inputs it was spawned
    /// for. Guards against re-spawning every tick and lets a path / crossover
    /// change trigger a fresh request.
    requested: AHashMap<ClipId, CurveKey>,
    tx: Sender<(ClipId, CurveKey, FeatureCurve)>,
    rx: Receiver<(ClipId, CurveKey, FeatureCurve)>,
}

impl Default for AudioLayerCurves {
    fn default() -> Self {
        let (tx, rx) = channel();
        Self {
            ready: AHashMap::new(),
            requested: AHashMap::new(),
            tx,
            rx,
        }
    }
}

impl AudioLayerCurves {
    /// Sample the feature curve for an audio `clip` at `clip_local_seconds` (the
    /// offset into the source file the playhead is currently over). On first use
    /// (or after a path / crossover change) this kicks the decode + analysis off
    /// to a background thread and returns `None`; once the curve lands, every
    /// later call samples it. `None` also for a clip with no usable audio.
    pub fn sample_clip(
        &mut self,
        clip: &TimelineClip,
        low_hz: f32,
        mid_hz: f32,
        clip_local_seconds: f32,
    ) -> Option<SendFeatures> {
        self.drain();

        let want: CurveKey = (clip.audio_file_path.clone(), low_hz, mid_hz);

        // Fresh curve already analysed for exactly these inputs.
        if let Some(c) = self.ready.get(&clip.id)
            && c.path == want.0
            && c.low_hz == want.1
            && c.mid_hz == want.2
        {
            return Some(c.curve.at_seconds(clip_local_seconds, 0.0));
        }

        // Otherwise (re)request the analysis once for these inputs, off-thread.
        if !clip.audio_file_path.is_empty()
            && self.requested.get(&clip.id) != Some(&want)
        {
            self.requested.insert(clip.id.clone(), want.clone());
            self.spawn(clip.id.clone(), want);
        }
        None
    }

    /// Drain finished background analyses into the ready cache. A result is
    /// accepted only if it still matches the clip's current request, so a stale
    /// late curve (inputs changed mid-flight) is discarded.
    fn drain(&mut self) {
        while let Ok((id, key, curve)) = self.rx.try_recv() {
            if self.requested.get(&id) == Some(&key) {
                self.ready.insert(
                    id,
                    Cached { path: key.0, low_hz: key.1, mid_hz: key.2, curve },
                );
            }
        }
    }

    fn spawn(&self, id: ClipId, key: CurveKey) {
        let tx = self.tx.clone();
        if let Err(e) = std::thread::Builder::new()
            .name("audio-layer-curve".into())
            .spawn(move || {
                let (path, low_hz, mid_hz) = key;
                if let Some(curve) = decode_and_analyze(&path, low_hz, mid_hz) {
                    // Receiver gone only on shutdown — ignore the send error.
                    let _ = tx.send((id, (path, low_hz, mid_hz), curve));
                }
            })
        {
            log::warn!("[AudioLayerCurves] failed to spawn analyze thread: {e}");
        }
    }

    /// Drop cached curves (and pending requests) for clips no longer present,
    /// bounding memory. Called when the project changes (not per tick). A no-op
    /// when `live` contains every tracked key.
    pub fn retain_live(&mut self, live: &HashSet<ClipId>) {
        self.ready.retain(|id, _| live.contains(id));
        self.requested.retain(|id, _| live.contains(id));
    }

    /// Number of ready curves (test/diagnostic).
    #[cfg(test)]
    pub fn cached_len(&self) -> usize {
        self.ready.len()
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
        let clip =
            TimelineClip::new_audio(String::new(), Beats(0.0), Beats(1.0), Seconds(0.0), Seconds(0.0));
        assert!(curves.sample_clip(&clip, 250.0, 2000.0, 0.0).is_none());
        assert_eq!(curves.cached_len(), 0);
        // An empty path requests nothing — no background thread to spawn.
        assert!(curves.requested.is_empty());
    }

    #[test]
    fn analysis_is_requested_once_and_off_thread() {
        let mut curves = AudioLayerCurves::default();
        let clip = TimelineClip::new_audio(
            "/no/such/file.wav".into(),
            Beats(0.0),
            Beats(1.0),
            Seconds(0.0),
            Seconds(0.0),
        );
        // First sample requests the analysis (off-thread) and returns immediately
        // with no curve — the content tick is never blocked by the decode.
        assert!(curves.sample_clip(&clip, 250.0, 2000.0, 0.0).is_none());
        assert_eq!(curves.cached_len(), 0, "no synchronous decode on the tick");
        assert_eq!(curves.requested.len(), 1, "decode requested exactly once");
        // Sampling again with the same inputs does not re-spawn.
        assert!(curves.sample_clip(&clip, 250.0, 2000.0, 0.0).is_none());
        assert_eq!(curves.requested.len(), 1);
    }
}
