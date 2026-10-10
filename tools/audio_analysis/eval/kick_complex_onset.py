"""Causal complex-domain spectral prediction for onset evaluation.

This module contains the measurement described by Duxbury et al. (DAFx 2003),
without a detector, post-processing, or threshold.  It is intentionally an
evaluation-only feature extractor; the Rust runtime is not changed here.
"""

from __future__ import annotations

import operator

import numpy as np

from eval.kick_tonal_experiment import _fft_size, _trailing_frames


LOW_FREQUENCY_HZ = 45.0
HIGH_FREQUENCY_HZ = 2000.0
DEFAULT_BATCH_HOPS = 256
NORMALIZED_DENOMINATOR_FLOOR = 1e-12


def _prediction_error(current, previous, previous_previous):
    prediction = np.abs(previous) * np.exp(
        1j * (2.0 * np.angle(previous) - np.angle(previous_previous))
    )
    return np.abs(current - prediction)


def _positive_integer(value: object, name: str) -> int:
    """Return a positive integer argument, with a useful error for callers."""
    try:
        integer = operator.index(value)
    except TypeError as exc:
        raise ValueError(f"{name} must be a positive integer") from exc
    if integer <= 0:
        raise ValueError(f"{name} must be a positive integer")
    return int(integer)


def _finite_real_mono(samples: np.ndarray) -> np.ndarray:
    """Validate and return samples as a finite float64 mono signal."""
    try:
        raw = np.asarray(samples)
    except (TypeError, ValueError) as exc:
        raise ValueError("samples must be a finite, real mono signal") from exc
    if raw.ndim != 1 or np.iscomplexobj(raw):
        raise ValueError("samples must be a finite, real mono signal")
    try:
        values = np.asarray(raw, dtype=np.float64)
    except (TypeError, ValueError) as exc:
        raise ValueError("samples must be a finite, real mono signal") from exc
    if not np.all(np.isfinite(values)):
        raise ValueError("samples must be a finite, real mono signal")
    return values


def complex_features(
    samples: np.ndarray,
    sample_rate: int,
    hop: int,
    batch_hops: int = DEFAULT_BATCH_HOPS,
) -> dict[str, np.ndarray]:
    """Return causal complex spectral-prediction measurements on the hop grid.

    The trailing Hann-windowed FFT frame ends at each completed hop, so a
    result at hop ``i`` only uses samples before ``(i + 1) * hop``.  The first
    two outputs are zero because the predictor needs the two preceding
    spectra.  ``normalized`` is the in-band prediction error divided by the
    current-plus-previous in-band magnitude; values are bounded to ``[0, 1]``
    by the triangle inequality.  If that denominator is at most
    ``1e-12`` after dividing by the Hann window sum, the normalized value is
    defined as zero.

    ``raw`` is the same in-band error sum divided by the Hann window sum.  The
    frequency range is fixed at 45 through 2000 Hz, inclusive, for both
    outputs.
    """
    values = _finite_real_mono(samples)
    sample_rate_int = _positive_integer(sample_rate, "sample_rate")
    if sample_rate_int <= 4000:
        raise ValueError("sample_rate must exceed 4000 Hz for the 2 kHz band")
    hop_int = _positive_integer(hop, "hop")
    batch_hops_int = _positive_integer(batch_hops, "batch_hops")

    count = len(values) // hop_int
    raw_output = np.zeros(count, dtype=np.float64)
    normalized_output = np.zeros(count, dtype=np.float64)
    if count == 0:
        return {"raw": raw_output, "normalized": normalized_output}

    fft_size = _fft_size(sample_rate_int)
    window = np.hanning(fft_size)
    window_sum = float(window.sum())
    frequencies = np.fft.rfftfreq(fft_size, d=1.0 / sample_rate_int)
    band_mask = (frequencies >= LOW_FREQUENCY_HZ) & (
        frequencies <= HIGH_FREQUENCY_HZ
    )

    previous_previous: np.ndarray | None = None
    previous: np.ndarray | None = None
    for batch_start in range(0, count, batch_hops_int):
        batch_count = min(batch_hops_int, count - batch_start)
        frames = _trailing_frames(
            values, batch_start, batch_count, hop_int, fft_size
        )
        spectra = np.fft.rfft(frames * window[None, :], axis=1)[:, band_mask]
        for offset, current in enumerate(spectra):
            hop_index = batch_start + offset
            if previous_previous is not None and previous is not None:
                error = _prediction_error(current, previous, previous_previous)
                current_magnitude = np.abs(current)
                previous_magnitude = np.abs(previous)
                error_sum = float(error.sum())
                denominator = float(
                    (current_magnitude + previous_magnitude).sum()
                )
                raw_output[hop_index] = error_sum / window_sum
                if denominator / window_sum > NORMALIZED_DENOMINATOR_FLOOR:
                    normalized_output[hop_index] = np.clip(
                        error_sum / denominator, 0.0, 1.0
                    )
            previous_previous = previous
            previous = current

    return {"raw": raw_output, "normalized": normalized_output}
