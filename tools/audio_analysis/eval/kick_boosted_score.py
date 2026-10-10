"""Three frozen shallow boosted-tree scorers for offline cached-feature research."""
from __future__ import annotations

import copy
import hashlib
import json
from pathlib import Path
import time

import numpy as np
from scipy.special import expit
import sklearn
from sklearn.ensemble import GradientBoostingClassifier

from .kick_subspace_score import _training_data

DEPTHS = (1, 2, 3)
FIXED_PARAMETERS = dict(n_estimators=64, learning_rate=.05,
                        min_weight_fraction_leaf=.005, random_state=0,
                        subsample=1., n_iter_no_change=None)


def parameters(depth):
    if (isinstance(depth, (bool, np.bool_)) or not isinstance(depth, (int, np.integer))
            or depth not in DEPTHS):
        raise ValueError('only the frozen depth1/depth2/depth3 configurations are allowed')
    return dict(FIXED_PARAMETERS, max_depth=int(depth))


def training_key(record):
    """Hash complete training records, never the excluded song's arrays."""
    digest = hashlib.sha256()
    for name in ('features', 'training_mask', 'labels', 'event_ids'):
        values = np.asarray(record[name])
        digest.update(str((name, values.shape, values.dtype.str)).encode())
        digest.update(values.tobytes())
    return record['track'], digest.hexdigest()


def _standardise(mean, scale, features):
    x = np.asarray(features, dtype=np.float64)
    mean, scale = np.asarray(mean), np.asarray(scale)
    if x.ndim != 2 or mean.shape != (x.shape[1],) or scale.shape != mean.shape:
        raise ValueError('prediction feature dimension differs from model')
    if (not np.all(np.isfinite(x)) or not np.all(np.isfinite(mean))
            or not np.all(np.isfinite(scale)) or np.any(scale <= 0)):
        raise ValueError('prediction values must be finite with positive scale')
    with np.errstate(over='ignore'):
        # sklearn's trees route float32 inputs even when the fitted z is float64.
        return np.clip((x-mean)/scale, -8, 8).astype(np.float32)


def export_model(classifier, mean, scale, training_tracks):
    """Export only the constants and compact arrays needed by inference."""
    prior = float(classifier.init_.class_prior_[1])
    eps = np.finfo(np.float64).eps
    prior = np.clip(prior, eps, 1-eps)
    trees = []
    for estimator in classifier.estimators_[:, 0]:
        tree = estimator.tree_
        trees.append(dict(left=tree.children_left.tolist(), right=tree.children_right.tolist(),
            feature=tree.feature.tolist(), threshold=tree.threshold.tolist(),
            value=tree.value[:, 0, 0].tolist()))
    return dict(format_version=1, training_tracks=training_tracks,
        mean=np.asarray(mean).tolist(), scale=np.asarray(scale).tolist(),
        initial_log_odds=float(np.log(prior/(1-prior))), learning_rate=classifier.learning_rate,
        max_depth=classifier.max_depth, trees=trees,
        score_kind='balanced boosted logistic score; not calibrated event probability')


def predict_score(model, features):
    """Row-local inference from JSON arrays, with no sklearn prediction calls."""
    z = _standardise(model['mean'], model['scale'], features)
    logits = np.full(len(z), model['initial_log_odds'], dtype=np.float64)
    for tree in model['trees']:
        left, right, feature = [np.asarray(tree[name], dtype=np.int64)
                                for name in ('left', 'right', 'feature')]
        threshold, value = np.asarray(tree['threshold']), np.asarray(tree['value'])
        node = np.zeros(len(z), dtype=np.int64)
        active = np.flatnonzero(left[node] != -1)
        for _ in range(model['max_depth']):
            if not len(active):
                break
            current = node[active]
            node[active] = np.where(z[active, feature[current]] <= threshold[current],
                                    left[current], right[current])
            active = active[left[node[active]] != -1]
        if len(active):
            raise ValueError('exported tree exceeds declared depth')
        logits += model['learning_rate'] * value[node]
    return expit(logits)


class FoldFitter:
    """Memoize identical ordered training inputs across nested exclusions."""

    def __init__(self, cache=None):
        self.cache = Path(cache) if cache is not None else None
        if self.cache is not None:
            self.cache.mkdir(parents=True, exist_ok=True)
        self.models = {}
        self.fits = 0
        self.memory_hits = 0
        self.disk_hits = 0
        self.fit_cpu_s = 0.
        self.source_sha256 = hashlib.sha256(Path(__file__).read_bytes()).hexdigest()

    def fit(self, records, held_out, depth):
        config = parameters(depth)
        names = [r['track'] for r in records]
        if len(set(names)) != len(names):
            raise ValueError('track identities must be unique')
        training = [r for r in records if r['track'] != held_out]
        if not training:
            raise ValueError('no training songs')
        keys = [training_key(r) for r in training]
        signature = dict(training_input_keys=keys, parameters=config,
                         sklearn_version=sklearn.__version__, source_sha256=self.source_sha256)
        # JSON roundtrip canonicalises tuple input keys before cache verification.
        signature = json.loads(json.dumps(signature))
        key = hashlib.sha256(json.dumps(signature, sort_keys=True).encode()).hexdigest()
        path = self.cache / f'{key}.json' if self.cache is not None else None
        if key in self.models:
            self.memory_hits += 1
        elif path is not None and path.exists():
            stored = json.loads(path.read_text())
            if stored['signature'] != signature:
                raise ValueError('boosted fit cache provenance mismatch')
            expected = hashlib.sha256(json.dumps(stored['model'], sort_keys=True).encode()).hexdigest()
            if stored['model_sha256'] != expected:
                raise ValueError('boosted model cache checksum mismatch')
            self.models[key] = stored['model']
            self.disk_hits += 1
        else:
            _, x, y, w = _training_data(training, held_out)
            mean = np.sum(x*w[:, None], axis=0)
            scale = np.maximum(np.sqrt(np.sum((x-mean)**2*w[:, None], axis=0)), 1e-3)
            z = _standardise(mean, scale, x)
            started = time.process_time()
            classifier = GradientBoostingClassifier(**config).fit(z, y, sample_weight=w)
            fit_cpu = time.process_time()-started
            model = export_model(classifier, mean, scale, [r['track'] for r in training])
            # Verify the JSON representation itself, not an in-memory tree wrapper.
            model = json.loads(json.dumps(model))
            actual, expected = predict_score(model, x), classifier.predict_proba(z)[:, 1]
            error = float(np.max(np.abs(actual-expected)))
            if error > 1e-12:
                raise ValueError(f'exported prediction differs from sklearn: {error}')
            model.update(training_input_keys=keys, training_samples=len(y),
                training_weight_mass=float(w.sum()), positive_weight_mass=float(w[y == 1].sum()),
                negative_weight_mass=float(w[y == 0].sum()), export_max_abs_error=error,
                sklearn_version=sklearn.__version__, fit_cpu_s=fit_cpu, parameters=config,
                training_input_sha256=key)
            model = json.loads(json.dumps(model))
            self.models[key] = model
            self.fits += 1
            self.fit_cpu_s += fit_cpu
            if path is not None:
                digest = hashlib.sha256(json.dumps(model, sort_keys=True).encode()).hexdigest()
                path.write_text(json.dumps(dict(signature=signature, model=model,
                                                model_sha256=digest), separators=(',', ':'))+'\n')
        result = copy.deepcopy(self.models[key])
        result['held_out'] = held_out
        return result

    def statistics(self):
        return dict(unique_models=len(self.models), fits=self.fits, memory_hits=self.memory_hits,
                    disk_hits=self.disk_hits, fit_cpu_s=self.fit_cpu_s)
