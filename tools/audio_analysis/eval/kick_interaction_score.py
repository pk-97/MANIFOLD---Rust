"""Smooth bounded pair interactions fitted discriminatively on training songs."""
from __future__ import annotations

import copy
import hashlib
import json
from pathlib import Path
import time

import numpy as np
from scipy.optimize import minimize
from scipy.special import expit

from .kick_boosted_score import training_key
from .kick_subspace_score import _training_data

REGULARISATIONS = (.001, .01, .1)
DIMENSION = 15
PAIR_I, PAIR_J = np.triu_indices(DIMENSION)


def basis(features, mean, scale):
    x, mean, scale = np.asarray(features), np.asarray(mean), np.asarray(scale)
    if x.ndim != 2 or x.shape[1] != DIMENSION or mean.shape != (DIMENSION,) or scale.shape != mean.shape:
        raise ValueError('expected the frozen fifteen input features')
    if not all(np.isfinite(a).all() for a in (x, mean, scale)) or np.any(scale <= 0):
        raise ValueError('finite inputs and positive scales required')
    with np.errstate(over='ignore'):
        z = np.clip((x-mean)/scale, -8, 8)
    bounded = np.tanh(z/2)
    return np.column_stack((z, bounded[:, PAIR_I]*bounded[:, PAIR_J]))


def objective(beta, design, labels, weights, regularisation):
    logits = design @ beta[:-1]+beta[-1]
    loss = np.sum(weights*(np.logaddexp(0, logits)-labels*logits))
    loss += .5*regularisation*np.dot(beta[:-1], beta[:-1])
    residual = weights*(expit(logits)-labels)
    return float(loss), np.r_[design.T @ residual+regularisation*beta[:-1], residual.sum()]


def predict_score(model, features):
    design = basis(features, model['mean'], model['scale'])
    coefficients = np.asarray(model['weights'])
    if coefficients.shape != (design.shape[1],) or not np.isfinite(coefficients).all() or not np.isfinite(model['intercept']):
        raise ValueError('invalid interaction model coefficients')
    return expit(design @ coefficients+model['intercept'])


class FoldFitter:
    def __init__(self, cache=None):
        self.cache = Path(cache) if cache is not None else None
        if self.cache is not None:
            self.cache.mkdir(parents=True, exist_ok=True)
        self.models = {}
        self.fits = self.disk_hits = self.memory_hits = 0
        self.fit_cpu_s = 0.
        self.source_sha256 = {p:hashlib.sha256((Path(__file__).parent/p).read_bytes()).hexdigest()
            for p in ('kick_interaction_score.py', 'kick_subspace_score.py', 'kick_boosted_score.py')}

    def fit(self, records, held_out, regularisation):
        if isinstance(regularisation, (bool, np.bool_)) or regularisation not in REGULARISATIONS:
            raise ValueError('regularisation must be predeclared')
        training, x, y, w = _training_data(records, held_out)
        keys = [training_key(r) for r in training]
        signature = json.loads(json.dumps(dict(training_input_keys=keys, l2=regularisation,
            source_sha256=self.source_sha256, basis='z15 plus upper triangular tanh(z/2) products')))
        key = hashlib.sha256(json.dumps(signature, sort_keys=True).encode()).hexdigest()
        path = self.cache/f'{key}.json' if self.cache is not None else None
        if key in self.models:
            self.memory_hits += 1
        elif path is not None and path.exists():
            stored = json.loads(path.read_text())
            if stored['signature'] != signature or stored['model_sha256'] != hashlib.sha256(json.dumps(stored['model'], sort_keys=True).encode()).hexdigest():
                raise ValueError('interaction fit cache provenance mismatch')
            self.models[key] = stored['model']; self.disk_hits += 1
        else:
            mean = np.sum(x*w[:, None], axis=0)
            scale = np.maximum(np.sqrt(np.sum((x-mean)**2*w[:, None], axis=0)), 1e-3)
            design = basis(x, mean, scale)
            started = time.process_time()
            fitted = minimize(objective, np.zeros(design.shape[1]+1), args=(design,y,w,regularisation),
                jac=True, method='L-BFGS-B', options=dict(maxiter=250, ftol=1e-12, gtol=1e-7))
            elapsed = time.process_time()-started
            loss, gradient = objective(fitted.x, design, y, w, regularisation)
            error = float(np.max(np.abs(gradient)))
            if not fitted.success or error > 2e-6:
                raise RuntimeError(f'interaction optimiser failed: {fitted.message}; gradient={error}')
            model = dict(training_tracks=[r['track'] for r in training], mean=mean.tolist(),
                scale=scale.tolist(), weights=fitted.x[:-1].tolist(), intercept=float(fitted.x[-1]),
                regularisation=regularisation, training_loss=loss, iterations=int(fitted.nit),
                gradient_max_abs=error, fit_cpu_s=elapsed, training_samples=len(y),
                training_weight_mass=float(w.sum()), positive_weight_mass=float(w[y==1].sum()),
                negative_weight_mass=float(w[y==0].sum()), training_input_keys=keys,
                training_input_sha256=key, input_features=DIMENSION, basis_features=design.shape[1],
                score_kind='balanced discriminative interaction score; not calibrated probability')
            model = json.loads(json.dumps(model))
            self.models[key] = model; self.fits += 1; self.fit_cpu_s += elapsed
            if path is not None:
                digest = hashlib.sha256(json.dumps(model, sort_keys=True).encode()).hexdigest()
                path.write_text(json.dumps(dict(signature=signature, model=model, model_sha256=digest), separators=(',', ':'))+'\n')
        model = copy.deepcopy(self.models[key]); model['held_out'] = held_out
        return model

    def statistics(self):
        return dict(fits=self.fits, memory_hits=self.memory_hits, disk_hits=self.disk_hits,
                    unique_models=len(self.models), fit_cpu_s=self.fit_cpu_s)
