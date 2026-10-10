"""Causal quadrature power features for the isolated kick experiment.

The low and body bands use causal complex demodulation followed by a
fourth-order lowpass.  Comparisons with the old real bandpass envelopes must
account for this different transition shape as well as the quadrature power.
"""

from __future__ import annotations

import math

import numpy as np
from scipy.signal import butter, lfilter, sosfilt


BANDS = ((45.0, 140.0), (140.0, 400.0))
FAST_TAU_S = 0.003
SLOW_TAU_S = 0.080


def _ema(power: np.ndarray, sample_rate: float, tau: float) -> np.ndarray:
    alpha = math.exp(-1.0 / (tau * sample_rate))
    return lfilter([1.0 - alpha], [1.0, -alpha], power)


def stable_features(samples, sample_rate, envelopes, hop):
    """Replace low/body envelopes with causal quadrature power envelopes.

    ``envelopes`` has shape ``(completed_hops, 3, 2)`` and is copied before
    the low and body bands are replaced.  The two columns are fast (3 ms) and
    slow (80 ms) zero-initialized EMAs sampled at each hop end; the mid band
    is preserved unchanged.
    """
    if sample_rate <= 800 or hop <= 0:
        raise ValueError("sample_rate must exceed 800 Hz and hop must be positive")
    env = np.asarray(envelopes, dtype=np.float64)
    if env.ndim != 3 or env.shape[1:] != (3, 2):
        raise ValueError("expected (completed_hops, 3, fast/slow) envelopes")
    if not np.all(np.isfinite(env)) or np.any(env < 0.0):
        raise ValueError("envelopes must contain finite, non-negative values")
    raw_signal = np.asarray(samples)
    if np.iscomplexobj(raw_signal):
        raise ValueError("samples must be real")
    signal = np.asarray(raw_signal, dtype=np.float64)
    if signal.ndim != 1 or not np.all(np.isfinite(signal)):
        raise ValueError("samples must be a finite, real one-dimensional array")
    count = len(env)
    needed = count * hop
    if len(signal) < needed:
        raise ValueError("samples must contain at least one complete hop per envelope")

    result = env.copy()
    if count == 0:
        return result
    signal = signal[:needed]
    sample_index = np.arange(needed, dtype=np.float64)
    for band_index, (low, high) in enumerate(BANDS):
        center = (low + high) / 2.0
        cutoff = (high - low) / 2.0
        sos = butter(4, cutoff, btype="lowpass", fs=sample_rate, output="sos")
        demodulated = signal * np.exp(
            -2j * np.pi * center * sample_index / sample_rate
        )
        quadrature = sosfilt(sos, demodulated)
        power = 2.0 * np.abs(quadrature) ** 2
        for column, tau in enumerate((FAST_TAU_S, SLOW_TAU_S)):
            smoothed = _ema(power, sample_rate, tau)
            result[:, band_index, column] = smoothed[hop - 1 : needed : hop]
    return result
