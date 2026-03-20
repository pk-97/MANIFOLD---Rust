"""Pure math utilities with no domain knowledge."""

from __future__ import annotations

import numpy as np


def _clamp(value: float, lo: float, hi: float) -> float:
    return max(lo, min(hi, value))


def _safe_div(num: float, den: float) -> float:
    return num / (den + 1e-8)


def _local_mean_value(values: np.ndarray, center_idx: int, half_window_frames: int) -> float:
    if values.size == 0:
        return 0.0
    lo = max(0, center_idx - half_window_frames)
    hi = min(values.size, center_idx + half_window_frames + 1)
    if hi <= lo:
        return 0.0
    return float(np.mean(values[lo:hi]))


def _local_peak_value(values: np.ndarray, center_idx: int, half_window_frames: int) -> float:
    if values.size == 0:
        return 0.0

    lo = max(0, center_idx - half_window_frames)
    hi = min(values.size, center_idx + half_window_frames + 1)
    if hi <= lo:
        return 0.0
    return float(np.max(values[lo:hi]))


def robust_threshold(values: np.ndarray, threshold_scale: float) -> float:
    if values.size == 0:
        return 0.0

    med = float(np.median(values))
    mad = float(np.median(np.abs(values - med)))
    p85 = float(np.percentile(values, 85))
    adaptive = med + (2.5 * mad)
    threshold = max(p85 * threshold_scale, adaptive)
    if threshold > 1e-12:
        return threshold

    # Sparse transients can collapse percentiles to zero; recover from non-zero mass.
    non_zero = values[values > 0.0]
    if non_zero.size == 0:
        return 0.0

    nz_med = float(np.median(non_zero))
    nz_mad = float(np.median(np.abs(non_zero - nz_med)))
    nz_p60 = float(np.percentile(non_zero, 60))
    nz_peak = float(np.max(non_zero))
    sparse_threshold = max(
        nz_p60 * max(0.5, threshold_scale),
        nz_med + (1.6 * nz_mad),
        0.08 * nz_peak,
    )
    return max(0.0, sparse_threshold)
