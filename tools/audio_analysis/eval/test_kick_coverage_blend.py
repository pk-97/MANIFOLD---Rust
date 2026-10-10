"""Logit algebra, complete-family reuse and immutable-input guards."""
import copy
import json
import unittest

import numpy as np
from scipy.special import expit

from .kick_boosted_score import training_key
from .kick_coverage_blend import FrozenFitter, INTERACTION_WEIGHTS, predict_score
from .kick_interaction_score import predict_score as interaction_predict
from .run_kick_fusion_trial import predict_score as linear_predict


def record(name,offset):
    return dict(track=name,features=np.arange(45,dtype=float).reshape(3,15)/10+offset,
        training_mask=np.ones(3,bool),labels=np.array([0,1,1]),event_ids=np.array([-1,0,0]))


def components(records,held):
    training=[r for r in records if r['track']!=held]
    common=dict(training_tracks=[r['track'] for r in training],held_out=held,
        training_input_keys=[list(training_key(r)) for r in training],mean=[.2]*15,scale=[2.]*15)
    return (dict(common,weights=np.linspace(-.3,.3,15).tolist(),intercept=.1),
            dict(common,weights=np.linspace(-.1,.1,135).tolist(),intercept=-.2,regularisation=.01))


def variants(records):
    a,b=dict(tracks=[]),dict(tracks=[])
    for outer in records:
        linear,interaction=components(records,outer['track'])
        ra=dict(track=outer['track'],model=linear,calibration=dict(inner_folds=[]))
        rb=dict(track=outer['track'],model=interaction,calibration=dict(inner_folds=[]))
        subset=[r for r in records if r['track']!=outer['track']]
        for inner in subset:
            linear,interaction=components(subset,inner['track'])
            ra['calibration']['inner_folds'].append(dict(validation_track=inner['track'],model=linear))
            rb['calibration']['inner_folds'].append(dict(validation_track=inner['track'],model=interaction))
        a['tracks'].append(ra);b['tracks'].append(rb)
    return a,b


class CoverageBlendTests(unittest.TestCase):
    def setUp(self):
        self.records=[record('a',0),record('b',1),record('held',2)]
        self.linear,self.interaction=variants(self.records)

    def test_logit_algebra_scalar_batch_and_export(self):
        bank=FrozenFitter(self.linear,self.interaction)
        x=np.vstack((np.linspace(-5,5,15),np.ones(15)*1e20,np.zeros(15)))
        z=np.clip((x-.2)/2,-8,8);b=np.tanh(z/2)
        design=np.stack([z[:,i] for i in range(15)]+[b[:,i]*b[:,j] for i in range(15) for j in range(i,15)],axis=1)
        a=z@np.linspace(-.3,.3,15)+.1; c=design@np.linspace(-.1,.1,135)-.2
        for weight in INTERACTION_WEIGHTS:
            model=json.loads(json.dumps(bank.fit(self.records,'held',weight)))
            scores=predict_score(model,x)
            np.testing.assert_allclose(scores,expit((1-weight)*a+weight*c),atol=1e-15,rtol=1e-15)
            np.testing.assert_allclose(scores,[predict_score(model,[v])[0] for v in x],atol=1e-15,rtol=1e-15)
            self.assertEqual(predict_score(model,np.empty((0,15))).shape,(0,))

    def test_endpoints_recover_original_predictors_exactly(self):
        bank=FrozenFitter(self.linear,self.interaction);model=bank.fit(self.records,'held',.5)
        x=np.vstack([r['features'] for r in self.records])
        for weight,predict,key in ((0,linear_predict,'linear'),(1,interaction_predict,'interaction')):
            model.update(interaction_logit_weight=weight,linear_logit_weight=1-weight)
            np.testing.assert_array_equal(predict_score(model,x),predict(model[key],x))

    def test_excluded_family_arrays_never_enter_reuse(self):
        bank=FrozenFitter(self.linear,self.interaction);outer=bank.fit(self.records,'held',.5)
        inner=bank.fit(self.records[:2],'b',.5)
        self.records[2].update(features=None,training_mask=None,labels=None,event_ids=None)
        self.assertEqual(outer,bank.fit(self.records,'held',.5))
        self.records[1].update(features=None,training_mask=None,labels=None,event_ids=None)
        self.assertEqual(inner,bank.fit(self.records[:2],'b',.5))
        leaked=copy.deepcopy(self.interaction);leaked['tracks'][0]['model']['training_tracks'].append('a')
        with self.assertRaisesRegex(ValueError,'exclusion'):FrozenFitter(self.linear,leaked)

    def test_changed_input_or_component_is_rejected(self):
        bank=FrozenFitter(self.linear,self.interaction)
        changed=copy.deepcopy(self.records);changed[0]['labels'][0]=1
        with self.assertRaisesRegex(ValueError,'provenance'):bank.fit(changed,'held',.5)
        with self.assertRaisesRegex(ValueError,'ordered'):bank.fit(self.records[::-1],'held',.5)
        with self.assertRaisesRegex(ValueError,'frozen'):bank.fit(self.records,'held',.6)
        changed=copy.deepcopy(self.interaction);changed['tracks'][0]['model']['mean'][0]+=1
        with self.assertRaisesRegex(ValueError,'normalisation'):FrozenFitter(self.linear,changed)
        changed=copy.deepcopy(self.interaction);changed['tracks'][0]['calibration']['inner_folds'][0]['model']['intercept']+=1
        with self.assertRaisesRegex(ValueError,'different cached'):FrozenFitter(self.linear,changed)


if __name__=='__main__':unittest.main()
