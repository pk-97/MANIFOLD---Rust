"""Frame-level spectral analysis: FFT, band energy, onset detection, spectral shape."""

from __future__ import annotations

from typing import Dict, Tuple

import numpy as np

from manifold_audio.audio_io import frame_signal
from manifold_audio.models import FrameAnalysis


def compute_frame_analysis(
    signal: np.ndarray,
    sample_rate: int,
    frame_size: int,
    hop_size: int,
    bands_hz: Dict[str, Tuple[float, float]],
    norm_window_sec: float = 3.0,
) -> FrameAnalysis:
    frames = frame_signal(signal, frame_size, hop_size)
    window = np.hanning(frame_size).astype(np.float32)
    windowed = frames * window[None, :]
    spectrum = np.fft.rfft(windowed, axis=1)
    mag = np.abs(spectrum).astype(np.float32)
    power = (mag * mag).astype(np.float32)
    freqs = np.fft.rfftfreq(frame_size, d=1.0 / sample_rate)
    nyquist = max(1.0, float(sample_rate) * 0.5)

    band_energy: Dict[str, np.ndarray] = {}
    for name, (f_lo, f_hi) in bands_hz.items():
        mask = (freqs >= f_lo) & (freqs < f_hi)
        if not np.any(mask):
            band_energy[name] = np.zeros(power.shape[0], dtype=np.float32)
            continue
        band_energy[name] = power[:, mask].sum(axis=1).astype(np.float32)

    band_onsets: Dict[str, np.ndarray] = {}
    kernel = np.array([0.2, 0.6, 0.2], dtype=np.float32)
    for name, (f_lo, f_hi) in bands_hz.items():
        mask = (freqs >= f_lo) & (freqs < f_hi)
        if not np.any(mask):
            band_onsets[name] = np.zeros(mag.shape[0], dtype=np.float32)
            continue
        band_mag = mag[:, mask]
        # Spectral flux: sum of positive per-bin magnitude differences.
        # Detects spectral change (transients) rather than just energy change,
        # reducing false positives from gradual ramps and filter sweeps.
        diff = np.diff(band_mag, axis=0, prepend=band_mag[:1, :])
        flux = np.maximum(diff, 0.0).sum(axis=1)
        band_onsets[name] = np.convolve(flux, kernel, mode="same").astype(np.float32)

    mag_sum = mag.sum(axis=1) + 1e-6
    centroid = (mag * freqs[None, :]).sum(axis=1) / mag_sum

    cum_mag = np.cumsum(mag, axis=1)
    targets = 0.85 * mag_sum
    rolloff_idx = np.array([int(np.searchsorted(cum_mag[i], targets[i], side="left")) for i in range(cum_mag.shape[0])])
    rolloff_idx = np.clip(rolloff_idx, 0, freqs.shape[0] - 1)
    rolloff_hz = freqs[rolloff_idx]

    gm = np.exp(np.mean(np.log(mag + 1e-8), axis=1))
    am = np.mean(mag + 1e-8, axis=1)
    flatness = np.clip(gm / (am + 1e-8), 0.0, 1.0).astype(np.float32)

    rms = np.sqrt(np.mean(np.square(frames), axis=1)).astype(np.float32)

    # Local loudness normalization: divide band envelopes by smoothed RMS so
    # onset strength reflects spectral activity relative to local loudness,
    # not absolute amplitude.  A kick in a quiet breakdown scores comparably
    # to the same kick in a loud drop.
    if norm_window_sec > 0 and rms.size >= 3:
        hop_time = hop_size / float(sample_rate)
        norm_half = max(1, int(round(norm_window_sec / hop_time)))
        norm_win = 2 * norm_half + 1
        norm_kernel = np.ones(norm_win, dtype=np.float32) / norm_win
        rms_smooth = np.convolve(rms, norm_kernel, mode="same").astype(np.float32)
        rms_floor = max(float(np.median(rms)) * 0.02, 1e-8)
        rms_ref = np.maximum(rms_smooth, rms_floor)
        for name in band_onsets:
            band_onsets[name] = (band_onsets[name] / rms_ref).astype(np.float32)
        for name in band_energy:
            band_energy[name] = (band_energy[name] / rms_ref).astype(np.float32)

    return FrameAnalysis(
        band_energy=band_energy,
        band_onsets=band_onsets,
        centroid_norm=np.clip(centroid / nyquist, 0.0, 1.0).astype(np.float32),
        rolloff85_norm=np.clip(rolloff_hz / nyquist, 0.0, 1.0).astype(np.float32),
        flatness=flatness,
        rms=rms,
    )


def compute_band_onsets(
    signal: np.ndarray,
    sample_rate: int,
    frame_size: int,
    hop_size: int,
    bands_hz: Dict[str, Tuple[float, float]],
    norm_window_sec: float = 3.0,
) -> Tuple[Dict[str, np.ndarray], np.ndarray, int]:
    """Return (band_onsets, raw_rms, hop_size).

    raw_rms is the pre-normalization broadband RMS at hop resolution,
    suitable for computing per-beat energy envelopes downstream.
    """
    fa = compute_frame_analysis(
        signal=signal,
        sample_rate=sample_rate,
        frame_size=frame_size,
        hop_size=hop_size,
        bands_hz=bands_hz,
        norm_window_sec=norm_window_sec,
    )
    return fa.band_onsets, fa.rms, hop_size
