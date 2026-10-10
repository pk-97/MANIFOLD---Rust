"""Three fixed logit blends of previously fitted linear15 and depth3 scorers."""
from __future__ import annotations

import copy
import hashlib
import json

import numpy as np
from scipy.special import expit, logit

from .kick_boosted_score import predict_score as tree_predict, training_key

TREE_WEIGHTS = (.25, .5, .75)


def _component_signature(model, kind):
    fields = ('mean', 'scale', 'weights', 'intercept') if kind == 'linear' else (
        'mean', 'scale', 'initial_log_odds', 'learning_rate', 'max_depth', 'trees')
    return hashlib.sha256(json.dumps({k:model[k] for k in fields}, sort_keys=True).encode()).hexdigest()


def _models(variant):
    result = {}
    for row in variant['tracks']:
        models = [row['model']] + [f['model'] for f in row['calibration']['inner_folds']]
        for model in models:
            names = tuple(model['training_tracks'])
            if not names or len(names) != len(set(names)) or model['held_out'] in names:
                raise ValueError('invalid cached training-song exclusion')
            if names in result:
                kind = 'linear' if 'weights' in model else 'tree'
                if _component_signature(result[names], kind) != _component_signature(model, kind):
                    raise ValueError('same training songs have different cached predictions')
            result[names] = model
    return result


class FrozenFitter:
    """Provide exact matching cached components; this class performs no fitting."""

    def __init__(self, linear_variant, tree_variant):
        self.linear, self.tree = _models(linear_variant), _models(tree_variant)
        if self.linear.keys() != self.tree.keys():
            raise ValueError('cached components cover different training-song sets')
        self.requests = 0
        for names, linear in self.linear.items():
            tree = self.tree[names]
            if tree['max_depth'] != 3:
                raise ValueError('anchored fusion requires the frozen depth3 model')
            if linear['mean'] != tree['mean'] or linear['scale'] != tree['scale']:
                raise ValueError('cached components have different fold-only normalisation')

    def fit(self, records, held_out, tree_weight):
        if tree_weight not in TREE_WEIGHTS:
            raise ValueError('only the frozen .25/.5/.75 tree logit weights are allowed')
        names = [r['track'] for r in records]
        if len(names) != len(set(names)):
            raise ValueError('track identities must be unique')
        training = [r for r in records if r['track'] != held_out]
        names = tuple(r['track'] for r in training)
        if names not in self.tree:
            raise ValueError('no cached model for this exact ordered training-song set')
        tree = self.tree[names]
        keys = [list(training_key(r)) for r in training]
        if keys != tree['training_input_keys']:
            raise ValueError('current training inputs differ from cached component provenance')
        self.requests += 1
        return dict(training_tracks=list(names), held_out=held_out,
            training_input_keys=keys, tree_logit_weight=tree_weight,
            linear_logit_weight=1-tree_weight,
            component_sha256=dict(linear=_component_signature(self.linear[names], 'linear'),
                                  tree=_component_signature(tree, 'tree')),
            linear=copy.deepcopy(self.linear[names]), tree=copy.deepcopy(tree),
            score_kind='fixed blend of balanced linear/tree logits; not event probability')


def predict_score(model, features):
    """Blend frozen row-local scores before the unchanged event decision stage."""
    x = np.asarray(features, dtype=np.float64)
    # The tree predictor performs dimension/finiteness validation and float32
    # split routing. The linear arithmetic remains identical to its reference.
    tree_logits = logit(tree_predict(model['tree'], x))
    if not np.all(np.isfinite(tree_logits)):
        raise ValueError('frozen tree scores saturated at zero or one')
    linear = model['linear']
    z = np.clip((x-np.asarray(linear['mean']))/np.asarray(linear['scale']), -8, 8)
    linear_logits = z @ np.asarray(linear['weights']) + linear['intercept']
    return expit(model['linear_logit_weight']*linear_logits + model['tree_logit_weight']*tree_logits)
