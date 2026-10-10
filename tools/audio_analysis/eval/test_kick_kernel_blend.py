"""Fixed-weight algebra, held-out isolation and hashed no-fit cache controls."""
import copy
import hashlib
import json
from pathlib import Path
import tempfile
import unittest

import numpy as np
from scipy.special import expit

from .kick_boosted_score import training_key
from .kick_kernel_blend import FrozenFitter, KERNEL_WEIGHTS, PredictionCache, predict_score
from .kick_kernel_score import parameters
from .test_kick_coverage_blend import record


def variants(records, root):
    a,b=dict(tracks=[]),dict(tracks=[])
    def components(rows,held):
        training=[r for r in rows if r['track']!=held]
        common=dict(training_tracks=[r['track'] for r in training],
            training_input_keys=[list(training_key(r)) for r in training],mean=[.2]*15,scale=[2.]*15)
        key=hashlib.sha256(json.dumps(common,sort_keys=True).encode()).hexdigest()
        kernel=dict(common,training_input_sha256=key,gamma=1/15,parameters=parameters(1/15),fit_status=0,
            support_vectors=[[-.5]*15,[.7]*15],dual_coefficients=[-.4,.6],intercept=-.1)
        payload=dict(model=kernel,model_sha256=hashlib.sha256(json.dumps(kernel,sort_keys=True).encode()).hexdigest())
        (root/f'{key}.json').write_text(json.dumps(payload))
        return dict(common,held_out=held,weights=np.linspace(-.3,.3,15).tolist(),intercept=.1),dict(kernel,held_out=held)
    for outer in records:
        linear,kernel=components(records,outer['track'])
        ra=dict(track=outer['track'],model=linear,calibration=dict(inner_folds=[]))
        rb=dict(track=outer['track'],model=kernel,calibration=dict(inner_folds=[]))
        subset=[r for r in records if r['track']!=outer['track']]
        for inner in subset:
            linear,kernel=components(subset,inner['track'])
            ra['calibration']['inner_folds'].append(dict(validation_track=inner['track'],model=linear))
            rb['calibration']['inner_folds'].append(dict(validation_track=inner['track'],model=kernel))
        a['tracks'].append(ra);b['tracks'].append(rb)
    return a,b


class KernelBlendTests(unittest.TestCase):
    def setUp(self):
        self.temp=tempfile.TemporaryDirectory();self.addCleanup(self.temp.cleanup);self.root=Path(self.temp.name)
        self.records=[record('a',0),record('b',1),record('held',2)]
        self.linear,self.kernel=variants(self.records,self.root)
        self.bank=FrozenFitter(self.linear,self.kernel,self.root)

    def test_export_scalar_batch_and_exact_raw_logit_weights(self):
        x=np.vstack((np.linspace(-5,5,15),np.ones(15)*1e20,np.zeros(15)))
        z=np.clip((x-.2)/2,-8,8);a=z@np.linspace(-.3,.3,15)+.1
        b=np.exp(-np.sum((z[:,None,:]-np.array([[-.5]*15,[.7]*15]))**2,axis=2)/15)@np.array([-.4,.6])-.1
        for weight in KERNEL_WEIGHTS:
            model=json.loads(json.dumps(self.bank.fit(self.records,'held',weight)))
            scores=predict_score(model,x)
            np.testing.assert_allclose(scores,expit((1-weight)*a+weight*b),atol=1e-15,rtol=1e-15)
            np.testing.assert_allclose(scores,[predict_score(model,[row])[0] for row in x],atol=1e-15,rtol=1e-15)
            self.assertEqual(predict_score(model,np.empty((0,15))).shape,(0,))
        for weight,expected in ((0,a),(1,b)):
            model.update(kernel_logit_weight=weight,linear_logit_weight=1-weight)
            np.testing.assert_allclose(predict_score(model,x),expit(expected),atol=1e-15,rtol=1e-15)

    def test_whole_family_exclusion_and_changed_inputs_rejected(self):
        outer=self.bank.fit(self.records,'held',.5);inner=self.bank.fit(self.records[:2],'b',.5)
        changed=copy.deepcopy(self.records);changed[0]['labels'][0]=1
        with self.assertRaisesRegex(ValueError,'provenance'):self.bank.fit(changed,'held',.5)
        with self.assertRaisesRegex(ValueError,'ordered'):self.bank.fit(self.records[::-1],'held',.5)
        for weight in (.6,True):
            with self.assertRaisesRegex(ValueError,'frozen'):self.bank.fit(self.records,'held',weight)
        self.records[2].update(features=None,training_mask=None,labels=None,event_ids=None)
        self.assertEqual(outer,self.bank.fit(self.records,'held',.5))
        self.records[1].update(features=None,training_mask=None,labels=None,event_ids=None)
        self.assertEqual(inner,self.bank.fit(self.records[:2],'b',.5))
        changed=copy.deepcopy(self.kernel);changed['tracks'][0]['model']['mean'][0]+=1
        with self.assertRaisesRegex(ValueError,'normalisation'):FrozenFitter(self.linear,changed,self.root)
        changed=copy.deepcopy(self.kernel);changed['tracks'][0]['model']['training_tracks'].append('a')
        with self.assertRaisesRegex(ValueError,'exclusion'):FrozenFitter(self.linear,changed,self.root)

    def test_cache_reuses_raw_pairs_and_rejects_tampering(self):
        cache=PredictionCache(self.root/'predictions');x=self.records[0]['features'];scores=[]
        for weight in KERNEL_WEIGHTS:scores.append(cache(self.bank.fit(self.records,'held',weight),x))
        self.assertEqual(cache.computed,1);self.assertEqual(cache.memory_hits,2)
        model=self.bank.fit(self.records,'held',.5)
        again=PredictionCache(self.root/'predictions');np.testing.assert_array_equal(again(model,x),scores[1]);self.assertEqual(again.disk_hits,1)
        cache(model,x+.1);self.assertEqual(cache.computed,2)
        changed=copy.deepcopy(model);changed['linear']['intercept']+=1
        with self.assertRaisesRegex(ValueError,'signature'):cache(changed,x)
        changed=copy.deepcopy(model);changed['linear_logit_weight']=.6
        with self.assertRaisesRegex(ValueError,'weights'):cache(changed,x)
        receipt=next((self.root/'predictions').glob('*.json'));data=json.loads(receipt.read_text());data['data_sha256']='bad';receipt.write_text(json.dumps(data))
        probe=PredictionCache(self.root/'predictions')
        with self.assertRaisesRegex(ValueError,'cache differs'):
            for values in (x,x+.1):probe(model,values)
        path=Path(model['kernel_reference']['path']);path.write_text(path.read_text()+' ')
        with self.assertRaisesRegex(ValueError,'checksum'):PredictionCache()(model,x)


if __name__=='__main__':unittest.main()
