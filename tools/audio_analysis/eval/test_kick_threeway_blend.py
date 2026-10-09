"""Exact three-way algebra, complete-family isolation and tamper rejection."""
import copy
import json
import unittest
import numpy as np
from scipy.special import expit
from . import test_kick_kernel_blend as fixture
from .kick_threeway_blend import FrozenFitter,INTERACTION_WEIGHTS,PredictionCache,predict_score


class ThreewayTests(unittest.TestCase):
    def setUp(self):
        self.fixture=fixture.KernelBlendTests();self.fixture.setUp();self.addCleanup(self.fixture.doCleanups)
        self.records=self.fixture.records;self.interaction=copy.deepcopy(self.fixture.linear)
        for row in self.interaction['tracks']:
            for m in [row['model']]+[f['model'] for f in row['calibration']['inner_folds']]:m.update(weights=np.linspace(-.1,.1,135).tolist(),regularisation=.01,intercept=-.2)
        self.bank=FrozenFitter(self.fixture.linear,self.fixture.kernel,self.interaction,self.fixture.root)

    def test_fixed_algebra_scalar_export_endpoints(self):
        x=np.vstack((np.linspace(-5,5,15),np.ones(15)*1e20,np.zeros(15)));z=np.clip((x-.2)/2,-8,8);b=np.tanh(z/2)
        design=np.stack([z[:,i] for i in range(15)]+[b[:,i]*b[:,j] for i in range(15) for j in range(i,15)],axis=1)
        interaction=design@np.linspace(-.1,.1,135)-.2;linear=z@np.linspace(-.3,.3,15)+.1
        kernel=np.exp(-np.sum((z[:,None,:]-np.array([[-.5]*15,[.7]*15]))**2,axis=2)/15)@np.array([-.4,.6])-.1
        cache=PredictionCache()
        for weight in INTERACTION_WEIGHTS:
            m=json.loads(json.dumps(self.bank.fit(self.records,'held',weight)));expected=expit((1-weight)*(.25*linear+.75*kernel)+weight*interaction)
            np.testing.assert_allclose(cache(m,x),expected,atol=1e-15,rtol=1e-15)
            np.testing.assert_allclose(predict_score(m,x),[predict_score(m,[v])[0] for v in x],atol=1e-15,rtol=1e-15)
            self.assertEqual(predict_score(m,np.empty((0,15))).shape,(0,))
        self.assertEqual(cache.computed,1);self.assertEqual(cache.pair.computed,1)
        for weight,value in ((0,.25*linear+.75*kernel),(1,interaction)):
            m.update(interaction_logit_weight=weight,kernel_logit_weight=.75*(1-weight),linear_logit_weight=.25*(1-weight));np.testing.assert_allclose(predict_score(m,x),expit(value),atol=1e-15,rtol=1e-15)

    def test_excluded_families_and_changed_training_rejected(self):
        outer=self.bank.fit(self.records,'held',.25);inner=self.bank.fit(self.records[:2],'b',.25)
        changed=copy.deepcopy(self.records);changed[0]['labels'][0]=1
        with self.assertRaisesRegex(ValueError,'provenance'):self.bank.fit(changed,'held',.25)
        with self.assertRaisesRegex(ValueError,'ordered'):self.bank.fit(self.records[::-1],'held',.25)
        for weight in (.75,True):
            with self.assertRaisesRegex(ValueError,'frozen'):self.bank.fit(self.records,'held',weight)
        self.records[2].update(features=None,labels=None,training_mask=None,event_ids=None);self.assertEqual(outer,self.bank.fit(self.records,'held',.25))
        self.records[1].update(features=None,labels=None,training_mask=None,event_ids=None);self.assertEqual(inner,self.bank.fit(self.records[:2],'b',.25))

    def test_component_hash_weights_and_family_rejection(self):
        m=self.bank.fit(self.records,'held',.25);x=self.records[0]['features']
        for field in ('linear_logit_weight','kernel_logit_weight'):
            changed=copy.deepcopy(m);changed[field]+=.01
            with self.assertRaisesRegex(ValueError,'weights'):predict_score(changed,x)
        changed=copy.deepcopy(m);changed['interaction']['intercept']+=1
        with self.assertRaisesRegex(ValueError,'signature'):predict_score(changed,x)
        changed=copy.deepcopy(m);changed['interaction']['mean'][0]+=1
        with self.assertRaisesRegex(ValueError,'provenance'):predict_score(changed,x)
        changed=copy.deepcopy(self.interaction);changed['tracks'][0]['model']['training_tracks'].append('a')
        with self.assertRaisesRegex(ValueError,'exclusion'):FrozenFitter(self.fixture.linear,self.fixture.kernel,changed,self.fixture.root)
        changed=copy.deepcopy(self.interaction);changed['tracks'][0]['model']['mean'][0]+=1
        with self.assertRaisesRegex(ValueError,'normalisation'):FrozenFitter(self.fixture.linear,self.fixture.kernel,changed,self.fixture.root)


if __name__=='__main__':unittest.main()
