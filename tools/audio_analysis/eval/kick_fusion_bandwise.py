"""Frozen kick fusion features plus six causal band-local spectral features."""
from __future__ import annotations

import numpy as np

from . import kick_fusion_features as base


BANDS = ((30.0, 140.0), (140.0, 1000.0), (1000.0, 8000.0))
FEATURE_NAMES = base.FEATURE_NAMES + (
    "low_relative_flux",
    "low_centroid_drop",
    "body_relative_flux",
    "body_centroid_drop",
    "upper_relative_flux",
    "upper_centroid_drop",
)


def _bandwise_features(
    spectra: np.ndarray,
    sample_rate: int,
    candidates: np.ndarray,
    available: np.ndarray,
    hop: int,
) -> np.ndarray:
    """Reduce the frozen trailing spectra on the unchanged candidate windows."""
    frequencies = np.fft.rfftfreq(base.FFT_SAMPLES, 1.0 / sample_rate)
    frequencies = frequencies[
        (frequencies >= base.LOW_FREQUENCY_HZ)
        & (frequencies <= base.HIGH_FREQUENCY_HZ)
    ]
    rows = np.zeros((len(candidates), 6), dtype=np.float64)
    for band_index, (low, high) in enumerate(BANDS):
        # Adjacent bands are disjoint; the final band includes the 8 kHz bin.
        mask = (frequencies >= low) & (
            (frequencies <= high) if band_index == len(BANDS) - 1
            else (frequencies < high)
        )
        band_spectra = spectra[:, mask]
        spectrum_sum = np.sum(band_spectra, axis=1)
        flux = spectrum_sum.copy()
        flux[1:] = np.sum(
            np.maximum(band_spectra[1:] - band_spectra[:-1], 0.0), axis=1
        )
        relative_flux = flux / (spectrum_sum + base.EPSILON)
        centroid = np.sum(
            band_spectra * np.log2(frequencies[mask])[None, :], axis=1
        ) / (spectrum_sum + base.EPSILON)
        for row, (index, deadline) in enumerate(zip(candidates, available)):
            index, deadline = int(index), int(deadline)
            first_start, first_end, last_start, last_end = base._window_bounds(
                index, deadline, hop, sample_rate
            )
            drop = (
                np.mean(centroid[first_start : first_end + 1])
                - np.mean(centroid[last_start : last_end + 1])
            )
            rows[row, band_index * 2] = np.clip(
                np.max(relative_flux[index : deadline + 1]), 0.0, 1.0
            )
            rows[row, band_index * 2 + 1] = np.clip(drop, -6.0, 6.0)
    return rows


def fusion_features(
    samples: np.ndarray, sample_rate: int
) -> tuple[np.ndarray, np.ndarray, np.ndarray, int]:
    """Return the frozen grid and nine columns followed by six bandwise columns.

    Each band contributes maximum positive flux divided by its current magnitude
    sum, then mean log2-frequency centroid over the first 15 ms minus the last
    15 ms. Both reductions use the frozen inclusive candidate/deadline bounds.
    The shared 2048-sample Hann FFT is trailing; no deadline is extended.
    """
    candidates, available, features, hop = base.fusion_features(samples, sample_rate)
    if len(candidates) == 0:
        return candidates, available, np.zeros((0, len(FEATURE_NAMES))), hop
    spectra = base._spectra(np.asarray(samples, dtype=np.float64), sample_rate, hop)
    bandwise = _bandwise_features(spectra, sample_rate, candidates, available, hop)
    return candidates, available, np.concatenate((features, bandwise), axis=1), hop


__all__ = ["FEATURE_NAMES", "fusion_features"]
