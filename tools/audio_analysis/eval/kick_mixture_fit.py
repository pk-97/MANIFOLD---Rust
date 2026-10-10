"""Fixed causal spectral mixture helpers for an offline kick experiment."""
from __future__ import annotations

import math

import numpy as np
from scipy.signal import lfilter

from .kick_tonal_experiment import _trailing_frames

FFT_SAMPLES = 2048
LOW_FREQUENCY_HZ = 30.0
HIGH_FREQUENCY_HZ = 2500.0
FFT_BATCH_HOPS = 256
BACKGROUND_TAU_S = 0.080
FAST_TAU_S = 0.003
SLOW_TAU_S = 0.080
REARM_RATIO = 1.2
FIRE_RATIO = 2.0
MIN_POWER = 1e-6
MIN_FIRE_GAP_S = 0.060


def _validate_spectra(spectra: np.ndarray) -> np.ndarray:
    values = np.asarray(spectra, dtype=np.float64)
    if values.ndim != 2:
        raise ValueError("spectra must be a two-dimensional array")
    if not np.all(np.isfinite(values)) or np.any(values < 0.0):
        raise ValueError("spectra must be finite and nonnegative")
    return values


def causal_spectra(samples: np.ndarray, sample_rate: int, hop: int) -> np.ndarray:
    """Return magnitude spectra for completed causal trailing hops."""
    if sample_rate <= 0 or hop <= 0:
        raise ValueError("sample_rate and hop must be positive")
    values = np.asarray(samples, dtype=np.float64)
    if values.ndim != 1:
        raise ValueError("samples must be a one-dimensional array")
    if not np.all(np.isfinite(values)):
        raise ValueError("samples must be finite")
    count = len(values) // hop
    if count == 0:
        frequencies = np.fft.rfftfreq(FFT_SAMPLES, 1.0 / sample_rate)
        bins = (frequencies >= LOW_FREQUENCY_HZ) & (frequencies <= HIGH_FREQUENCY_HZ)
        return np.zeros((0, int(np.count_nonzero(bins))), dtype=np.float64)
    window = np.hanning(FFT_SAMPLES)
    frequencies = np.fft.rfftfreq(FFT_SAMPLES, 1.0 / sample_rate)
    bins = (frequencies >= LOW_FREQUENCY_HZ) & (frequencies <= HIGH_FREQUENCY_HZ)
    spectra = np.empty((count, int(np.count_nonzero(bins))), dtype=np.float64)
    scale = 2.0 / float(np.sum(window))
    for batch_start in range(0, count, FFT_BATCH_HOPS):
        batch_count = min(FFT_BATCH_HOPS, count - batch_start)
        frames = _trailing_frames(values, batch_start, batch_count, hop, FFT_SAMPLES)
        magnitudes = np.abs(np.fft.rfft(frames * window[None, :], axis=1)) * scale
        spectra[batch_start : batch_start + batch_count] = magnitudes[:, bins]
    return spectra


def _previous_ema(spectra: np.ndarray, sample_rate: int, hop: int, tau_s: float) -> np.ndarray:
    if sample_rate <= 0 or hop <= 0 or tau_s <= 0.0:
        raise ValueError("sample_rate, hop, and tau_s must be positive")
    if len(spectra) == 0:
        return np.zeros_like(spectra)
    dt = hop / float(sample_rate)
    alpha = math.exp(-dt / tau_s)
    filtered = lfilter([1.0 - alpha], [1.0, -alpha], spectra, axis=0)
    return np.vstack((np.zeros((1, spectra.shape[1]), dtype=np.float64), filtered[:-1]))


def _one_column_error(observations: np.ndarray, column: np.ndarray) -> np.ndarray:
    denominator = np.sum(column * column, axis=1)
    numerator = np.sum(observations * column, axis=1)
    coefficients = np.divide(numerator, denominator, out=np.zeros_like(numerator), where=denominator > 0.0)
    coefficients = np.maximum(coefficients, 0.0)
    residual = observations - coefficients[:, None] * column
    return np.sum(residual * residual, axis=1)


def nnls_improvement(
    spectra: np.ndarray, background: np.ndarray, template: np.ndarray
) -> np.ndarray:
    """Return squared-error decrease from adding a nonnegative kick column.

    The minimum is taken over exact one-column NNLS fits and a feasible
    unconstrained two-column solution.  Singular two-column systems are
    explicitly represented by their one-column candidates.
    """
    observations = _validate_spectra(spectra)
    columns = np.asarray(background, dtype=np.float64)
    kick = np.asarray(template, dtype=np.float64)
    if columns.ndim == 1:
        columns = np.broadcast_to(columns, observations.shape)
    if columns.shape != observations.shape:
        raise ValueError("background must have shape (frames, bins) or (bins,)")
    if kick.ndim != 1 or kick.shape[0] != observations.shape[1]:
        raise ValueError("template must have one value per frequency bin")
    if not np.all(np.isfinite(columns)) or np.any(columns < 0.0):
        raise ValueError("background must be finite and nonnegative")
    if not np.all(np.isfinite(kick)) or np.any(kick < 0.0):
        raise ValueError("template must be finite and nonnegative")
    norm = float(np.linalg.norm(kick))
    if norm <= 0.0:
        raise ValueError("template must have positive norm")
    kick = kick / norm
    background_error = _one_column_error(observations, columns)
    kick_column = np.broadcast_to(kick, observations.shape)
    kick_error = _one_column_error(observations, kick_column)
    best_error = np.minimum(background_error, kick_error)

    bb = np.sum(columns * columns, axis=1)
    bk = np.sum(columns * kick_column, axis=1)
    kk = float(np.dot(kick, kick))
    bs = np.sum(columns * observations, axis=1)
    ks = np.sum(kick_column * observations, axis=1)
    determinant = bb * kk - bk * bk
    nonsingular = determinant > 16 * np.finfo(np.float64).eps * bb * kk
    coefficients_background = np.zeros_like(bs)
    coefficients_kick = np.zeros_like(ks)
    coefficients_background[nonsingular] = (
        bs[nonsingular] * kk - ks[nonsingular] * bk[nonsingular]
    ) / determinant[nonsingular]
    coefficients_kick[nonsingular] = (
        ks[nonsingular] * bb[nonsingular] - bs[nonsingular] * bk[nonsingular]
    ) / determinant[nonsingular]
    feasible = nonsingular & (coefficients_background >= 0.0) & (coefficients_kick >= 0.0)
    if np.any(feasible):
        residual = observations[feasible] - (
            coefficients_background[feasible, None] * columns[feasible]
            + coefficients_kick[feasible, None] * kick
        )
        best_error[feasible] = np.minimum(best_error[feasible], np.sum(residual * residual, axis=1))
    improvement = background_error - best_error
    scale = np.maximum(background_error, 1.0)
    materially_negative = improvement < -1e-10 * scale
    if np.any(materially_negative):
        raise FloatingPointError("NNLS improvement became materially negative")
    return np.maximum(improvement, 0.0)


def explained_kick_power(
    spectra: np.ndarray, template: np.ndarray, sample_rate: int, hop: int
) -> np.ndarray:
    """Measure per-hop kick power explained beyond a strictly prior EMA background."""
    values = _validate_spectra(spectra)
    background = _previous_ema(values, sample_rate, hop, BACKGROUND_TAU_S)
    return nnls_improvement(values, background, template)


def _ema(values: np.ndarray, sample_rate: int, hop: int, tau_s: float) -> np.ndarray:
    dt = hop / float(sample_rate)
    alpha = math.exp(-dt / tau_s)
    return lfilter([1.0 - alpha], [1.0, -alpha], values)


def detect_activation(power: np.ndarray, sample_rate: int, hop: int) -> list[int]:
    """Detect fixed-ratio causal activations on the completed-hop grid."""
    values = np.asarray(power, dtype=np.float64)
    if values.ndim != 1:
        raise ValueError("power must be one-dimensional")
    if not np.all(np.isfinite(values)) or np.any(values < 0.0):
        raise ValueError("power must be finite and nonnegative")
    if sample_rate <= 0 or hop <= 0:
        raise ValueError("sample_rate and hop must be positive")
    fast = _ema(values, sample_rate, hop, FAST_TAU_S)
    slow = _ema(values, sample_rate, hop, SLOW_TAU_S)
    ratio = fast / (slow + 1e-12)
    min_gap = MIN_FIRE_GAP_S * sample_rate / hop
    armed = True
    last_fire = -math.inf
    fires: list[int] = []
    for index, (fast_value, ratio_value) in enumerate(zip(fast, ratio)):
        if ratio_value < REARM_RATIO:
            armed = True
        if (
            armed
            and fast_value > MIN_POWER
            and ratio_value > FIRE_RATIO
            and index - last_fire >= min_gap
        ):
            fires.append(index)
            last_fire = index
            armed = False
    return fires


__all__ = [
    "causal_spectra",
    "detect_activation",
    "explained_kick_power",
    "nnls_improvement",
]
