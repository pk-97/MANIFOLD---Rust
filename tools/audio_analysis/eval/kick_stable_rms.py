"""Causal frequency-dependent RMS power features for kick experiments."""

from __future__ import annotations

import math

import numpy as np
from scipy.signal import butter, lfilter, sosfilt


BANDS = ((45.0, 140.0), (140.0, 400.0))
FAST_TAU_S = 0.003
SLOW_TAU_S = 0.080


def _trailing_average(power: np.ndarray, width: int) -> np.ndarray:
    """Return a zero-history, fixed-denominator trailing average."""
    cumulative = np.concatenate((np.zeros(1, dtype=np.float64), np.cumsum(power)))
    end = np.arange(1, len(power) + 1)
    start = np.maximum(end - width, 0)
    return (cumulative[end] - cumulative[start]) / width


def _ema(values: np.ndarray, sample_rate: int, tau: float) -> np.ndarray:
    alpha = math.exp(-1.0 / (tau * sample_rate))
    return lfilter([1.0 - alpha], [1.0, -alpha], values)


def stable_features(samples, sample_rate, envelopes, hop):
    """Replace low/body envelopes with causal frequency-dependent RMS power."""
    if sample_rate <= 0 or hop <= 0:
        raise ValueError("sample_rate and hop must be positive")
    env = np.asarray(envelopes, dtype=np.float64)
    if env.ndim != 3 or env.shape[1:] != (3, 2):
        raise ValueError("expected (hop, band, fast/slow) envelopes")
    if not np.all(np.isfinite(env)) or np.any(env < 0.0):
        raise ValueError("envelopes must be finite and non-negative")
    signal = np.asarray(samples, dtype=np.float64)
    if signal.ndim != 1 or not np.all(np.isfinite(signal)):
        raise ValueError("samples must be a finite mono signal")

    count = len(env)
    if count > len(signal) // hop:
        raise ValueError("samples must contain a complete hop per envelope")
    result = env.copy()
    for band, (low, high) in enumerate(BANDS):
        sos = butter(2, [low, high], btype="bandpass", fs=sample_rate, output="sos")
        power = sosfilt(sos, signal)
        power *= power
        averaged = _trailing_average(power, max(1, round(sample_rate / low)))
        fast = _ema(averaged, sample_rate, FAST_TAU_S)
        slow = _ema(averaged, sample_rate, SLOW_TAU_S)
        result[:count, band, 0] = fast[hop - 1 : count * hop : hop]
        result[:count, band, 1] = slow[hop - 1 : count * hop : hop]
    return result
