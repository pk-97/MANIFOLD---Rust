"""Causal low-frequency filter bank: where below 400 Hz a candidate's new energy lands, early and late.

The 1024-sample spectrum has bins 47 Hz apart with a four-bin Hann main lobe,
so a 50 Hz kick body and a 120 Hz tom smear together. Here: eight causal
2nd-order bandpass filters (Q 2) centred 40-320 Hz, each with a 5 ms moving
average of its rectified output, in dB on the 1 ms grid. Per band and
candidate: the rise of the max over the early and the late half of
[onset, evidence deadline] over the median of [-60, -10] ms before onset.
"""
from __future__ import annotations

import numpy as np
from scipy.signal import butter, sosfilt

CENTRES = (40.0, 55.0, 75.0, 100.0, 135.0, 180.0, 240.0, 320.0)
Q = 2.0
MS = .001
LOWBANK_NAMES = tuple(f'low_{w}_{int(c)}' for w in ('early', 'late') for c in CENTRES)


def band_envelopes(x, sr):
    """(n_ms, bands) dB envelopes on the exact 1 ms grid; causal."""
    grid = np.round(np.arange(int(len(x) / sr / MS)) * MS * sr).astype(int)
    out = np.zeros((len(grid), len(CENTRES)))
    k = max(1, int(round(.005 * sr)))
    for b, fc in enumerate(CENTRES):
        bw = fc / Q
        sos = butter(2, [fc - bw / 2, fc + bw / 2], btype='bandpass', fs=sr, output='sos')
        y = np.abs(sosfilt(sos, x))
        cs = np.concatenate([np.zeros(k), np.cumsum(y)])
        y = (cs[k:] - cs[:-k]) / k
        out[:, b] = 20 * np.log10(y[grid] + 1e-9)
    return out


def lowbank_features(env, onset_s, emit_s):
    out = np.zeros((len(onset_s), 2 * len(CENTRES)))
    n = len(env)
    for i, (on, em) in enumerate(zip(onset_s, emit_s)):
        a, d = int(on / MS), min(n - 1, int(em / MS))
        if a < 60 or d <= a:
            continue
        base = np.median(env[a - 60:a - 10], axis=0)
        mid = a + max(1, (d - a + 1) // 2)
        out[i, :len(CENTRES)] = env[a:mid].max(axis=0) - base
        out[i, len(CENTRES):] = env[mid:d + 1].max(axis=0) - base if d + 1 > mid else out[i, :len(CENTRES)]
    return out
