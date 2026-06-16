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

/// Extracted features for one send at one analysis instant.
///
/// v1 fills `band_energy` (and, once the onset extractor lands, `onset`); the
/// pitch fields are v2 (the synchro ridge tracker) and default to zero until
/// that extractor produces them. New features become new fields here — the
/// modulation model's [`crate::audio_mod::AudioFeature`] enum selects among
/// them, so adding one does not disturb the plumbing.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SendFeatures {
    /// Overall RMS level of the analysis block, normalized **0..1** (samples are
    /// −1..1, so their RMS is in range by construction). This is the `Amplitude`
    /// feature — a true level the shaper maps straight onto a slider's range.
    pub amplitude: f32,
    /// Relative energy in [low, mid, high] perceptual bands. An unnormalized FFT
    /// magnitude (not 0..1) — the shaper's sensitivity scales it.
    pub band_energy: [f32; 3],
    /// Transient trigger, 0..1 impulse that decays (v1 extractor pending).
    pub onset: f32,
    /// Tracked fundamental in Hz (v2).
    pub pitch_hz: f32,
    /// Pitch rate-of-change in semitones/sec (v2) — the headline "motion"
    /// feature. Signed.
    pub pitch_delta_st: f32,
    /// Confidence the pitch reading is real, 0..1, from ridge magnitude /
    /// energy (v2). Gates the pitch features so they go still on non-tonal
    /// input.
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
