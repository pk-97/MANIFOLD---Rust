"""Vocal gesture detection (non-drum event types).

Bass, synth, and pad detection migrated to Basic Pitch (basic_pitch_detection.py).
Only vocal gesture analysis remains here (madmom CNN + spectral composite).
"""

from __future__ import annotations

from typing import TYPE_CHECKING, List, Optional, Tuple

import numpy as np

from manifold_audio.models import Event, Peak
from manifold_audio.onset_detection import detect_stem_onsets
from manifold_audio.peak_detection import events_from_peaks
from manifold_audio.spectral import compute_band_onsets

if TYPE_CHECKING:
    from manifold_audio.models import AnalysisConfig

# Type alias for pre-computed madmom CNN onset results (activation, onset_times).
MadmomOnsetResult = Optional[Tuple[np.ndarray, np.ndarray]]


def _peaks_from_precomputed_onsets(
    onset_times: np.ndarray,
    composite: np.ndarray,
    hop_time: float,
    sample_half_window: int = 2,
) -> List[Peak]:
    """Convert pre-computed madmom onset times to Peaks using a composite envelope.

    Mirrors the logic in detect_stem_onsets() but skips the madmom CNN call.
    """
    peaks: List[Peak] = []
    for t in onset_times:
        frame_idx = int(round(float(t) / hop_time))
        lo = max(0, frame_idx - sample_half_window)
        hi = min(composite.size, frame_idx + sample_half_window + 1)
        if hi <= lo:
            continue
        strength = float(np.max(composite[lo:hi]))
        if strength > 0.0:
            peaks.append(Peak(time_sec=float(t), strength=strength))
    return peaks


def _aligned_onset_views(onsets: Sequence[np.ndarray]) -> List[np.ndarray]:
    if not onsets:
        return []
    frame_count = min((arr.size for arr in onsets), default=0)
    if frame_count <= 0:
        return [np.zeros(0, dtype=np.float32) for _ in onsets]
    return [np.ascontiguousarray(arr[:frame_count], dtype=np.float32) for arr in onsets]


def analyze_vocal_gestures(
    audio: np.ndarray,
    sample_rate: int,
    frame_size: int,
    hop_size: int,
    min_confidence: float,
    audio_path: Optional[str] = None,
    ffmpeg_bin: Optional[str] = None,
    precomputed_madmom_onsets: MadmomOnsetResult = None,
    detection_config: Optional["AnalysisConfig"] = None,
) -> List[Event]:
    """Detect vocal onsets (word/phrase attacks) from an isolated vocal stem.

    Tuned for the Demucs "vocals" stem.  Consonant attacks (t, k, p, s)
    create sharp transients in the presence band, while vowel onsets show
    formant energy shifts.  The combination gives robust per-word triggers.
    """
    cfg = detection_config
    bands_hz = {
        "chest": cfg.vocal_chest_band_hz if cfg and cfg.vocal_chest_band_hz is not None else (80.0, 500.0),
        "formant": cfg.vocal_formant_band_hz if cfg and cfg.vocal_formant_band_hz is not None else (500.0, 3000.0),
        "presence": cfg.vocal_presence_band_hz if cfg and cfg.vocal_presence_band_hz is not None else (3000.0, 8000.0),
    }
    _norm_w = cfg.local_norm_window if cfg and cfg.local_norm_window is not None else 3.0
    onsets, _rms, _hop = compute_band_onsets(audio, sample_rate, frame_size, hop_size, bands_hz, norm_window_sec=_norm_w)
    chest = onsets["chest"]
    formant = onsets["formant"]
    presence = onsets["presence"]
    arrays = _aligned_onset_views([chest, formant, presence])
    if not arrays or arrays[0].size < 3:
        return []
    chest, formant, presence = arrays

    hop_time = hop_size / float(sample_rate)
    chest_w = cfg.vocal_chest_weight if cfg and cfg.vocal_chest_weight is not None else 0.40
    formant_w = cfg.vocal_formant_weight if cfg and cfg.vocal_formant_weight is not None else 0.85
    presence_w = cfg.vocal_presence_weight if cfg and cfg.vocal_presence_weight is not None else 1.00
    composite = (chest_w * chest) + (formant_w * formant) + (presence_w * presence)

    candidates: Optional[List[Peak]] = None
    if precomputed_madmom_onsets is not None:
        _activation, onset_times = precomputed_madmom_onsets
        candidates = _peaks_from_precomputed_onsets(onset_times, composite, hop_time)
    elif audio_path is not None:
        candidates = detect_stem_onsets(
            audio_path=audio_path,
            composite=composite,
            hop_time=hop_time,
            ffmpeg_bin=ffmpeg_bin,
        )
    if not candidates:
        return []

    events = events_from_peaks("vocal", candidates, min_confidence)
    events.sort(key=lambda e: e.time)
    return events
