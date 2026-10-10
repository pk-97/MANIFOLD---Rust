"""Past-only calibration of frozen linear scores; never synthesises candidates."""
from __future__ import annotations

import copy
import hashlib
import json

import numpy as np
from scipy.special import expit

from .kick_boosted_score import training_key
from .kick_subspace_score import _training_data

STRENGTHS = (.25,.5,1.)
CAPACITY = 1536
HISTORY_S = 8.
WARMUP = 64
MAX_OFFSET = 2.


def raw_logits(model, features):
    x = np.asarray(features)
    if x.ndim != 2 or x.shape[1] != 15 or not np.isfinite(x).all():
        raise ValueError('fifteen finite acoustic features required')
    return np.clip((x-np.asarray(model['mean']))/np.asarray(model['scale']),-8,8) @ np.asarray(model['weights'])+model['intercept']


class ScoreStream:
    def __init__(self, reference, strength):
        if not np.isfinite(reference) or strength not in (0.,)+STRENGTHS:
            raise ValueError('finite reference and declared strength required')
        self.reference,self.strength = reference,strength
        self.times = np.zeros(CAPACITY)
        self.logits = np.zeros(CAPACITY)
        self.scratch = np.zeros(CAPACITY)
        self.head = self.count = 0
        self.last_time = -np.inf

    def push(self, raw_logit, now):
        if not np.isfinite(raw_logit) or not np.isfinite(now) or now < 0 or now <= self.last_time:
            raise ValueError('finite logits and strictly increasing nonnegative times required')
        self.last_time = now
        while self.count and self.times[self.head] < now-HISTORY_S:
            self.head = (self.head+1)%CAPACITY; self.count -= 1
        offset = 0.
        if self.count:
            first = min(self.count,CAPACITY-self.head)
            self.scratch[:first] = self.logits[self.head:self.head+first]
            self.scratch[first:self.count] = self.logits[:self.count-first]
            median = np.median(self.scratch[:self.count],overwrite_input=True)
            offset = self.strength*min(self.count/WARMUP,1.)*np.clip(self.reference-median,-MAX_OFFSET,MAX_OFFSET)
        score = float(expit(raw_logit+offset))
        if self.count == CAPACITY:
            self.head = (self.head+1)%CAPACITY; self.count -= 1
        index = (self.head+self.count)%CAPACITY
        self.times[index],self.logits[index] = now,raw_logit
        self.count += 1
        return score,float(offset)


def predict_score(model, features):
    """Column15 is emitted time metadata, excluded from every learned quantity."""
    if np.ndim(features) != 2 or np.shape(features)[1] != 16:
        raise ValueError('fifteen acoustic features plus emission-time metadata required')
    logits = raw_logits(model,features[:,:15])
    stream = ScoreStream(model['reference_logit_median'],model['strength'])
    return np.asarray([stream.push(logit,now)[0] for logit,now in zip(logits,features[:,15])])


class FoldFitter:
    """Reuse exact frozen base coefficients and derive training-family priors only."""
    def __init__(self, frozen_variant):
        self.base_models = {}
        self.prepared = {}
        for row in frozen_variant['tracks']:
            for original in [row['model']]+[f['model'] for f in row['calibration']['inner_folds']]:
                model = {k:copy.deepcopy(original[k]) for k in ('training_tracks','mean','scale','weights','intercept')}
                key = tuple(model['training_tracks'])
                if key in self.base_models and model != self.base_models[key]:
                    raise ValueError('same training families have inconsistent frozen coefficients')
                self.base_models[key] = model

    def fit(self, records, held_out, strength):
        if strength not in STRENGTHS:
            raise ValueError('strength must be predeclared')
        acoustic = [dict(r,features=r['features'][:,:15]) for r in records]
        training,x,y,w = _training_data(acoustic,held_out)
        names = tuple(r['track'] for r in training)
        keys = [training_key(r) for r in training]
        signature = hashlib.sha256(json.dumps(keys).encode()).hexdigest()
        key = names,signature
        if key not in self.prepared:
            model = copy.deepcopy(self.base_models[names])
            mean = np.sum(x*w[:,None],axis=0)
            scale = np.maximum(np.sqrt(np.sum((x-mean)**2*w[:,None],axis=0)),1e-3)
            np.testing.assert_allclose(mean,model['mean'],rtol=0,atol=1e-12)
            np.testing.assert_allclose(scale,model['scale'],rtol=0,atol=1e-12)
            priors = [dict(track=r['track'],candidates=len(r['features']),median=float(np.median(raw_logits(model,r['features'])))) for r in training]
            model.update(reference_logit_median=float(np.mean([p['median'] for p in priors])),
                reference_by_training_family=priors,training_input_keys=keys,training_input_sha256=signature,
                history_seconds=HISTORY_S,capacity=CAPACITY,warmup_candidates=WARMUP,maximum_offset_logit=MAX_OFFSET,
                time_column_is_transport_metadata=True)
            self.prepared[key] = model
        model = copy.deepcopy(self.prepared[key]);model.update(held_out=held_out,strength=strength)
        return model

    def statistics(self):
        return dict(frozen_base_models=len(self.base_models), reference_profiles=len(self.prepared), new_base_fits=0)
