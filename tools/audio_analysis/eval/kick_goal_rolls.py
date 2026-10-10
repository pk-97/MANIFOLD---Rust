"""Kick notes from an isolated kick stem, rolls over a ringing tail included (Peter's ruling, 2026-10-10).

An 808 note played into a ringing tail barely changes the amplitude envelope,
so a fresh-attack rule (kick_goal_labels.fresh_onsets) misses it; a decay-line
rule caught 10 of 71 reviewed rolls. What every new note does is restart the
pitch drop: the kick stem's instantaneous frequency (zero crossings after a
400 Hz low-pass, offline) jumps from the tail's 35-70 Hz to 150-600 Hz and glides
back down. A pitch restart at ms i: the stem is sounding (within 30 dB of its
99.9th percentile) over the previous 25 ms, f[i] >= max(JUMP x the median of
f over [i-25, i-5], MIN_HZ), and f stays >= HOLD x that median for HOLD_MS.
Notes = fresh attacks plus pitch restarts, at least MIN_GAP_MS apart.
"""
from __future__ import annotations

import numpy as np
from scipy.signal import butter, sosfiltfilt

from tools.audio_analysis.eval.kick_goal_labels import MS, fresh_onsets

MIN_GAP_MS = 25


def inst_freq(x, sr):
    """Instantaneous frequency (Hz) on the 1 ms grid from zero crossings below 400 Hz."""
    y = sosfiltfilt(butter(4, 400, fs=sr, output='sos'), x)
    s = np.signbit(y)
    zc = np.flatnonzero(s[1:] != s[:-1]) + 1
    t = zc / sr
    f = 1 / (2 * np.diff(t))
    grid = np.arange(int(len(x) / sr / MS)) * MS
    return np.interp(grid, t[1:], f)


def pitch_restarts(e, f, jump=2.2, min_hz=110.0, hold=1.5, hold_ms=6):
    n = min(len(e), len(f))
    e, f = e[:n], f[:n]
    sounding = e >= np.percentile(e, 99.9) - 30
    run = np.convolve(sounding, np.ones(25, dtype=int), mode='full')[:n] >= 25
    base = np.full(n, np.inf)
    base[25:] = np.median(np.lib.stride_tricks.sliding_window_view(f, 20)[:n - 25], axis=1)
    cand = np.flatnonzero(run & (f >= np.maximum(jump * base, min_hz)))
    out = []
    for i in cand:
        if i + hold_ms < n and np.all(f[i:i + hold_ms] >= hold * base[i]):
            out.append(i)
    return np.array(out, dtype=int)


def kick_notes(x, sr, e, **kw):
    """Note times (s, stem time): fresh attacks plus pitch restarts, MIN_GAP_MS apart."""
    fresh = np.round(fresh_onsets(e) / MS).astype(int)
    restart = pitch_restarts(e, inst_freq(x, sr), **kw)
    out, last = [], -10 ** 9
    for i in np.unique(np.concatenate([fresh, restart])):
        if i - last >= MIN_GAP_MS:
            out.append(i)
            last = i
    return np.array(out) * MS
