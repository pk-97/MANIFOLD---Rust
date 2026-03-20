"""Onset detection: madmom CNN/RNN detector for beat tracking and vocal onsets."""

from __future__ import annotations

import os
import sys
from typing import List, Optional, Tuple

import numpy as np

from manifold_audio.models import Peak

from madmom.features.onsets import (
    CNNOnsetProcessor,
    OnsetPeakPickingProcessor,
    RNNOnsetProcessor,
)


def _ensure_ffmpeg_on_path(ffmpeg_bin: Optional[str]) -> None:
    """Prepend ffmpeg directory to PATH so madmom can decode compressed audio."""
    if ffmpeg_bin and os.path.isfile(ffmpeg_bin):
        ffmpeg_dir = os.path.dirname(os.path.abspath(ffmpeg_bin))
        current_path = os.environ.get("PATH", "")
        if ffmpeg_dir not in current_path.split(os.pathsep):
            os.environ["PATH"] = ffmpeg_dir + os.pathsep + current_path


def detect_madmom_onsets(
    audio_path: str,
    method: str = "cnn",
    threshold: float = 0.5,
    combine: float = 0.03,
    pre_max: float = 0.03,
    post_max: float = 0.03,
    ffmpeg_bin: Optional[str] = None,
) -> Optional[Tuple[np.ndarray, np.ndarray]]:
    """Detect onsets using madmom's CNN or RNN onset detector.

    Returns (activation_100fps, onset_times_sec) or None on failure.
    The activation is a 1D array at 100fps with values in [0, 1].
    onset_times_sec is a 1D array of peak-picked onset times in seconds.
    """
    try:
        _ensure_ffmpeg_on_path(ffmpeg_bin)

        if method == "rnn":
            proc = RNNOnsetProcessor()
        else:
            proc = CNNOnsetProcessor()

        activation = proc(audio_path)

        picker = OnsetPeakPickingProcessor(
            threshold=threshold,
            fps=100,
            pre_max=pre_max,
            post_max=post_max,
            combine=combine,
        )
        onset_times = picker(activation)

        onset_times = np.asarray(onset_times, dtype=np.float64).ravel()
        onset_times = onset_times[np.isfinite(onset_times) & (onset_times >= 0.0)]
        onset_times.sort()

        return activation, onset_times
    except Exception as exc:
        print(f"[onset] madmom onset detection failed: {exc}", file=sys.stderr)
        return None


def detect_stem_onsets(
    audio_path: str,
    composite: np.ndarray,
    hop_time: float,
    sample_half_window: int = 2,
    ffmpeg_bin: Optional[str] = None,
) -> Optional[List[Peak]]:
    """Detect onsets on a single-type stem (bass, synth, vocal).

    Uses madmom CNN for onset timing, then samples the pre-computed composite
    envelope for peak strength. Unlike classify_onsets_by_band() (which
    assigns across multiple bands), this produces a flat list of Peaks since
    the stem already contains a single instrument class.

    Returns a list of Peaks, or None if madmom is unavailable/fails.
    """
    result = detect_madmom_onsets(audio_path, method="cnn", ffmpeg_bin=ffmpeg_bin)
    if result is None:
        return None

    _activation, onset_times = result
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
