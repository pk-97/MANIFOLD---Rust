"""Offline weighted class subspaces; reconstruction contrast is not a probability.

Fit rank-2 bases once from training songs. Prediction uses frozen normalisation,
class centring, and projection; it never recomputes an SVD or changes event timing.
"""
from __future__ import annotations

import numpy as np


def _training_data(records, held_out):
    names = [r['track'] for r in records]
    if len(set(names)) != len(names):
        raise ValueError('track identities must be unique')
    training = [r for r in records if r['track'] != held_out]
    if not training:
        raise ValueError('no training songs')
    arrays, labels, weights = [], [], []
    dimension = None
    for record in training:
        x = np.asarray(record['features'], dtype=np.float64)
        y = np.asarray(record['labels'])
        ids = np.asarray(record['event_ids'])
        selected = np.asarray(record['training_mask'])
        if x.ndim != 2 or x.shape[1] == 0:
            raise ValueError('features must be a nonempty-width matrix')
        if any(a.shape != (len(x),) for a in (y, ids, selected)) or selected.dtype != np.bool_:
            raise ValueError('labels, event IDs, and boolean mask must align with features')
        if dimension is not None and x.shape[1] != dimension:
            raise ValueError('training feature dimensions differ')
        dimension = x.shape[1]
        x, y, ids = x[selected], y[selected], ids[selected]
        if not np.all(np.isfinite(x)) or not np.all(np.isin(y, (0, 1))):
            raise ValueError('reviewed training features must be finite and labels binary')
        positive, negative = y == 1, y == 0
        if not positive.any() or not negative.any():
            raise ValueError(f'both training classes required: {record["track"]}')
        positive_ids = ids[positive]
        if not np.issubdtype(positive_ids.dtype, np.integer) or np.any(positive_ids < 0):
            raise ValueError('positive candidates require nonnegative integer event IDs')
        # Exactly the frozen fusion weights: equal songs and classes; positive
        # candidates share one event's weight, while negatives share class weight.
        w = np.zeros(len(y))
        events, counts = np.unique(positive_ids, return_counts=True)
        for event, count in zip(events, counts):
            w[positive & (ids == event)] = .5 / len(events) / count
        w[negative] = .5 / np.count_nonzero(negative)
        arrays.append(x)
        labels.append(y)
        weights.append(w / len(training))
    return training, np.vstack(arrays), np.concatenate(labels), np.concatenate(weights)


def fit_without(records, held_out, rank=2):
    """Fit weighted affine class subspaces after excluding the entire held-out song."""
    if isinstance(rank, (bool, np.bool_)) or not isinstance(rank, (int, np.integer)) or rank < 1:
        raise ValueError('rank must be a positive integer')
    training, x, y, w = _training_data(records, held_out)
    if rank >= x.shape[1]:
        raise ValueError('rank must be smaller than feature dimension')
    with np.errstate(over='ignore', invalid='ignore'):
        mean = np.sum(x * w[:, None], axis=0)
        scale = np.maximum(np.sqrt(np.sum((x - mean)**2 * w[:, None], axis=0)), 1e-3)
    if not np.all(np.isfinite(mean)) or not np.all(np.isfinite(scale)):
        raise ValueError('training values overflow normalisation')
    z = np.clip((x - mean) / scale, -8, 8)
    classes = {}
    for label, name in ((0, 'negative'), (1, 'positive')):
        selected = y == label
        if np.count_nonzero(selected) <= rank:
            raise ValueError(f'{name} class needs more samples than rank')
        values, class_weights = z[selected], w[selected] / np.sum(w[selected])
        centre = np.sum(values * class_weights[:, None], axis=0)
        weighted = (values - centre) * np.sqrt(class_weights[:, None])
        _, singular_values, vh = np.linalg.svd(weighted, full_matrices=False)
        tolerance = np.finfo(float).eps * max(weighted.shape) * singular_values[0]
        if singular_values[rank - 1] <= tolerance:
            raise ValueError(f'{name} class has insufficient numerical rank')
        classes[name] = dict(mean=centre.tolist(), basis=vh[:rank].tolist(),
                             singular_values=singular_values.tolist(),
                             training_samples=int(np.count_nonzero(selected)))
    return dict(training_tracks=[r['track'] for r in training], held_out=held_out,
                mean=mean.tolist(), scale=scale.tolist(), rank=int(rank), classes=classes,
                score_kind='squared reconstruction residual contrast mapped to [0,1]; not probability')


def _residual(standardised, class_model, rank):
    centre, basis = np.asarray(class_model['mean']), np.asarray(class_model['basis'])
    dimension = standardised.shape[1]
    if centre.shape != (dimension,) or basis.shape != (rank, dimension):
        raise ValueError('class mean or basis has invalid shape')
    if not np.all(np.isfinite(centre)) or not np.all(np.isfinite(basis)):
        raise ValueError('class model must be finite')
    centred = standardised - centre
    error = centred - (centred @ basis.T) @ basis
    return np.sum(error**2, axis=1)


def predict_score(model, features):
    """Compare squared distances from the two fixed affine subspaces."""
    x = np.asarray(features, dtype=np.float64)
    mean, scale = np.asarray(model['mean']), np.asarray(model['scale'])
    if x.ndim != 2 or mean.shape != (x.shape[1],) or scale.shape != mean.shape:
        raise ValueError('prediction feature dimension differs from model')
    if (not np.all(np.isfinite(x)) or not np.all(np.isfinite(mean))
            or not np.all(np.isfinite(scale)) or np.any(scale <= 0)):
        raise ValueError('prediction inputs and normalisation must be finite with positive scale')
    with np.errstate(over='ignore'):
        standardised = np.clip((x - mean) / scale, -8, 8)
    negative = _residual(standardised, model['classes']['negative'], model['rank'])
    positive = _residual(standardised, model['classes']['positive'], model['rank'])
    contrast = (negative - positive) / (negative + positive + 1e-12)
    return .5 * (contrast + 1.)


__all__ = ['fit_without', 'predict_score']
