"""Training-fold-only logistic stacking on top of frozen fold logits.

score = base_logit + X @ beta + b, with base_logit used as a fixed offset. For
outer song T, beta is fitted on the inner logits (models trained without T and
U) of every U != T. For inner cutoff selection on U, beta is refitted without U.
Songs and classes are weighted equally, as in the evening protocol. The fitted
quantities never see the song they score.
"""
from __future__ import annotations

import numpy as np
from scipy.optimize import minimize
from scipy.special import expit

from .kick_night_common import TRACKS


def _weights(y, song):
    w = np.zeros(len(y))
    for s in np.unique(song):
        for c in (0, 1):
            m = (song == s) & (y == c)
            if m.any():
                w[m] = 1.0 / m.sum()
    return w / w.sum()


def fit_stack(offset, x, y, song, l2):
    mean, scale = x.mean(axis=0), np.maximum(x.std(axis=0), 1e-6)
    z = (x - mean) / scale
    w = _weights(y, song)
    k = z.shape[1]

    def loss(p):
        eta = offset + z @ p[:k] + p[k]
        q = expit(eta)
        nll = -np.sum(w * (y * np.log(q + 1e-15) + (1 - y) * np.log(1 - q + 1e-15)))
        g = w * (q - y)
        return nll + l2 * np.sum(p[:k] ** 2), np.concatenate([z.T @ g + 2 * l2 * p[:k], [g.sum()]])

    res = minimize(loss, np.zeros(k + 1), jac=True, method='L-BFGS-B')
    return dict(mean=mean, scale=scale, beta=res.x[:k], bias=float(res.x[k]), converged=bool(res.success))


def apply_stack(model, offset, x):
    return offset + ((x - model['mean']) / model['scale']) @ model['beta'] + model['bias']


class Stacker:
    """Caches one fit per (excluded songs) set; base and design are callables."""

    def __init__(self, d, base, design, l2):
        self.d, self.base, self.design, self.l2 = d, base, design, l2
        self.models = {}

    def _training(self, outer, exclude):
        offs, xs, ys, ss = [], [], [], []
        for u in TRACKS:
            if u in exclude:
                continue
            r = self.d.records[u]
            m = np.asarray(r['training_mask'], dtype=bool)
            offs.append(self.base(outer, u)[m])
            xs.append(self.design(outer, u)[m])
            ys.append(np.asarray(r['labels'])[m])
            ss.append(np.full(m.sum(), TRACKS.index(u)))
        return np.concatenate(offs), np.concatenate(xs), np.concatenate(ys).astype(float), np.concatenate(ss)

    def model(self, outer, target):
        # Training offsets depend on the outer song, so it is part of the key.
        exclude = frozenset({outer, target})
        key = (outer, exclude)
        if key not in self.models:
            self.models[key] = fit_stack(*self._training(outer, exclude), self.l2)
        return self.models[key]

    def logit(self, outer, target):
        return apply_stack(self.model(outer, target), self.base(outer, target), self.design(outer, target))

    def prob(self, outer, target):
        return expit(self.logit(outer, target))
