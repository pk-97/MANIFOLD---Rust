"""H18: exactly three frozen interaction/kernel/linear logit combinations."""
from __future__ import annotations
import copy
import hashlib
import json
import numpy as np
from scipy.special import expit
from .kick_coverage_blend import _models,component_signature
from .kick_kernel_blend import FrozenFitter as PairFitter,PredictionCache as PairCache
from .kick_interaction_score import basis

INTERACTION_WEIGHTS=(.10,.25,.50)

def frozen_rule():
    return dict(hypothesis='H18',configurations=[dict(interaction_logit_weight=w,kernel_logit_weight=.75*(1-w),linear_logit_weight=.25*(1-w)) for w in INTERACTION_WEIGHTS],
        score='expit((1-w)*(.75*raw_H16_gamma1over15_margin+.25*raw_H10_linear_logit)+w*raw_H10_interaction_logit)',
        components='Frozen H16 gamma1/15 and H10 coverage_linear15/coverage_interaction15(.01), exact same73-coverage training sets; no refits.',
        protocol='Original381/174+207 and9negative cores; unchanged full candidate/refractory replay and nested coarse+single-refinement cutoffs. Whole outer/inner families excluded from all3 components.',
        cache='Hash-verified H17 kernel/linear prediction pairs copied into owned h18 cache; no repeated support arrays in reports.',
        cpu_seconds_limit=90,guard='One active CPU child with default-fatal ITIMER_PROF; partial outputs preserved on limit.',
        acceptance='223hits with<=70extras OR250with<=89extras;zero9cores;<=1previously caught baseline label lost pertrack.',
        disclosure='Development experiment motivated by prior complementary errors, not independent confirmation. Additional known73 have other-family training reuse and are scored only after all381 outputs freeze.',
        prohibition='Exactly3 configurations; no refits/newfeatures/audio/labels/Pattern/D2/reserved/app integration or extra cutoff policy. No further research after this audit.')


class FrozenFitter:
    def __init__(self,linear,kernel,interaction,model_root):
        self.pair=PairFitter(linear,kernel,model_root);self.interaction=_models(interaction);self.requests=0
        if self.interaction.keys()!=self.pair.linear.keys():raise ValueError('component families differ')
        for names,component in self.interaction.items():
            for key in ('training_input_keys','mean','scale'):
                if component[key]!=self.pair.linear[names][key]:raise ValueError('component provenance/normalisation differs')
            if len(component['weights'])!=135 or component['regularisation']!=.01:raise ValueError('expected frozen interaction .01')

    def fit(self,records,held_out,weight):
        if isinstance(weight,(bool,np.bool_)) or weight not in INTERACTION_WEIGHTS:raise ValueError('only frozen .10/.25/.50 interaction weights')
        base=self.pair.fit(records,held_out,.75);interaction=copy.deepcopy(self.interaction[tuple(base['training_tracks'])]);interaction['held_out']=held_out;self.requests+=1
        return dict(training_tracks=base['training_tracks'],training_input_keys=base['training_input_keys'],held_out=held_out,mean=base['mean'],scale=base['scale'],base=base,interaction=interaction,
            interaction_logit_weight=weight,kernel_logit_weight=.75*(1-weight),linear_logit_weight=.25*(1-weight),interaction_sha256=component_signature(interaction),score_kind='fixed3-component raw logit blend, not calibrated probability')


class PredictionCache:
    def __init__(self,pair_cache=None):
        self.pair=PairCache(pair_cache);self.interactions={};self.computed=0;self.hits=0

    def __call__(self,model,features):
        weight=model['interaction_logit_weight'];base=model['base'];interaction=model['interaction']
        if not np.isfinite(weight) or not 0<=weight<=1 or model['kernel_logit_weight']!=.75*(1-weight) or model['linear_logit_weight']!=.25*(1-weight) or base['kernel_logit_weight']!=.75 or base['linear_logit_weight']!=.25:raise ValueError('invalid fixed complementary weights')
        for component in (base,interaction):
            for key in ('training_tracks','training_input_keys','mean','scale'):
                if component[key]!=model[key]:raise ValueError('component/root provenance differs')
        if component_signature(interaction)!=model['interaction_sha256']:raise ValueError('interaction signature differs')
        x=np.asarray(features,dtype=np.float64);a,b=self.pair.logits(base,x)
        digest=hashlib.sha256();digest.update(model['interaction_sha256'].encode());digest.update(str((x.shape,x.dtype.str)).encode());digest.update(x.tobytes());key=digest.hexdigest()
        if key not in self.interactions:
            self.interactions[key]=basis(x,interaction['mean'],interaction['scale'])@np.asarray(interaction['weights'])+interaction['intercept'];self.computed+=1
            if not np.isfinite(self.interactions[key]).all():raise ValueError('finite logits required')
        else:self.hits+=1
        return expit((1-weight)*(.25*a+.75*b)+weight*self.interactions[key])

    def statistics(self):return dict(kernel_linear=self.pair.statistics(),computed_interaction_pairs=self.computed,interaction_memory_hits=self.hits)


def predict_score(model,features):return PredictionCache()(model,features)
