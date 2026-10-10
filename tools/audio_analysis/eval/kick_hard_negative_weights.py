"""One-pass, training-fold-only hard-negative weighting of frozen DSP features."""
from __future__ import annotations

import copy
import hashlib
import json

import numpy as np
from scipy.optimize import minimize
from scipy.special import expit

from .run_kick_fusion_trial import REGULARISATION, fit_without, predict_score

HARD_SCORE = .90
MULTIPLIERS = (1, 2, 4)


def temporal_eligibility(record):
    """Keep reviewed negatives whose evidence cannot be a nearby labelled kick."""
    start = (record['candidates'] + 1) * record['hop'] / record['sample_rate']
    end = (record['available'] + 1) * record['hop'] / record['sample_rate']
    source = record['source']
    truth = np.asarray(source['truth'] if source['group'] == 'original_five'
                       else [t for p in source['passages'] for t in p['kick_times_s']])
    eligible = record['training_mask'] & (record['labels'] == 0)
    if len(truth):
        eligible &= ~np.any((start[:, None] <= truth[None, :] + .200 + 1e-12)
                           & (end[:, None] >= truth[None, :] - .035 - 1e-12), axis=1)
    return eligible


def song_weights(labels, event_ids, hard, multiplier):
    """Keep each class at half the song's mass, including equal positive events."""
    if multiplier not in MULTIPLIERS:
        raise ValueError('only the frozen 1x/2x/4x comparison is allowed')
    labels, event_ids, hard = np.asarray(labels), np.asarray(event_ids), np.asarray(hard, bool)
    if labels.shape != event_ids.shape or labels.shape != hard.shape:
        raise ValueError('weight inputs must align')
    positive, negative = labels == 1, labels == 0
    if not positive.any() or not negative.any() or np.any(~(positive | negative)):
        raise ValueError('both binary training classes required')
    if np.any(hard & ~negative):
        raise ValueError('hard-negative selection contains a positive')
    weights = np.zeros(len(labels))
    events, counts = np.unique(event_ids[positive], return_counts=True)
    for event, count in zip(events, counts):
        weights[positive & (event_ids == event)] = .5 / len(events) / count
    # With multiplier1 use the original arithmetic for an exact baseline replay.
    weights[negative] = .5 / (negative.sum() + (multiplier - 1) * hard.sum())
    weights[hard] *= multiplier
    return weights


def reviewed_masks(records, review):
    """Review is source annotation; a fold must still apply its own score cutoff."""
    if review.get('status') != 'lead_accepted_provisional_source_review':
        raise ValueError('negative evidence review has not been accepted')
    tracks = {t['track']: t for t in review['tracks']}
    if len(tracks) != len(review['tracks']) or set(tracks) != {r['track'] for r in records}:
        raise ValueError('review must cover exactly the development songs')
    result = {}
    for record in records:
        row = tracks[record['track']]
        if row['audio_sha256'] != record['source']['audio_sha256']:
            raise ValueError('review audio differs from cached source')
        mask = np.zeros(len(record['candidates']), bool)
        seen = set()
        for decision in row['candidates']:
            index = decision['candidate_index']
            if index in seen or not 0 <= index < len(mask):
                raise ValueError('duplicate or invalid reviewed candidate index')
            seen.add(index)
            if int(record['candidates'][index]) != decision['candidate_hop']:
                raise ValueError('review candidate identity changed')
            if decision['disposition'] not in ('accept_non_kick', 'reject_ambiguous', 'reject_kick_or_tail'):
                raise ValueError('unknown evidence disposition')
            if not decision.get('reason') or not decision.get('evidence'):
                raise ValueError('review needs a reason and evidence')
            mask[index] = decision['disposition'] == 'accept_non_kick'
        if np.any(mask & ~temporal_eligibility(record)):
            raise ValueError('accepted review violates frozen temporal/label eligibility')
        result[record['track']] = mask
    return result


def training_key(record, reviewed):
    """Changing training data or evidence invalidates the in-memory fit cache."""
    digest = hashlib.sha256()
    for name in ('features', 'training_mask', 'labels', 'event_ids', 'candidates', 'available'):
        array = np.asarray(record[name])
        digest.update(str((name, array.shape, array.dtype.str)).encode())
        digest.update(array.tobytes())
    digest.update(np.asarray(reviewed, bool).tobytes())
    digest.update(json.dumps(record['source'], sort_keys=True).encode())
    digest.update(str((record['sample_rate'], record['hop'])).encode())
    return record['track'], digest.hexdigest()


class FoldFitter:
    """Caches only identical training sets; no test features enter selection or fit."""

    def __init__(self, reviewed):
        self.reviewed = reviewed
        self.baselines = {}
        self.models = {}

    def fit(self, records, held_out, multiplier):
        if multiplier not in MULTIPLIERS:
            raise ValueError('unsupported multiplier')
        training = [r for r in records if r['track'] != held_out]
        names = [r['track'] for r in training]
        if not names or len(set(names)) != len(names):
            raise ValueError('training songs must be nonempty and unique')
        key = tuple(training_key(r, self.reviewed[r['track']]) for r in training)
        if key not in self.baselines:
            self.baselines[key] = fit_without(training, held_out)
        if (key, multiplier) not in self.models:
            baseline = self.baselines[key]
            arrays, labels, weights, selections = [], [], [], []
            for record in training:
                mask = record['training_mask']
                x, y, ids = record['features'][mask], record['labels'][mask], record['event_ids'][mask]
                eligible = self.reviewed[record['track']] & temporal_eligibility(record)
                scores = predict_score(baseline, record['features'])
                hard = eligible & (scores >= HARD_SCORE)
                w = song_weights(y, ids, hard[mask], multiplier) / len(training)
                arrays.append(x); labels.append(y); weights.append(w)
                selected = np.flatnonzero(hard).tolist()
                n, m = int(np.count_nonzero(y == 0)), len(selected)
                selections.append(dict(track=record['track'], negative_rows=n,
                    selected_candidate_indices=selected, selected_count=m,
                    selected_hops=record['candidates'][hard].astype(int).tolist(),
                    selection_sha256=hashlib.sha256(json.dumps(selected).encode()).hexdigest(),
                    effective_selected_increase=multiplier*n/(n+(multiplier-1)*m),
                    positive_mass=float(w[y == 1].sum()), negative_mass=float(w[y == 0].sum())))
            x, y, w = np.vstack(arrays), np.concatenate(labels), np.concatenate(weights)
            z = np.clip((x-np.asarray(baseline['mean']))/np.asarray(baseline['scale']), -8, 8)

            def loss(beta):
                logits = z @ beta[:-1] + beta[-1]
                value = np.sum(w*(np.logaddexp(0, logits)-y*logits))
                value += .5*REGULARISATION*np.dot(beta[:-1], beta[:-1])
                residual = w*(expit(logits)-y)
                return value, np.r_[z.T @ residual+REGULARISATION*beta[:-1], residual.sum()]

            fit = minimize(loss, np.zeros(x.shape[1]+1), jac=True, method='L-BFGS-B',
                           options=dict(maxiter=150, ftol=1e-10))
            if not fit.success:
                raise RuntimeError(f'hard-negative fit failed: {fit.message}')
            model = dict(training_tracks=names, training_input_keys=list(key),
                mean=baseline['mean'], scale=baseline['scale'],
                weights=fit.x[:-1].tolist(), intercept=float(fit.x[-1]),
                training_loss=float(fit.fun), iterations=int(fit.nit), multiplier=multiplier,
                selection=selections)
            if multiplier == 1:
                for field in ('weights', 'intercept', 'training_loss', 'iterations'):
                    if model[field] != baseline[field]:
                        raise ValueError(f'1x does not exactly replay baseline {field}')
            self.models[key, multiplier] = model
        result = copy.deepcopy(self.models[key, multiplier])
        result['held_out'] = held_out
        return result
