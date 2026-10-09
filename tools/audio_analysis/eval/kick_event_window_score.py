"""Training-only latent selection of timely event evidence; linear inference."""
from __future__ import annotations

import copy

import numpy as np
from scipy.optimize import minimize
from scipy.special import expit, softmax

from .kick_hard_negative_weights import training_key
from .run_kick_fusion_trial import REGULARISATION, fit_without as baseline_fit

SELECTIONS = ('top1', 'top2', 'softmax_tau1')
ITERATIONS = 3


def timely_positive(record):
    source = record['source']
    truth = np.asarray(source['truth'] if source['group'] == 'original_five'
                       else [t for p in source['passages'] for t in p['kick_times_s']])
    positive = record['training_mask'] & (record['labels'] == 1)
    result = np.zeros(len(positive), bool)
    rows = np.flatnonzero(positive)
    ids = record['event_ids'][rows]
    if np.any(ids < 0) or np.any(ids >= len(truth)):
        raise ValueError('positive event IDs must resolve to source truth')
    emission = (record['available'][rows]+1)*record['hop']/record['sample_rate']
    result[rows] = (emission <= truth[ids]+.070+1e-12) & (emission >= truth[ids]-.035-1e-12)
    return result


def event_weights(labels, ids, timely, logits, selection):
    if selection not in SELECTIONS:
        raise ValueError('unsupported latent-window selection')
    labels, ids, timely, logits = map(np.asarray, (labels, ids, timely, logits))
    if not (labels.shape == ids.shape == timely.shape == logits.shape):
        raise ValueError('unaligned selection inputs')
    valid = (labels == 1) & timely
    events = np.unique(ids[valid])
    negatives = labels == 0
    if not len(events) or not negatives.any():
        raise ValueError('both represented timely events and negatives required')
    weights = np.zeros(len(labels))
    weights[negatives] = .5/negatives.sum()
    for event in events:
        rows = np.flatnonzero(valid & (ids == event))
        if selection == 'softmax_tau1':
            weights[rows] = .5/len(events)*softmax(logits[rows])
        else:
            count = min(int(selection[-1]), len(rows))
            chosen = rows[np.argsort(-logits[rows], kind='stable')[:count]]
            weights[chosen] = .5/len(events)/count
    return weights


def fit_without(records, held_out, selection):
    if selection not in SELECTIONS:
        raise ValueError('unsupported selection')
    training = [r for r in records if r['track'] != held_out]
    names = [r['track'] for r in training]
    if not names or len(set(names)) != len(names):
        raise ValueError('nonempty unique training songs required')
    baseline = baseline_fit(training, held_out)
    arrays, labels, song_data = [], [], []
    offset = 0
    for record in training:
        mask = record['training_mask']
        x, y, ids = record['features'][mask], record['labels'][mask], record['event_ids'][mask]
        timely = timely_positive(record)[mask]
        song_data.append((record['track'], slice(offset, offset+len(y)), ids, timely))
        offset += len(y)
        arrays.append(x); labels.append(y)
    x, y = np.vstack(arrays), np.concatenate(labels)
    z = np.clip((x-baseline['mean'])/baseline['scale'], -8, 8)
    beta = np.r_[baseline['weights'], baseline['intercept']]
    history = []
    for iteration in range(ITERATIONS):
        logits = z @ beta[:-1]+beta[-1]
        weights = np.zeros(len(y)); audit = []
        for track, segment, ids, timely in song_data:
            w = event_weights(y[segment], ids, timely, logits[segment], selection)/len(training)
            weights[segment] = w
            audit.append(dict(track=track, positive_mass=float(w[y[segment] == 1].sum()),
                negative_mass=float(w[y[segment] == 0].sum()),
                selected_positive_rows=np.flatnonzero((y[segment] == 1) & (w > 0)).tolist(),
                positive_weights=w[y[segment] == 1].tolist(),
                represented_events=len(np.unique(ids[(y[segment] == 1) & timely])),
                late_or_early_positive_rows=int(((y[segment] == 1) & ~timely).sum())))

        def loss(value):
            score = z @ value[:-1]+value[-1]
            objective = weights @ (np.logaddexp(0, score)-y*score)
            objective += .5*REGULARISATION*(value[:-1] @ value[:-1])
            residual = weights*(expit(score)-y)
            gradient = np.r_[z.T @ residual+REGULARISATION*value[:-1], residual.sum()]
            return objective, gradient

        result = minimize(loss, beta, jac=True, method='L-BFGS-B',
                          options=dict(maxiter=150, ftol=1e-10))
        if not result.success:
            raise RuntimeError(f'event-window fit failed: {result.message}')
        beta = result.x
        history.append(dict(iteration=iteration+1, loss=float(result.fun),
                            optimizer_iterations=int(result.nit), songs=audit))
    return dict(training_tracks=names, held_out=held_out, selection=selection,
        mean=baseline['mean'], scale=baseline['scale'], weights=beta[:-1].tolist(),
        intercept=float(beta[-1]), history=history)


class FoldFitter:
    def __init__(self, selection):
        self.selection = selection
        self.cache = {}

    def fit(self, records, held_out):
        training = [r for r in records if r['track'] != held_out]
        key = tuple(training_key(r, np.zeros(len(r['labels']), bool)) for r in training)
        if key not in self.cache:
            self.cache[key] = fit_without(training, held_out, self.selection)
        result = copy.deepcopy(self.cache[key])
        result['held_out'] = held_out
        return result
