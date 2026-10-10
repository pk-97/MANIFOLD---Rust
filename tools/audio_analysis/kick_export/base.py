"""Kick model export hook for the base piece: candidates and the 15 base features.

Rust reader: crates/manifold-audio/src/kick/base/. Reference: tools/audio_analysis/eval/kick_fusion_bandwise.py
over kick_fusion_features.py, kick_attack_rejection.causal_features and kick_upper_cue.upper_features. Every
value is computed by the reference's own constants and calls.
"""
from __future__ import annotations

import math
import sys
from pathlib import Path

import numpy as np
from scipy.signal import butter

_ROOT = Path(__file__).resolve().parents[3]
if str(_ROOT) not in sys.path:
    sys.path.append(str(_ROOT))

from tools.audio_analysis.eval import kick_attack_rejection as attack  # noqa: E402
from tools.audio_analysis.eval import kick_fusion_bandwise as bandwise  # noqa: E402
from tools.audio_analysis.eval import kick_fusion_features as base  # noqa: E402
from tools.audio_analysis.eval import kick_upper_cue as upper  # noqa: E402

SR = 48000


def _sos(low, high):
    return butter(2, [low, high], btype='bandpass', fs=SR, output='sos')


def model_entries(final: dict) -> dict[str, np.ndarray]:
    del final  # the base piece has no learned numbers
    hop = max(1, round(SR * 256 / 48_000))
    # fusion_features reads envelopes band 0 (low) and 1 (body) and upper bands 1..3; the low band is the same
    # filter in both helpers and causal band 2 is never read.
    low, body = attack.BANDS[0], attack.BANDS[1]
    assert upper.BANDS[0] == low
    bands = [low, body, *upper.BANDS[1:]]
    sos = np.stack([_sos(lo, hi) for lo, hi in bands])
    assert sos.shape == (5, 2, 6) and np.all(sos[:, :, 3] == 1.0)
    assert (attack.FAST_TAU_S, attack.SLOW_TAU_S) == (upper.FAST_TAU_S, upper.SLOW_TAU_S)
    alpha = np.array([math.exp(-1.0 / (tau * SR)) for tau in (attack.FAST_TAU_S, attack.SLOW_TAU_S)])

    freqs = np.fft.rfftfreq(base.FFT_SAMPLES, 1.0 / SR)
    mask = (freqs >= base.LOW_FREQUENCY_HZ) & (freqs <= base.HIGH_FREQUENCY_HZ)
    bins = np.flatnonzero(mask)
    assert np.array_equal(bins, np.arange(bins[0], bins[-1] + 1)), 'spectrum bins must be one run'
    masked = freqs[mask]
    splits = [0]
    for i, (lo, hi) in enumerate(bandwise.BANDS):
        m = (masked >= lo) & ((masked <= hi) if i == len(bandwise.BANDS) - 1 else (masked < hi))
        idx = np.flatnonzero(m)
        assert idx[0] == splits[-1] and np.array_equal(idx, np.arange(idx[0], idx[-1] + 1)), 'bands must tile'
        splits.append(int(idx[-1]) + 1)
    assert splits[-1] == len(masked)

    i64, f64 = np.int64, np.float64
    return {
        'base.sample_rate': np.array(SR, i64),
        'base.hop': np.array(hop, i64),
        # (band, section, b0 b1 b2 a0 a1 a2); bands low 45-140, body 140-400, upper 1-2k, 2-4k, 4-8k Hz.
        'base.band_sos': sos.astype(f64),
        # Fast then slow one-pole: y = gain * power + alpha * y.
        'base.env_alpha': alpha.astype(f64),
        'base.env_gain': (1.0 - alpha).astype(f64),
        'base.fft_window': np.hanning(base.FFT_SAMPLES).astype(f64),
        # [first, end) rfft bins kept (30 Hz to 8 kHz inclusive).
        'base.fft_bins': np.array([bins[0], bins[-1] + 1], i64),
        'base.log2_freq': np.log2(masked).astype(f64),
        # Kept-bin boundaries of the bandwise low, body and upper bands.
        'base.band_splits': np.array(splits, i64),
        'base.span': np.array(math.floor(0.015 * SR / hop), i64),
        'base.deadline': np.array(math.ceil(0.040 * SR / hop), i64),
        'base.eps': np.array(base.EPSILON, f64),
        'base.active_floor': np.array(1e-6, f64),
        'base.low_ratio': np.array(1.2, f64),
        'base.body_ratio': np.array(2.0, f64),
        'base.upper_ratio': np.array(2.0, f64),
        'base.upper_count': np.array(2, i64),
        'base.log_clip': np.array(6.0, f64),
        'base.lag_norm_s': np.array(0.040, f64),
    }
