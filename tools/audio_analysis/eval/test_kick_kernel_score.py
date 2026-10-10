"""Kernel serialization, exact weights and fatal CPU guard proofs."""
import tempfile
import unittest

import numpy as np

from .kick_kernel_score import (GAMMAS,FoldFitter,KernelRuntime,TrainingBudgetExceeded,
    cpu_limited,decision_values,parameters,predict_score)
from .kick_subspace_score import _training_data


def busy_loop():
    while True:pass


def fixture():
    rng=np.random.default_rng(16);records=[]
    for name in ('a','b','outer','inner'):
        y=np.tile([0,1],16);x=rng.normal(size=(32,15));x[:,0]+=y*2
        records.append(dict(track=name,features=x,labels=y,event_ids=np.where(y,np.arange(32)//4,-1),training_mask=np.ones(32,bool)))
    return records


class KernelTests(unittest.TestCase):
    def test_os_cpu_guard_does_not_return_partial_model(self):
        with self.assertRaises(TrainingBudgetExceeded):cpu_limited(busy_loop,(),.03)

    def test_frozen_inputs_weights_cache_and_exported_predictions(self):
        rows=fixture();training=rows[:2]+rows[3:]
        with tempfile.TemporaryDirectory() as cache:
            fitter=FoldFitter(cache);model=fitter.fit(training,'inner',GAMMAS[0])
            _,x,y,w=_training_data(training,'inner')
            np.testing.assert_array_equal(model['mean'],np.sum(x*w[:,None],axis=0))
            self.assertAlmostEqual(model['positive_weight_mass'],.5);self.assertAlmostEqual(model['negative_weight_mass'],.5)
            self.assertEqual(model['fit_status'],0);self.assertLessEqual(model['export_margin_max_abs_error'],1e-9)
            rows[2]['features'][:]=np.nan;rows[3]['features'][:]=np.nan
            self.assertEqual(model,fitter.fit(training,'inner',GAMMAS[0]));self.assertEqual(model['training_tracks'],['a','b'])
            reused=FoldFitter(cache);self.assertEqual(model,reused.fit(training,'inner',GAMMAS[0]));self.assertEqual(reused.disk_hits,1)
            points=rows[0]['features'];scores=predict_score(model,points);runtime=KernelRuntime(model);size=runtime.retained_array_bytes()
            np.testing.assert_allclose(scores,[runtime.score(x) for x in points],atol=1e-12,rtol=0)
            np.testing.assert_allclose(scores,[predict_score(model,x[None,:])[0] for x in points],atol=1e-12,rtol=0)
            self.assertEqual(size,runtime.retained_array_bytes());self.assertEqual(predict_score(model,np.empty((0,15))).shape,(0,))

    def test_exact_three_configs_and_input_rejection(self):
        self.assertEqual(GAMMAS,(1/60,1/30,1/15))
        for g in GAMMAS:
            p=parameters(g);self.assertEqual(p['C'],100);self.assertFalse(p['probability']);self.assertEqual(p['tol'],1e-5)
        for g in (0,.1,True):
            with self.assertRaises(ValueError):parameters(g)
        with self.assertRaises(ValueError):decision_values(dict(mean=[0]*15,scale=[1]*15),np.ones((1,14)))


if __name__=='__main__':unittest.main()
