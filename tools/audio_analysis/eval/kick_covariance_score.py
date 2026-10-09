"""Regularised class-covariance DSP scoring, with no future or held-out input."""
from __future__ import annotations

import copy

import numpy as np
from scipy.special import expit

from .kick_hard_negative_weights import song_weights, training_key

SHRINKAGES = (.1, .5, .9)
COVARIANCE_FLOOR = .01


def class_parameters(z, weights, shrinkage):
    weights = weights/weights.sum()
    mean = weights @ z
    centered = z-mean
    covariance = (centered*weights[:, None]).T @ centered
    covariance = (1-shrinkage)*covariance+shrinkage*np.diag(np.diag(covariance))
    covariance += COVARIANCE_FLOOR*np.eye(z.shape[1])
    lower = np.linalg.cholesky(covariance)
    inverse = np.linalg.solve(lower.T, np.linalg.solve(lower, np.eye(z.shape[1])))
    log_determinant = 2*np.log(np.diag(lower)).sum()
    return mean, covariance, inverse, float(log_determinant)


def fit_without(records, held_out, shrinkage):
    if shrinkage not in SHRINKAGES:
        raise ValueError('only the three predeclared shrinkages are allowed')
    training = [r for r in records if r['track'] != held_out]
    if not training or len({r['track'] for r in training}) != len(training):
        raise ValueError('nonempty unique training songs required')
    arrays, labels, weights = [], [], []
    for record in training:
        mask = record['training_mask']
        y, ids = record['labels'][mask], record['event_ids'][mask]
        arrays.append(record['features'][mask]); labels.append(y)
        weights.append(song_weights(y, ids, np.zeros(len(y), bool), 1)/len(training))
    x, y, w = np.vstack(arrays), np.concatenate(labels), np.concatenate(weights)
    if not np.isfinite(x).all():
        raise ValueError('nonfinite training features')
    mean = np.sum(x*w[:, None], axis=0)
    scale = np.maximum(np.sqrt(np.sum((x-mean)**2*w[:, None], axis=0)), 1e-3)
    z = np.clip((x-mean)/scale, -8, 8)
    positive = class_parameters(z[y == 1], w[y == 1], shrinkage)
    negative = class_parameters(z[y == 0], w[y == 0], shrinkage)
    pm, pc, pi, pd = positive
    nm, nc, ni, nd = negative
    # Compile equal-prior Gaussian log likelihood ratio to a quadratic form.
    quadratic = -.5*(pi-ni)
    linear = pi @ pm-ni @ nm
    intercept = -.5*(pm @ pi @ pm-nm @ ni @ nm+pd-nd)
    return dict(training_tracks=[r['track'] for r in training], held_out=held_out,
        shrinkage=shrinkage, mean=mean.tolist(), scale=scale.tolist(),
        quadratic=quadratic.tolist(), linear=linear.tolist(), intercept=float(intercept),
        score_scale=x.shape[1],
        classes=[dict(label=label, mean=m.tolist(), covariance=c.tolist(), inverse=i.tolist(),
                      log_determinant=d) for label, (m, c, i, d) in ((1, positive), (0, negative))])


def decision_logits(model, features):
    z = np.clip((features-np.asarray(model['mean']))/np.asarray(model['scale']), -8, 8)
    q = np.asarray(model['quadratic'])
    return (np.einsum('bi,ij,bj->b', z, q, z)+z @ np.asarray(model['linear'])
            +model['intercept'])/model['score_scale']


def predict_score(model, features):
    # Per-dimension log evidence keeps the frozen probability grid usable.
    # This monotone score is not a calibrated event probability.
    return expit(decision_logits(model, features))


class FoldFitter:
    def __init__(self, shrinkage):
        if shrinkage not in SHRINKAGES:
            raise ValueError('unsupported shrinkage')
        self.shrinkage = shrinkage
        self.cache = {}

    def fit(self, records, held_out):
        training = [r for r in records if r['track'] != held_out]
        key = tuple(training_key(r, np.zeros(len(r['labels']), bool)) for r in training)
        if key not in self.cache:
            self.cache[key] = fit_without(training, held_out, self.shrinkage)
        result = copy.deepcopy(self.cache[key])
        result['held_out'] = held_out
        return result
