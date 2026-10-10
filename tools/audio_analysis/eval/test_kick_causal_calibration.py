"""Past-only state, cold starts, bounded history and family exclusion."""
import copy
import unittest

import numpy as np
from scipy.special import expit

from .kick_causal_calibration import ScoreStream,CAPACITY,MAX_OFFSET,predict_score,raw_logits,FoldFitter
from .run_kick_fusion_trial import fit_without
from .test_kick_interaction_score import record


class CalibrationTests(unittest.TestCase):
    def test_past_only_and_gap(self):
        stream=ScoreStream(2.,1.)
        score,offset=stream.push(-1.,0.)
        self.assertEqual(score,expit(-1.));self.assertEqual(offset,0.)
        for i in range(1,65):stream.push(0.,i*.01)
        score,offset=stream.push(-2.,.65)
        self.assertEqual(offset,2.);self.assertEqual(score,.5)
        score,offset=stream.push(10.,9.)
        self.assertEqual(offset,0.);self.assertEqual(score,expit(10.))
        self.assertEqual(stream.count,1)

    def test_prefix_and_zero_strength(self):
        rng=np.random.default_rng(3);values=rng.normal(size=300);times=np.arange(300)*.02
        def run(n,strength):
            stream=ScoreStream(.4,strength)
            return np.array([stream.push(x,t) for x,t in zip(values[:n],times[:n])])
        np.testing.assert_array_equal(run(100,.5),run(300,.5)[:100])
        np.testing.assert_array_equal(run(300,0.)[:,0],expit(values))
        self.assertLessEqual(np.abs(run(300,1.)[:,1]).max(),MAX_OFFSET)

    def test_bounded_ring_and_time_validation(self):
        stream=ScoreStream(-1.,1.)
        for i in range(CAPACITY+100):stream.push(float(i%7),i*.001)
        self.assertEqual(stream.count,CAPACITY)
        self.assertEqual(stream.times.nbytes+stream.logits.nbytes+stream.scratch.nbytes,CAPACITY*3*8)
        with self.assertRaises(ValueError):stream.push(0.,.1)

    def test_frozen_model_and_excluded_family(self):
        original=[record('a'),record('b'),record('held')]
        frozen=dict(tracks=[])
        for row in original:
            train=[r for r in original if r['track']!=row['track']]
            frozen['tracks'].append(dict(model=fit_without(original,row['track']),calibration=dict(inner_folds=[dict(model=fit_without(train,r['track'])) for r in train])))
        rows=[dict(r,features=np.column_stack((r['features'],np.arange(len(r['features']))*.1))) for r in original]
        fitter=FoldFitter(frozen);model=fitter.fit(rows,'held',.5)
        self.assertEqual(fitter.statistics(),dict(frozen_base_models=6,reference_profiles=1,new_base_fits=0))
        changed=copy.deepcopy(rows);changed[-1]['features'][:,:15]*=10000
        self.assertEqual(model,fitter.fit(changed,'held',.5))
        np.testing.assert_array_equal(model['weights'],frozen['tracks'][-1]['model']['weights'])
        scores=predict_score(model,rows[-1]['features'])
        self.assertEqual(scores[0],expit(raw_logits(model,rows[-1]['features'][:1,:15]))[0])
        self.assertEqual({r['track'] for r in model['reference_by_training_family']},{'a','b'})
        changed=copy.deepcopy(rows)
        for r in changed:r['features'][:,15]*=5
        self.assertEqual(model,fitter.fit(changed,'held',.5))


if __name__=='__main__':unittest.main()
