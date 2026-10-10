"""Causal upper-band and low-pulse cue diagnostics for kick review."""
from __future__ import annotations

import math

import numpy as np
from scipy.signal import butter, lfilter, sosfilt

BANDS = ((45.0, 140.0), (1000.0, 2000.0), (2000.0, 4000.0), (4000.0, 8000.0))
FAST_TAU_S = 0.003
SLOW_TAU_S = 0.080


def _validate_samples(samples: np.ndarray, sample_rate: int) -> np.ndarray:
    if sample_rate <= 16_000:
        raise ValueError("sample_rate must exceed 16000 Hz")
    values = np.asarray(samples, dtype=np.float64)
    if values.ndim != 1:
        raise ValueError("samples must be a one-dimensional mono array")
    if not np.all(np.isfinite(values)):
        raise ValueError("samples must be finite")
    return values


def upper_features(samples: np.ndarray, sample_rate: int) -> tuple[np.ndarray, int]:
    """Return causal fast/slow power envelopes for four fixed native-rate bands."""
    values = _validate_samples(samples, sample_rate)
    hop = max(1, round(sample_rate * 256 / 48_000))
    count = len(values) // hop
    bands = []
    for low, high in BANDS:
        filtered = sosfilt(
            butter(2, [low, high], btype="bandpass", fs=sample_rate, output="sos"),
            values,
        )
        power = filtered * filtered
        envelopes = []
        for tau in (FAST_TAU_S, SLOW_TAU_S):
            alpha = math.exp(-1.0 / (tau * sample_rate))
            smoothed = lfilter([1.0 - alpha], [1.0, -alpha], power)
            envelopes.append(smoothed[hop - 1 : count * hop : hop])
        bands.append(np.stack(envelopes, axis=1))
    result = np.stack(bands, axis=1) if count else np.zeros((0, 4, 2), dtype=np.float64)
    return result, hop


def _validate_envelopes(envelopes: np.ndarray, sample_rate: int, hop: int) -> np.ndarray:
    if sample_rate <= 0 or hop <= 0:
        raise ValueError("sample_rate and hop must be positive")
    values = np.asarray(envelopes, dtype=np.float64)
    if values.ndim != 3 or values.shape[1:] != (4, 2):
        raise ValueError("envelopes must have shape (hops, 4, 2)")
    if not np.all(np.isfinite(values)) or np.any(values < 0.0):
        raise ValueError("envelopes must be finite and nonnegative")
    return values


def inspect_cues(envelopes: np.ndarray, sample_rate: int, hop: int) -> list[dict[str, object]]:
    """Report upper rising edges with fixed-horizon low linkage and decay."""
    values = _validate_envelopes(envelopes, sample_rate, hop)
    if len(values) == 0:
        return []
    low = values[:, 0]
    upper = values[:, 1:]
    upper_ratios = upper[:, :, 0] / (upper[:, :, 1] + 1e-12)
    upper_count = np.sum(upper_ratios > 2.0, axis=1)
    upper_fast = np.sum(upper[:, :, 0], axis=1)
    upper_active = (upper_count >= 2) & (upper_fast > 1e-6)
    low_active = (low[:, 0] > 1e-6) & (low[:, 0] > low[:, 1] * 1.2)
    upper_rising = upper_active & ~np.concatenate(([False], upper_active[:-1]))
    low_rising = low_active & ~np.concatenate(([False], low_active[:-1]))

    before = math.floor(0.010 * sample_rate / hop)
    after = math.floor(0.035 * sample_rate / hop)
    deadline_offset = math.ceil(0.050 * sample_rate / hop)
    decay_offset = math.floor(0.015 * sample_rate / hop)
    rows: list[dict[str, object]] = []
    for index in np.flatnonzero(upper_rising):
        index = int(index)
        deadline = index + deadline_offset
        if deadline >= len(values):
            continue
        search_start = max(0, index - before)
        search_end = min(len(values), index + after + 1)
        linked = np.flatnonzero(low_rising[search_start:search_end])
        low_hop = int(search_start + linked[0]) if len(linked) else None
        denominator = max(float(np.max(upper_fast[index : index + decay_offset + 1])), 1e-12)
        decay_ratio = float(upper_fast[deadline] / denominator)
        decayed = decay_ratio <= 0.5
        rows.append(
            {
                "upper_hop": index,
                "available_hop": deadline,
                "low_hop": low_hop,
                "upper_bands_above_threshold": int(upper_count[index]),
                "low_linked": low_hop is not None,
                "upper_decay_ratio": decay_ratio,
                "decayed": bool(decayed),
                "linked_and_decayed": bool(low_hop is not None and decayed),
            }
        )
    return rows


__all__ = ["inspect_cues", "upper_features"]
