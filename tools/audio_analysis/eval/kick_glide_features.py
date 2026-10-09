"""Causal low-band pitch glide at each kick candidate (night research, 2026-10-10).

Why a new measurement: the existing centroid cues come from 2048-sample FFT
frames (42.7 ms at 48 kHz, ~23 Hz bins). A frame spans the whole 40 ms evidence
window and has about five bins between 30 and 140 Hz, so it cannot resolve the
40 ms downward sweep that separates a kick body from a bass note. Zero-crossing
intervals of a causally band-passed signal resolve pitch at half-period
resolution (4-10 ms at 50-125 Hz) without any look-ahead.

Fixed design, declared before scoring:
- 4th-order Butterworth band-pass, 30-250 Hz, run causally from sample zero.
- Window = [candidate hop start, end of the availability hop): never later audio.
- Half-period intervals between interpolated zero crossings; f = 1/(2*interval);
  only 25-400 Hz kept; each interval weighted by its mean absolute amplitude.
- glide_slope: weighted least-squares slope of log2 f against time, oct/s.
- glide_drop: weighted mean log2 f in the first half minus the second half.
- pre_jump: |log2 f at window end - log2 f over the 40 ms before the window|;
  a continuing bass note keeps its pitch, a new kick body does not.
Missing values (fewer than three intervals) are 0, the neutral value.
"""
from __future__ import annotations

import numpy as np
from scipy.signal import butter, sosfilt

GLIDE_NAMES = ('glide_slope', 'glide_drop', 'pre_jump')
BAND_HZ = (30.0, 250.0)
F_RANGE = (25.0, 400.0)
PRE_S = .040


def _crossings(y, sr):
    s = np.signbit(y)
    idx = np.flatnonzero(s[1:] != s[:-1])
    frac = y[idx] / (y[idx] - y[idx + 1])
    return (idx + frac) / sr


def _intervals(times, cs, sr, start_s, end_s):
    """`cs` is the cumulative absolute amplitude, computed once per signal."""
    lo, hi = np.searchsorted(times, [start_s, end_s])
    t = times[lo:hi]
    if len(t) < 2:
        return None
    dt = np.diff(t)
    f = 1.0 / (2.0 * dt)
    mid = (t[1:] + t[:-1]) / 2
    keep = (f >= F_RANGE[0]) & (f <= F_RANGE[1])
    if keep.sum() < 3:
        return None
    a = (t[:-1] * sr).astype(np.int64)
    b = np.minimum(np.maximum((t[1:] * sr).astype(np.int64), a + 1), len(cs) - 1)
    w = (cs[b] - cs[a]) / np.maximum(b - a, 1)
    return mid[keep], np.log2(f[keep]), w[keep] + 1e-12


def glide_features(samples, sr, hop, candidates, available):
    """Return (n, 3) causal glide features aligned with the frozen candidate grid."""
    x = np.asarray(samples, dtype=np.float64)
    sos = butter(4, BAND_HZ, btype='band', fs=sr, output='sos')
    y = sosfilt(sos, x)
    times = _crossings(y, sr)
    cs = np.concatenate(([0.0], np.cumsum(np.abs(y))))
    out = np.zeros((len(candidates), len(GLIDE_NAMES)))
    for row, (c, k) in enumerate(zip(candidates, available)):
        # A crossing interpolated before sample n uses samples n-1 and n; the last
        # available sample is (k+1)*hop - 1, so crossings must lie before it.
        start, end = c * hop / sr, ((k + 1) * hop - 1) / sr
        cur = _intervals(times, cs, sr, start, end)
        if cur is None:
            continue
        t, lf, w = cur
        tc = t - np.average(t, weights=w)
        denom = np.sum(w * tc * tc)
        slope = float(np.sum(w * tc * (lf - np.average(lf, weights=w))) / denom) if denom > 0 else 0.0
        half = (start + end) / 2
        first, last = t < half, t >= half
        drop = float(np.average(lf[first], weights=w[first]) - np.average(lf[last], weights=w[last])) \
            if first.any() and last.any() else 0.0
        pre = _intervals(times, cs, sr, start - PRE_S, start)
        jump = 0.0
        if pre is not None:
            tail = t >= end - .015
            end_pitch = np.average(lf[tail], weights=w[tail]) if tail.any() else lf[-1]
            jump = float(abs(end_pitch - np.average(pre[1], weights=pre[2])))
        out[row] = (np.clip(slope, -100, 100), np.clip(drop, -3, 3), np.clip(jump, 0, 4))
    return out


__all__ = ['GLIDE_NAMES', 'glide_features']
