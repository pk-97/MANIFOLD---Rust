"""Firing rules on top of a candidate score. Causal and tempo-free.

dip rule (Fable consult): after a fire, a new fire also needs the low band
(max of the 40-100 Hz filter-bank envelopes) to have fallen DIP_DB below its
peak since the last fire's onset, before the new onset. A kick's own sub body
cannot retrigger; a fresh kick after the tail has decayed can. The 60 ms
refractory still applies.
"""
from __future__ import annotations

import numpy as np

from tools.audio_analysis.eval.kick_goal_eval import REFRACTORY

MS = .001
LOW_BANDS = slice(0, 4)


def low_envelope(env):
    return env[:, LOW_BANDS].max(axis=1)


def fires_dip(scores, available, onset_s, sr, hop, th, low, dip_db):
    out, last_a, last_on = [], -np.inf, None
    for i in np.flatnonzero(scores >= th):
        a = available[i]
        if (a - last_a) * hop / sr < REFRACTORY - 1e-12:
            continue
        if last_on is not None and dip_db > 0:
            s, e = int(last_on / MS), int(onset_s[i] / MS)
            if e > s:
                seg = low[s:e]
                k = int(np.argmax(seg))
                if seg[k:].min() > seg[k] - dip_db:
                    continue
        out.append(i)
        last_a, last_on = a, onset_s[i]
    return np.asarray(out, dtype=int)
