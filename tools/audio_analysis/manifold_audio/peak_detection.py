"""Peak finding, confidence normalization, and event construction."""

from __future__ import annotations

from typing import List, Sequence

import numpy as np

from manifold_audio.models import Event, Peak


def detect_peaks(
    onset: np.ndarray,
    hop_time: float,
    min_spacing_sec: float,
    threshold_scale: float,
    adaptive_window_sec: float = 2.0,
) -> List[Peak]:
    if onset.size < 3:
        return []

    max_onset = float(np.max(onset))
    if max_onset <= 0.0:
        return []

    # Global floor: prevents noise detection in silence.
    # Set very low — adaptive threshold handles real sensitivity control.
    global_floor = max_onset * 0.005

    # Adaptive local threshold: peak must exceed local mean by a factor
    # that scales with threshold_scale.  Adapts to musical dynamics so
    # quiet verses and loud drops both detect correctly.
    half_win = max(1, int(round(adaptive_window_sec / hop_time)))
    win_size = 2 * half_win + 1
    kernel_smooth = np.ones(win_size, dtype=np.float32) / win_size
    local_mean = np.convolve(onset, kernel_smooth, mode="same").astype(np.float32)
    delta_factor = 1.0 + 1.5 * threshold_scale
    threshold = np.maximum(global_floor, local_mean * delta_factor)

    min_spacing_frames = max(1, int(round(min_spacing_sec / hop_time)))
    peaks: List[Peak] = []
    last_idx = -10**9

    for i in range(1, onset.size - 1):
        v = float(onset[i])
        if v < float(threshold[i]):
            continue
        if not (v >= onset[i - 1] and v >= onset[i + 1]):
            continue
        if i - last_idx < min_spacing_frames:
            if peaks and v > peaks[-1].strength:
                peaks[-1] = Peak(time_sec=i * hop_time, strength=v)
                last_idx = i
            continue

        peaks.append(Peak(time_sec=i * hop_time, strength=v))
        last_idx = i

    return peaks


def normalize_confidences(
    peaks: Sequence[Peak],
    floor: float = 0.15,
    percentile_lo: float = 5.0,
    percentile_hi: float = 98.0,
    log_scale: float = 9.0,
) -> List[float]:
    if not peaks:
        return []

    strengths = np.array([p.strength for p in peaks], dtype=np.float32)
    lo = float(np.percentile(strengths, percentile_lo))
    hi = float(np.percentile(strengths, percentile_hi))
    if hi <= lo:
        hi = lo + 1e-6

    # Log-scale normalization: compresses dynamic range so quiet hits
    # aren't flatly discarded while loud hits still saturate near 1.0.
    confs: List[float] = []
    for s in strengths:
        z = (float(s) - lo) / (hi - lo)
        z = max(0.0, min(1.0, z))
        z = float(np.log1p(z * log_scale) / np.log1p(log_scale))
        c = floor + (1.0 - floor) * z
        confs.append(round(c, 4))
    return confs


def events_from_peaks(
    label: str,
    peaks: Sequence[Peak],
    min_confidence: float,
    confidence_floor: float = 0.15,
    confidence_percentile_lo: float = 5.0,
    confidence_percentile_hi: float = 98.0,
    confidence_log_scale: float = 9.0,
) -> List[Event]:
    confs = normalize_confidences(
        peaks,
        floor=confidence_floor,
        percentile_lo=confidence_percentile_lo,
        percentile_hi=confidence_percentile_hi,
        log_scale=confidence_log_scale,
    )
    out: List[Event] = []
    for peak, conf in zip(peaks, confs):
        if conf < min_confidence:
            continue
        out.append(Event(type=label, time=round(peak.time_sec, 4), confidence=conf))
    return out
