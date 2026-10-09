"""H13: three fixed prefixes of the existing compact temporal representation."""
from __future__ import annotations

import numpy as np

from .kick_attack_background import AttackBackgroundStream
from .kick_shape_features import FEATURE_NAMES, from_cached_features

CONFIGURATIONS = ('delays18', 'concordance21', 'concentration24')
DIMENSIONS = dict(zip(CONFIGURATIONS, (18,21,24)))


def frozen_rule():
    return dict(hypothesis='H13',configurations=list(CONFIGURATIONS),dimensions=DIMENSIONS,
        model=dict(n_estimators=64,learning_rate=.05,max_depth=3,min_weight_fraction_leaf=.005,
                   random_state=0,subsample=1.,n_iter_no_change=None),
        feature_order=list(FEATURE_NAMES),
        representation='Reuse kick_shape_features.from_cached_features exactly; first15 unchanged, then signed delays15:18, concordance18:21, concentration21:24.',
        formula='For each band, softmax the8 cached clipped log-power observations into temporal mass p. Centre=sum(p*i/7). Signed centres:low-body,low-upper,body-upper. Centred q=p-1/8; concordance=2*sum(qa*qb)/(sum(qa²)+sum(qb²)+1e-12), same pair order. Concentration=clip(sum(q²)*8/7,0,1),low/body/upper.',
        input='Immutable native39 caches, same candidate grid,40ms rounded horizon and60ms refractory. No audio decode/FFT or new measurements for model fitting.',
        weights='Exact original song/class/positive-event sample weights; fold-only mean/scale with z clipping8; outer and inner families excluded from every learned stage.',
        calibration='Existing coarse threshold search plus one fixed bracket refinement, unchanged wide-extra and kick-free budgets.',
        expected_failure='These compressed relationships may still overlap bass and other attacks, inherit trajectory clipping, and cause timbre-dependent partitions or calibration shifts.',
        bounded_reference='Reuse AttackBackgroundStream unchanged to obtain original39, discard its six background descriptors, then apply the existing compact transform. This avoids a second DSP implementation; its extra unused work is included in measured cost.',
        prohibition='Exactly these3 configurations,381 labels and9 songs. No tuning, extra fits/configurations,73-label training, reserved material, neural/GPU work or app integration.')


def configured_features(cached39, configuration):
    if configuration not in DIMENSIONS:
        raise ValueError('configuration was not predeclared')
    return from_cached_features(cached39)[:,:DIMENSIONS[configuration]]


def feature_names(configuration):
    if configuration not in DIMENSIONS:
        raise ValueError('configuration was not predeclared')
    return FEATURE_NAMES[:DIMENSIONS[configuration]]


class CompactShapeStream:
    """Bounded shared DSP reference; borrowed24-value output, no new horizon."""
    def __init__(self,sample_rate):
        self.base=AttackBackgroundStream(sample_rate)
        self.hop=self.base.hop
        self.output=np.zeros(24)

    def retained_array_bytes(self):
        return self.base.retained_array_bytes()+self.output.nbytes

    def push_hop(self,samples):
        row=self.base.push_hop(samples)
        if row is None:return None
        c,a,features=row
        self.output[:]=from_cached_features(features[None,:39])[0]
        return c,a,self.output
