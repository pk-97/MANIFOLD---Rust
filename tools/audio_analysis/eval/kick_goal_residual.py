"""Causal percussive residual of the low filter bank: new energy above each band's own steady level.

Per band of the 40-320 Hz filter bank (kick_goal_lowbank.band_envelopes, dB on
the 1 ms grid), the steady level is the median over the past WINDOW_MS (the
causal half of harmonic-percussive separation: sustained bass, pads and
Cold's 50 Hz bed sit at their median, a kick climbs above it). Residual =
max(0, envelope - steady level). Per candidate and band: the max residual over
the early and the late half of [onset, emit], 16 features.
"""
from __future__ import annotations

import numpy as np
from scipy.ndimage import median_filter

WINDOW_MS = 400
MS = .001


def steady_level(env, window_ms=WINDOW_MS):
    """Median of each band over [t - window_ms - 1, t - 2] ms (past only)."""
    centred = median_filter(env, size=(window_ms, 1), mode='nearest')
    shift = window_ms // 2 + 1
    out = np.empty_like(centred)
    out[shift:] = centred[:-shift]
    out[:shift] = centred[0]
    return out


def residual_features(env, onset_s, emit_s, window_ms=WINDOW_MS):
    res = np.maximum(0.0, env - steady_level(env, window_ms))
    nb = env.shape[1]
    out = np.zeros((len(onset_s), 2 * nb))
    n = len(res)
    for i, (on, em) in enumerate(zip(onset_s, emit_s)):
        a, d = int(on / MS), min(n - 1, int(em / MS))
        if d <= a:
            continue
        mid = a + max(1, (d - a + 1) // 2)
        out[i, :nb] = res[a:mid].max(axis=0)
        out[i, nb:] = res[mid:d + 1].max(axis=0) if d + 1 > mid else out[i, :nb]
    return out
