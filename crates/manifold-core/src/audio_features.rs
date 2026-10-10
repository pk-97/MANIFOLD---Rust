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
//! block. See `docs/AUDIO_MODULATION_DESIGN.md` section 5.

mod hops;
pub use hops::{
    AudioFeatureHop, AudioHopBatch, AudioHopCursor, AudioHopError, AudioHopSample, AudioHopStamp,
    new_audio_analysis_epoch,
};

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
    /// threshold on the band's flux. A general onset in the band.
    pub transients: f32,
    /// Kick trigger — 1.0 on the hop a kick fires, decaying after. Low band only
    /// (zero on Full/Mid/High), from the trained kick detector, independent of
    /// `transients` so binding one never blocks the other. See
    /// `docs/KICK_REALTIME_DESIGN.md`.
    pub kick: f32,
    /// Tracked dominant-object log-frequency position within this band's bin
    /// window, 0..1 (same mapping as `brightness`). HOLDS its last value on
    /// dropout — gate with `presence`, never read 0 as "low pitch". Filled by
    /// the D5 ridge tracker (`docs/AUDIO_OBJECT_TRACKING_DESIGN.md`), one per
    /// band window (Full/Low/Mid/High), all four run over one shared salience
    /// column (D4).
    pub pitch: f32,
    /// Tracker confidence 0..1 (ridge salience / window energy, smoothed) —
    /// see D5/D6 in `docs/AUDIO_OBJECT_TRACKING_DESIGN.md`.
    pub presence: f32,
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

/// Latest values for meters plus each send's completed analysis hops, indexed
/// by `AudioSetup::sends`. Producers reuse these buffers on the content thread;
/// stateful evaluators consume the hops with their own independent cursors.
#[derive(Clone, Debug, Default)]
pub struct AudioFeatureSnapshot {
    pub sends: Vec<SendFeatures>,
    /// Authoritative completed hops, indexed like `sends`. An empty vector is
    /// the legacy snapshot-only contract; an empty batch means no new hop.
    pub hop_batches: Vec<AudioHopBatch>,
    /// Source discontinuities observed during this update. Consumers recording
    /// inputs must invalidate that interval instead of treating it as silence.
    pub input_discontinuities: Vec<AudioInputDiscontinuity>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AudioInputSource {
    Capture,
    Layer(crate::LayerId),
    Send(crate::AudioSendId),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AudioInputProblem {
    Gap { first_frame: u64, end_frame: u64 },
    SourceChanged,
    FormatChanged,
    InvalidInput,
    AnalysisOverflow,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioInputDiscontinuity {
    pub source: AudioInputSource,
    pub problem: AudioInputProblem,
}

impl AudioFeatureSnapshot {
    /// Features for a send by its position index, or `None` if absent.
    pub fn get(&self, send_index: usize) -> Option<&SendFeatures> {
        self.sends.get(send_index)
    }

    /// True when there is neither a legacy snapshot nor an authoritative batch.
    pub fn is_empty(&self) -> bool {
        self.sends.is_empty() && self.hop_batches.is_empty()
    }
}
