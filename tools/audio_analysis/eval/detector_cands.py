"""Shared causal candidate finders. A family picks a band and a rule; the detector's net decides what each
candidate is. Everything here is causal: hop h uses only samples up to the end of hop h.

- band_hops: log energy of one Butterworth band over the last `win_s` of every HOP-sample hop.
- rise_candidates: one candidate per attack. Hop h is a candidate when its energy rises at least rise_db over the
  minimum of the previous `lookback` hops, it is the first such hop since the rise fell under half of rise_db, and
  the last candidate is at least `refractory` hops back. Availability = candidate + `deadline` hops.
"""
import numpy as np
from scipy.signal import butter, sosfilt

HOP = 256


def band_hops(x, sr, lo, hi, win_s=.005, order=4):
    y = sosfilt(butter(order, (lo, hi), btype='band', fs=sr, output='sos'), x) ** 2
    k = int(win_s * sr)
    c = np.concatenate([[0.0], np.cumsum(y)])
    ends = np.arange(HOP, len(x) + 1, HOP)
    return 10 * np.log10((c[ends] - c[ends - k]) / k + 1e-12)


def rise_candidates(e, rise_db, lookback, refractory, deadline):
    cand, last, armed = [], -100, True
    for h in range(lookback, len(e)):
        r = e[h] - e[h - lookback:h].min()
        if r < rise_db / 2:
            armed = True
        if armed and r >= rise_db and h - last >= refractory:
            cand.append(h)
            last, armed = h, False
    cand = np.array(cand, int)
    return cand, cand + deadline


def spectral_rise(spec, sr, lo, hi, lookback=10):
    """Per hop: the mean over net bands in [lo, hi] Hz of how far each band's level (dB, the net spectrum on its 2 ms
    grid) sits above its own mean over the previous `lookback` frames, clipped at 0. A new note lights up bands that
    were quiet, so a pitch change scores even when the overall level does not rise. Hop h reads the last frame that
    ends at or before the hop's end."""
    from tools.audio_analysis.eval.kick_goal_nn import CENTRES, FRAME_S
    sel = (CENTRES >= lo) & (CENTRES <= hi)
    s = spec[:, sel].astype(np.float64)
    c = np.cumsum(np.vstack([np.zeros((1, s.shape[1])), s]), axis=0)
    past = np.full_like(s, np.inf)
    past[lookback:] = (c[lookback:-1] - c[:-lookback - 1]) / lookback
    rise = np.clip(s - past, 0, None).mean(1)
    rise[:lookback] = 0
    frame_hop = FRAME_S * sr
    n_hops = int(len(s) * frame_hop // HOP)
    f = np.floor((np.arange(n_hops) + 1) * HOP / frame_hop).astype(int)
    return rise[np.clip(f, 0, len(rise) - 1)]


def peak_candidates(v, thr, refractory, deadline):
    """One candidate per excursion of v over thr: armed again once v falls under thr / 2, refractory hops apart."""
    cand, last, armed = [], -100, True
    for h in range(len(v)):
        if v[h] < thr / 2:
            armed = True
        if armed and v[h] >= thr and h - last >= refractory:
            cand.append(h)
            last, armed = h, False
    cand = np.array(cand, int)
    return cand, cand + deadline
