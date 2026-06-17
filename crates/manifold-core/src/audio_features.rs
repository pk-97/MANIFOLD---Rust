//! Runtime audio-feature data — the contract between the analysis worker and
//! the modulation evaluator.
//!
//! `manifold-audio`'s worker produces [`SendFeatures`] per send; the content
//! thread assembles an [`AudioFeatureSnapshot`] (indexed by send position in
//! `AudioSetup::sends`) and hands it to the modulation evaluator. Defining the
//! type here keeps the evaluator (in `manifold-playback`) free of any
//! dependency on the audio/CoreAudio stack — it reads core types only.
//!
//! These are **runtime** values: never serialized, recomputed every analysis
//! block. See `docs/AUDIO_MODULATION_DESIGN.md` §5.

/// The detector outputs for one frequency band, all normalized **0..1**. The
/// same five detectors run on every band (`Full`/`Low`/`Mid`/`High`), so any
/// feature can be measured over any band — the cross-product the drawer exposes.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct BandFeatures {
    /// Loudness of the band — dB-normalized energy (RMS of the band magnitude).
    pub amplitude: f32,
    /// Spectral centroid within the band — brightness (log-mapped 0..1).
    pub brightness: f32,
    /// Spectral flatness within the band — tonal (0) vs noisy (1).
    pub noisiness: f32,
    /// Relative spectral flux within the band — change ÷ band energy, so it
    /// self-scales with density instead of pinning on loud/busy material.
    pub liveliness: f32,
    /// Transient trigger — a 0..1 impulse that decays, from an adaptive
    /// threshold on the band's flux. `Transients` on `Low` is a kick detector.
    pub transients: f32,
}

/// Extracted features for one send at one analysis instant.
///
/// Per-band detector outputs (`bands`, indexed by [`crate::audio_mod::AudioBand`])
/// plus the per-send pitch fields. All cheap reductions over the one FFT the
/// worker runs; the pitch fields are v2 (the ridge tracker) and default to zero
/// until that extractor produces them. [`crate::audio_mod::AudioFeature`] selects
/// a `(kind, band)` cell, so adding a band or kind doesn't disturb the plumbing.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SendFeatures {
    /// Per-band detector outputs, indexed by `AudioBand::index()` —
    /// `[Full, Low, Mid, High]`.
    pub bands: [BandFeatures; 4],
    /// Tracked fundamental in Hz (v2) — per-send, not per-band.
    pub pitch_hz: f32,
    /// Pitch rate-of-change in semitones/sec (v2). Signed.
    pub pitch_delta_st: f32,
    /// Confidence the pitch reading is real, 0..1 (v2).
    pub pitch_confidence: f32,
}

/// All sends' features at one instant, indexed by send position in
/// `AudioSetup::sends`. Owned and rebuilt each tick from the worker's latest
/// frame; the evaluator resolves a slider's `AudioSendId` to a send index via
/// the `AudioSetup` and reads the matching entry.
#[derive(Clone, Debug, Default)]
pub struct AudioFeatureSnapshot {
    pub sends: Vec<SendFeatures>,
}

impl AudioFeatureSnapshot {
    /// Features for a send by its position index, or `None` if absent.
    pub fn get(&self, send_index: usize) -> Option<&SendFeatures> {
        self.sends.get(send_index)
    }

    /// True when no send has any features — the evaluator can skip the walk.
    pub fn is_empty(&self) -> bool {
        self.sends.is_empty()
    }
}
