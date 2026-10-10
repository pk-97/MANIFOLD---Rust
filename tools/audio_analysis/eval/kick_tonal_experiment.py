"""Causal SuperFlux-style tonal rejection experiment for kick detection.

This module computes a deliberately small, fixed tonal confirmation mask and
passes it to the v5 detector.  It is an evaluation experiment only; the Rust
detector is not changed here.
"""

from __future__ import annotations

import importlib
import math

import numpy as np


FFT_SAMPLES_AT_48K = 2048
BANDS_PER_OCTAVE = 24
LOW_FREQUENCY_HZ = 30.0
HIGH_FREQUENCY_HZ = 2000.0
NEIGHBOURHOOD_BANDS = 3
WINDOW_SECONDS = 0.020
SUPERFLUX_RATIO = 0.35
ORDINARY_FLUX_FLOOR = 1e-8
FFT_BATCH_HOPS = 256


def _fft_size(sample_rate: int) -> int:
    if sample_rate <= 0:
        raise ValueError("sample_rate must be positive")
    # Keep the analysis window's physical duration fixed as the sample rate
    # changes.  rfft supports both even and odd lengths.
    return max(2, int(round(FFT_SAMPLES_AT_48K * sample_rate / 48_000)))


def _log_band_centres() -> np.ndarray:
    count = int(math.floor(
        BANDS_PER_OCTAVE * math.log2(HIGH_FREQUENCY_HZ / LOW_FREQUENCY_HZ)
    )) + 1
    return LOW_FREQUENCY_HZ * 2.0 ** (
        np.arange(count, dtype=np.float64) / BANDS_PER_OCTAVE
    )


def _quantized_band_centres(sample_rate: int, fft_size: int) -> np.ndarray:
    """Keep unique native FFT-bin centres inside the requested frequency range.

    At the 2048-sample/48 kHz setting, the native FFT bin spacing is much
    wider than a 24-band/octave step below a few hundred hertz.  Quantizing
    and dropping duplicate centres avoids empty triangles while preserving the
    requested logarithmic target grid as far as the FFT resolution permits.
    """
    bin_width = sample_rate / fft_size
    target_bins = np.rint(_log_band_centres() / bin_width).astype(np.int64)
    frequencies = target_bins * bin_width
    valid = (frequencies >= LOW_FREQUENCY_HZ) & (frequencies <= HIGH_FREQUENCY_HZ)
    return np.unique(target_bins[valid]).astype(np.float64) * bin_width


def _triangular_filterbank(sample_rate: int, fft_size: int) -> np.ndarray:
    """Build the fixed log-frequency triangular bank at native FFT resolution."""
    frequencies = np.fft.rfftfreq(fft_size, d=1.0 / sample_rate)
    centres = _quantized_band_centres(sample_rate, fft_size)
    bank = np.zeros((len(centres), len(frequencies)), dtype=np.float64)
    for band, centre in enumerate(centres):
        left = LOW_FREQUENCY_HZ if band == 0 else centres[band - 1]
        right = HIGH_FREQUENCY_HZ if band + 1 == len(centres) else centres[band + 1]
        rising = (
            (frequencies >= max(LOW_FREQUENCY_HZ, left))
            & (frequencies <= centre)
        )
        falling = (
            (frequencies > centre)
            & (frequencies <= min(HIGH_FREQUENCY_HZ, right))
        )
        bank[band, rising] = (frequencies[rising] - left) / (centre - left)
        bank[band, falling] = (right - frequencies[falling]) / (right - centre)
    return bank


def _trailing_frames(
    samples: np.ndarray,
    start_frame: int,
    frame_count: int,
    hop: int,
    fft_size: int,
) -> np.ndarray:
    """Return one bounded batch of zero-padded causal trailing frames."""
    frames = np.zeros((frame_count, fft_size), dtype=np.float64)
    for offset in range(frame_count):
        end = (start_frame + offset + 1) * hop
        start = max(0, end - fft_size)
        tail = samples[start:end]
        frames[offset, -len(tail):] = tail
    return frames


def flux_features(
    samples: np.ndarray, sample_rate: int, hop: int, frame_count: int | None = None
) -> tuple[np.ndarray, np.ndarray]:
    """Return ``(ordinary_flux, superflux)`` on the causal hop grid.

    Both flux values are zero until two complete hops exist, because each
    decision compares the current spectrum with the spectrum two hops earlier.
    The trailing FFT frame ends at the current hop and therefore never reads a
    future sample.
    """
    if hop <= 0:
        raise ValueError("hop must be positive")
    values = np.asarray(samples, dtype=np.float64)
    if values.ndim != 1:
        raise ValueError("samples must be a one-dimensional array")
    fft_size = _fft_size(int(sample_rate))
    available_hops = len(values) // hop
    if frame_count is not None and frame_count != available_hops:
        raise ValueError(
            "frame_count must equal the number of completed sample hops"
        )
    count = available_hops
    if count <= 0:
        return np.zeros(0, dtype=np.float64), np.zeros(0, dtype=np.float64)

    window = np.hanning(fft_size)
    bank = _triangular_filterbank(int(sample_rate), fft_size)
    log_spectrum = np.empty((count, bank.shape[0]), dtype=np.float64)
    for batch_start in range(0, count, FFT_BATCH_HOPS):
        batch_count = min(FFT_BATCH_HOPS, count - batch_start)
        frames = _trailing_frames(
            values, batch_start, batch_count, hop, fft_size
        )
        magnitude = np.abs(np.fft.rfft(frames * window[None, :], axis=1))
        band_magnitude = magnitude @ bank.T
        log_spectrum[batch_start : batch_start + batch_count] = np.log1p(
            10.0 * band_magnitude
        )

    ordinary = np.zeros(count, dtype=np.float64)
    superflux = np.zeros(count, dtype=np.float64)
    if count < 3:
        return ordinary, superflux

    previous = log_spectrum[:-2]
    current = log_spectrum[2:]
    ordinary[2:] = np.maximum(current - previous, 0.0).sum(axis=1)

    # A three-band frequency neighbourhood (one band on either side) makes a
    # moving narrow tone look stationary when its energy merely changes band.
    if NEIGHBOURHOOD_BANDS < 1 or NEIGHBOURHOOD_BANDS % 2 == 0:
        raise ValueError("NEIGHBOURHOOD_BANDS must be a positive odd number")
    radius = NEIGHBOURHOOD_BANDS // 2
    padded = np.pad(previous, ((0, 0), (radius, radius)), mode="edge")
    neighbourhood_max = np.maximum.reduce(
        tuple(
            padded[:, offset : offset + previous.shape[1]]
            for offset in range(NEIGHBOURHOOD_BANDS)
        )
    )
    superflux[2:] = np.maximum(current - neighbourhood_max, 0.0).sum(axis=1)
    return ordinary, superflux


def confirmation_mask(
    samples: np.ndarray, sample_rate: int, hop: int, frame_count: int | None = None
) -> np.ndarray:
    """Return the fixed causal tonal confirmation mask on the hop grid."""
    return confirmation_diagnostics(samples, sample_rate, hop, frame_count)["mask"]


def confirmation_diagnostics(
    samples: np.ndarray, sample_rate: int, hop: int, frame_count: int | None = None
) -> dict[str, np.ndarray]:
    """Return fluxes, rolling sums, ratio, and mask for experiment inspection."""
    ordinary, superflux = flux_features(samples, sample_rate, hop, frame_count)
    if ordinary.size == 0:
        empty = np.zeros(0, dtype=np.float64)
        return {
            "ordinary_flux": empty,
            "superflux": empty,
            "ordinary_sum": empty,
            "superflux_sum": empty,
            "ratio": empty,
            "mask": np.zeros(0, dtype=bool),
        }
    window_hops = max(1, int(math.ceil(WINDOW_SECONDS * sample_rate / hop)))
    ordinary_prefix = np.concatenate(([0.0], np.cumsum(ordinary)))
    superflux_prefix = np.concatenate(([0.0], np.cumsum(superflux)))
    starts = np.maximum(np.arange(ordinary.size) + 1 - window_hops, 0)
    ends = np.arange(ordinary.size) + 1
    ordinary_sum = ordinary_prefix[ends] - ordinary_prefix[starts]
    superflux_sum = superflux_prefix[ends] - superflux_prefix[starts]
    ratio = np.divide(
        superflux_sum,
        ordinary_sum,
        out=np.zeros_like(superflux_sum),
        where=ordinary_sum > 0.0,
    )
    mask = (ordinary_sum > ORDINARY_FLUX_FLOOR) & (ratio >= SUPERFLUX_RATIO)
    return {
        "ordinary_flux": ordinary,
        "superflux": superflux,
        "ordinary_sum": ordinary_sum,
        "superflux_sum": superflux_sum,
        "ratio": ratio,
        "mask": mask,
    }


def _baseline_detector():
    try:
        return importlib.import_module("eval.kick_dsp_experiments").detect_v5
    except (ImportError, AttributeError) as exc:
        raise ImportError(
            "eval.kick_dsp_experiments.detect_v5 is required for tonal detection"
        ) from exc


def detect(
    samples: np.ndarray, sample_rate: int, envelopes: np.ndarray, hop: int
) -> list[int]:
    """Run v5 with the causal tonal confirmation mask and return fire hops."""
    envelope_count = len(envelopes)
    mask = confirmation_mask(samples, sample_rate, hop, envelope_count)
    return [
        int(index)
        for index in _baseline_detector()(
            envelopes, sample_rate, hop, confirmation_mask=mask
        )
    ]


__all__ = [
    "BANDS_PER_OCTAVE",
    "HIGH_FREQUENCY_HZ",
    "LOW_FREQUENCY_HZ",
    "ORDINARY_FLUX_FLOOR",
    "SUPERFLUX_RATIO",
    "confirmation_diagnostics",
    "confirmation_mask",
    "detect",
    "flux_features",
]
