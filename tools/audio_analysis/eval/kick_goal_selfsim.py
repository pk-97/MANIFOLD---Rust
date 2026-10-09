"""Song-relative cues: each candidate against the song's own recent likely kicks. Causal.

Inputs per song: the base probability p of every candidate (from a model that
never saw the song), a shape descriptor D (rises, clipped at zero, L2-normalised)
and the low-band rise level. Over the candidates decided in the previous WINDOW_S
seconds (strictly earlier in decision order), weights w = p ** 8 pick out the
song's own confident hits on its own score scale. Features:
- lp: logit p
- sim: cosine of D with the weighted mean D (the song's recent kick shape)
- lp_rel: lp minus the weighted mean lp
- lvl_rel: low rise minus its weighted mean
- evidence: log(1 + sum w), how much kick evidence the recent past holds
No timing or tempo is used: nothing is predicted, a fire still needs its own attack.
"""
from __future__ import annotations

import numpy as np
from scipy.special import logit

SELF_NAMES = ('lp', 'sim', 'lp_rel', 'lvl_rel', 'evidence')
POWER = 8


def descriptor(shape):
    d = np.clip(np.asarray(shape, dtype=np.float64), 0, None)
    return d / (np.linalg.norm(d, axis=1, keepdims=True) + 1e-9)


def self_features(p, shape, level, emit_s, window_s):
    order = np.argsort(emit_s, kind='stable')
    lp_all = logit(np.clip(p, 1e-6, 1 - 1e-6))
    d, lp, lv, em = descriptor(shape)[order], lp_all[order], np.asarray(level)[order], np.asarray(emit_s)[order]
    w = np.clip(p[order], 0, 1) ** POWER

    def csum(x):
        return np.concatenate([np.zeros((1,) + x.shape[1:]), np.cumsum(x, axis=0)])
    cw, cwd, cwl, cwv = csum(w), csum(w[:, None] * d), csum(w * lp), csum(w * lv)
    i = np.arange(len(p))
    j = np.searchsorted(em, em - window_s, side='left')
    sw = cw[i] - cw[j]
    tmpl = cwd[i] - cwd[j]
    safe = np.maximum(sw, 1e-12)
    sim = np.einsum('ij,ij->i', d, tmpl) / (np.linalg.norm(tmpl, axis=1) + 1e-12)
    out = np.zeros((len(p), len(SELF_NAMES)))
    out[:, 0] = lp
    out[:, 1] = np.where(sw > 1e-9, sim, 0.0)
    out[:, 2] = np.where(sw > 1e-9, lp - (cwl[i] - cwl[j]) / safe, 0.0)
    out[:, 3] = np.where(sw > 1e-9, lv - (cwv[i] - cwv[j]) / safe, 0.0)
    out[:, 4] = np.log1p(sw)
    res = np.zeros_like(out)
    res[order] = out
    return res
