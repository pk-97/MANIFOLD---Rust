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
