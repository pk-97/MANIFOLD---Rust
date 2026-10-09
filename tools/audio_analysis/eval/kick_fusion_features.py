"""Fixed causal feature extraction for a non-neural kick fusion trial."""
from __future__ import annotations

import math

import numpy as np

from .kick_attack_rejection import causal_features
from .kick_tonal_experiment import _trailing_frames
from .kick_upper_cue import upper_features

FEATURE_NAMES = (
    "low_log_rise",
    "body_log_rise",
    "upper_log_rise",
    "relative_flux",
    "centroid_drop",
    "upper_log_decay",
    "body_low_log_balance",
    "low_log_evolution",
    "peak_lag",
)
FFT_SAMPLES = 2048
LOW_FREQUENCY_HZ = 30.0
HIGH_FREQUENCY_HZ = 8000.0
FFT_BATCH_HOPS = 256
EPSILON = 1e-12


def _validate_samples(samples: np.ndarray, sample_rate: int) -> np.ndarray:
    if sample_rate <= 16_000:
        raise ValueError("sample_rate must exceed 16000 Hz")
    values = np.asarray(samples, dtype=np.float64)
    if values.ndim != 1:
        raise ValueError("samples must be one-dimensional mono data")
    if not np.all(np.isfinite(values)):
        raise ValueError("samples must be finite")
    return values


def _spectra(samples: np.ndarray, sample_rate: int, hop: int) -> np.ndarray:
    count = len(samples) // hop
    frequencies = np.fft.rfftfreq(FFT_SAMPLES, 1.0 / sample_rate)
    bins = (frequencies >= LOW_FREQUENCY_HZ) & (frequencies <= HIGH_FREQUENCY_HZ)
    result = np.empty((count, int(np.count_nonzero(bins))), dtype=np.float64)
    if count == 0:
        return result
    window = np.hanning(FFT_SAMPLES)
    for batch_start in range(0, count, FFT_BATCH_HOPS):
        batch_count = min(FFT_BATCH_HOPS, count - batch_start)
        frames = _trailing_frames(samples, batch_start, batch_count, hop, FFT_SAMPLES)
        result[batch_start : batch_start + batch_count] = np.abs(
            np.fft.rfft(frames * window[None, :], axis=1)
        )[:, bins]
    return result


def _clip_log(value: float) -> float:
    return float(np.clip(value, -6.0, 6.0))


def _rise_edges(envelopes: np.ndarray, upper_envelopes: np.ndarray) -> np.ndarray:
    low = envelopes[:, 0]
    body = envelopes[:, 1]
    upper = upper_envelopes[:, 1:]
    low_active = (low[:, 0] > 1e-6) & (low[:, 0] > 1.2 * low[:, 1])
    body_active = (body[:, 0] > 1e-6) & (body[:, 0] / (body[:, 1] + EPSILON) > 2.0)
    upper_count = np.sum(upper[:, :, 0] / (upper[:, :, 1] + EPSILON) > 2.0, axis=1)
    upper_active = (np.sum(upper[:, :, 0], axis=1) > 1e-6) & (upper_count >= 2)
    def rising(active):
        return active & ~np.concatenate(([False], active[:-1]))
    return rising(low_active) | rising(body_active) | rising(upper_active)


def _window_bounds(index: int, deadline: int, hop: int, sample_rate: int) -> tuple[int, int, int, int]:
    span = math.floor(0.015 * sample_rate / hop)
    first_start, first_end = index, min(deadline, index + span)
    last_start, last_end = max(index, deadline - span), deadline
    return first_start, first_end, last_start, last_end


def fusion_features(
    samples: np.ndarray, sample_rate: int, deadline_s: float = 0.040
) -> tuple[np.ndarray, np.ndarray, np.ndarray, int]:
    """Return candidate hops, deadline hops, fixed features, and native hop size."""
    values = _validate_samples(samples, sample_rate)
    hop = max(1, round(sample_rate * 256 / 48_000))
    if len(values) < hop:
        return (np.zeros(0, dtype=np.int64), np.zeros(0, dtype=np.int64),
                np.zeros((0, len(FEATURE_NAMES))), hop)
    envelopes, hop = causal_features(values, sample_rate)
    upper, upper_hop = upper_features(values, sample_rate)
    if hop != upper_hop or len(envelopes) != len(upper):
        raise ValueError("causal envelope hop grids do not agree")
    count = len(envelopes)
    if count == 0:
        return (
            np.zeros(0, dtype=np.int64),
            np.zeros(0, dtype=np.int64),
            np.zeros((0, len(FEATURE_NAMES)), dtype=np.float64),
            hop,
        )
    edges = _rise_edges(envelopes, upper)
    # The upper helper uses bands 1000-2000, 2000-4000, and 4000-8000.
    upper_fast = np.sum(upper[:, 1:, 0], axis=1)
    upper_slow = np.sum(upper[:, 1:, 1], axis=1)
    deadline_offset = math.ceil(deadline_s * sample_rate / hop)
    candidates = np.flatnonzero(edges)
    candidates = candidates[candidates + deadline_offset < count]
    available = candidates + deadline_offset
    spectra = _spectra(values, sample_rate, hop)
    spectrum_sum = np.sum(spectra, axis=1)
    flux = np.zeros(count, dtype=np.float64)
    flux[0] = spectrum_sum[0]
    if count > 1:
        flux[1:] = np.sum(np.maximum(spectra[1:] - spectra[:-1], 0.0), axis=1)
    relative_flux = np.divide(flux, spectrum_sum + EPSILON)
    frequencies = np.fft.rfftfreq(FFT_SAMPLES, 1.0 / sample_rate)
    frequency_mask = (frequencies >= LOW_FREQUENCY_HZ) & (frequencies <= HIGH_FREQUENCY_HZ)
    log_frequencies = np.log2(frequencies[frequency_mask])
    centroid = np.divide(
        np.sum(spectra * log_frequencies[None, :], axis=1),
        spectrum_sum + EPSILON,
    )
    rows = np.empty((len(candidates), len(FEATURE_NAMES)), dtype=np.float64)
    for row, (index, deadline) in enumerate(zip(candidates, available)):
        index, deadline = int(index), int(deadline)
        first_start, first_end, last_start, last_end = _window_bounds(
            index, deadline, hop, sample_rate
        )
        first = slice(first_start, first_end + 1)
        last = slice(last_start, last_end + 1)
        whole = slice(index, deadline + 1)
        low_fast = envelopes[:, 0, 0]
        low_slow = envelopes[:, 0, 1]
        body_fast = envelopes[:, 1, 0]
        body_slow = envelopes[:, 1, 1]
        low_rise = np.max(np.log((low_fast[whole] + EPSILON) / (low_slow[whole] + EPSILON)))
        body_rise = np.max(np.log((body_fast[whole] + EPSILON) / (body_slow[whole] + EPSILON)))
        upper_rise = np.max(np.log((upper_fast[whole] + EPSILON) / (upper_slow[whole] + EPSILON)))
        centroid_drop = float(np.mean(centroid[first]) - np.mean(centroid[last]))
        upper_decay = np.log((upper_fast[deadline] + EPSILON) / (np.max(upper_fast[first]) + EPSILON))
        balance = np.log((np.max(body_fast[whole]) + EPSILON) / (np.max(low_fast[whole]) + EPSILON))
        evolution = np.log((np.mean(low_fast[last]) + EPSILON) / (np.mean(low_fast[first]) + EPSILON))
        low_peak = int(np.argmax(low_fast[whole]))
        upper_peak = int(np.argmax(upper_fast[whole]))
        peak_lag = abs(low_peak - upper_peak) * hop / sample_rate / 0.040
        rows[row] = (
            _clip_log(float(low_rise)),
            _clip_log(float(body_rise)),
            _clip_log(float(upper_rise)),
            float(np.clip(np.max(relative_flux[whole]), 0.0, 1.0)),
            float(np.clip(centroid_drop, -6.0, 6.0)),
            _clip_log(float(upper_decay)),
            _clip_log(float(balance)),
            _clip_log(float(evolution)),
            float(np.clip(peak_lag, 0.0, 1.0)),
        )
    return candidates.astype(np.int64), available.astype(np.int64), rows, hop


__all__ = ["FEATURE_NAMES", "fusion_features"]
