"""Fixed logit shrinkage between the two completed H10 smooth scorers."""
from __future__ import annotations

import copy
import hashlib
import json

import numpy as np
from scipy.special import expit

from .kick_boosted_score import training_key
from .kick_interaction_score import basis, predict_score as interaction_predict
from .run_kick_fusion_trial import predict_score as linear_predict

INTERACTION_WEIGHTS = (.25,.5,.75)


def component_signature(model):
    """The exclusion name can differ when the same seven families are reused."""
    return hashlib.sha256(json.dumps({k:v for k,v in model.items() if k!='held_out'},
                                    sort_keys=True).encode()).hexdigest()


def _models(variant):
    result = {}
    for row in variant['tracks']:
        outer = row['track']
        models = [(row['model'],{outer})]+[(f['model'],{outer,f['validation_track']})
                                          for f in row['calibration']['inner_folds']]
        for model,excluded in models:
            names = tuple(model['training_tracks'])
            if (not names or len(names)!=len(set(names)) or excluded.intersection(names)
                    or model['held_out'] in names):
                raise ValueError('invalid cached family exclusion')
            if [k[0] for k in model['training_input_keys']] != list(names):
                raise ValueError('cached training provenance names differ')
            if names in result and component_signature(result[names])!=component_signature(model):
                raise ValueError('same training families have different cached components')
            result[names] = model
    return result


class FrozenFitter:
    """Select matching completed components; never optimise any model."""
    def __init__(self, linear_variant, interaction_variant):
        self.linear,self.interaction = _models(linear_variant),_models(interaction_variant)
        self.requests = 0
        if self.linear.keys()!=self.interaction.keys():
            raise ValueError('component training family sets differ')
        for names,linear in self.linear.items():
            interaction = self.interaction[names]
            if linear['training_input_keys'] != interaction['training_input_keys']:
                raise ValueError('component training provenance differs')
            if linear['mean']!=interaction['mean'] or linear['scale']!=interaction['scale']:
                raise ValueError('component fold normalisation differs')
            if len(linear['weights'])!=15 or len(interaction['weights'])!=135 or interaction['regularisation']!=.01:
                raise ValueError('expected covered linear15 and interaction .01')

    def fit(self, records, held_out, interaction_weight):
        if isinstance(interaction_weight,(bool,np.bool_)) or interaction_weight not in INTERACTION_WEIGHTS:
            raise ValueError('only frozen .25/.5/.75 interaction weights allowed')
        if len({r['track'] for r in records})!=len(records):
            raise ValueError('training families must be unique')
        training = [r for r in records if r['track']!=held_out]
        names = tuple(r['track'] for r in training)
        if names not in self.linear:
            raise ValueError('no cached ordered training family set')
        linear,interaction = self.linear[names],self.interaction[names]
        keys = [list(training_key(r)) for r in training]
        if keys!=linear['training_input_keys'] or keys!=interaction['training_input_keys']:
            raise ValueError('current training inputs differ from cached provenance')
        a,b = copy.deepcopy(linear),copy.deepcopy(interaction)
        a['held_out']=b['held_out']=held_out
        self.requests += 1
        return dict(training_tracks=list(names),held_out=held_out,training_input_keys=keys,
            mean=linear['mean'].copy(),scale=linear['scale'].copy(),
            interaction_logit_weight=interaction_weight,linear_logit_weight=1-interaction_weight,
            component_sha256=dict(linear=component_signature(a),interaction=component_signature(b)),
            linear=a,interaction=b,score_kind='fixed covered linear/interaction logit blend; not probability')


def predict_score(model, features):
    """Use raw finite logits; do not invert saturated sigmoid probabilities."""
    x = np.asarray(features,dtype=np.float64)
    weight = model['interaction_logit_weight']
    if (not np.isfinite(weight) or not 0<=weight<=1
            or model['linear_logit_weight']!=1-weight):
        raise ValueError('invalid complementary blend weights')
    linear,interaction = model['linear'],model['interaction']
    if linear['mean']!=interaction['mean'] or linear['scale']!=interaction['scale']:
        raise ValueError('component fold normalisation differs')
    design = basis(x,interaction['mean'],interaction['scale'])
    # Endpoints exist solely for exact algebra/replay controls, not experiments.
    if weight==0: return linear_predict(linear,x)
    if weight==1: return interaction_predict(interaction,x)
    linear_logits = design[:,:15] @ np.asarray(linear['weights'])+linear['intercept']
    interaction_logits = design @ np.asarray(interaction['weights'])+interaction['intercept']
    if not np.isfinite(linear_logits).all() or not np.isfinite(interaction_logits).all():
        raise ValueError('component logits must be finite')
    return expit((1-weight)*linear_logits+weight*interaction_logits)
