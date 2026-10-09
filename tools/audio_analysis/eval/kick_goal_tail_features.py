"""Tail-aware low-band features: does a hit jump above where the ringing tail should be?

Causal, from the mix only. Low band: 4th-order Butterworth 30-140 Hz (causal),
squared, 12 ms causal moving average (spans one 100 Hz ripple period of a 50 Hz
tone), sampled on an exact 1 ms grid, dB. Per candidate with onset t0 and
evidence deadline t1:
- tail_excess_db: max over [t0, t1] of the envelope minus a straight-line (dB)
  fit to [t0 - 50, t0 - 5] ms, the extrapolation capped at that window's max so
  a rising pre-window never predicts growth. A fresh kick jumps far above it; a
  retrigger over its own tail barely clears it.
- pre_rel_db: mean of [t0 - 50, t0 - 5] ms minus the max over the past 4 s
  (gain-invariant: how loud the low end already is).
- pre_slope_db_ms: slope of that fit (a ringing tail falls).
"""
from __future__ import annotations

import numpy as np
from scipy.ndimage import maximum_filter1d
from scipy.signal import butter, sosfilt

TAIL_NAMES = ('tail_excess_db', 'pre_rel_db', 'pre_slope_db_ms')
MS = .001


def low_env_db(x, sr):
    y = sosfilt(butter(4, (30, 140), btype='band', fs=sr, output='sos'), x) ** 2
    k = int(.012 * sr)
    c = np.concatenate([[0.0], np.cumsum(y)])
    ma = (c[k:] - c[:-k]) / k  # ma[j] averages y[j : j + k]; causal value at sample j + k - 1
    idx = np.round(np.arange(int(len(x) / sr / MS)) * MS * sr).astype(int)
    src = np.clip(idx - (k - 1), 0, len(ma) - 1)
    return 10 * np.log10(ma[src] + 1e-12)


def tail_features(x, sr, onset_s, deadline_s):
    e = low_env_db(x, sr)
    past_max = maximum_filter1d(e, size=4000, origin=1999)  # max over the previous 4 s, inclusive
    out = np.zeros((len(onset_s), 3))
    tt = np.arange(-50, -4) * 1.0
    for n, (t0, t1) in enumerate(zip(onset_s, deadline_s)):
        i0, i1 = int(round(t0 / MS)), int(round(t1 / MS))
        if i0 < 60 or i1 >= len(e):
            continue
        pre = e[i0 - 50:i0 - 4]
        slope, icpt = np.polyfit(tt, pre, 1)
        horizon = np.arange(0, i1 - i0 + 1)
        pred = np.minimum(icpt + slope * horizon, pre.max())
        out[n] = (float(np.max(e[i0:i1 + 1] - pred)), float(pre.mean() - past_max[i0 - 5]), float(slope))
    return out
