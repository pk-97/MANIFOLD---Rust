"""Parity and causal availability proofs for the bounded reference stream."""
import unittest

import numpy as np

from .kick_fusion_bandwise import fusion_features
from .kick_streaming_reference import DecisionStream, FeatureStream
from .run_kick_fusion_trial import predict_score


def stimulus(sr):
    t = np.arange(round(1.5*sr))/sr
    rng = np.random.default_rng(31)
    x = .08*np.sin(2*np.pi*53*t)
    for onset in (0,.333,.71,1.02):
        age = t-onset; positive = age >=0
        x[positive] += .4*np.exp(-age[positive]/.045)*np.sin(2*np.pi*(70*age[positive]+.5*(1-np.exp(-age[positive]*50))))
    x += .03*rng.normal(size=len(t))*np.exp(-np.maximum(t-.43,0)/.018)*(t>=.43)
    x[t>1.2] *= .12
    return x


def stream_values(samples,sr):
    stream = FeatureStream(sr); rows=[]
    for i in range(len(samples)//stream.hop):
        result = stream.push_hop(samples[i*stream.hop:(i+1)*stream.hop])
        if result:
            c,a,f = result;rows.append((c,a,f.copy()))
    return stream,rows


class StreamingTests(unittest.TestCase):
    def test_exact_candidates_and_feature_parity_at_both_rates(self):
        for sr in (44100,48000):
            samples = stimulus(sr)
            c,a,f,hop = fusion_features(samples,sr)
            stream,rows = stream_values(samples,sr)
            self.assertEqual(hop,stream.hop)
            np.testing.assert_array_equal([r[0] for r in rows],c)
            np.testing.assert_array_equal([r[1] for r in rows],a)
            np.testing.assert_allclose(np.asarray([r[2] for r in rows]),f,rtol=1e-11,atol=1e-11)
            self.assertLessEqual(stream.horizon*hop/sr,.070)

    def test_silence_and_retained_storage_are_bounded(self):
        stream = FeatureStream(48000); before=stream.retained_array_bytes()
        for _ in range(400):
            self.assertIsNone(stream.push_hop(np.zeros(stream.hop)))
        self.assertEqual(stream.retained_array_bytes(),before)
        self.assertLess(before,100000)

    def test_prefix_and_real_availability(self):
        sr=48000;samples=stimulus(sr);stream,full=stream_values(samples,sr)
        end=round(.8*sr)//stream.hop*stream.hop
        _,prefix=stream_values(samples[:end],sr)
        wanted=[r for r in full if (r[1]+1)*stream.hop<=end]
        self.assertEqual([(r[0],r[1]) for r in prefix],[(r[0],r[1]) for r in wanted])
        np.testing.assert_array_equal([r[2] for r in prefix],[r[2] for r in wanted])
        self.assertTrue(all(a-c==stream.horizon for c,a,_ in full))

    def test_linear_decision_parity_and_actual_refractory(self):
        model=dict(mean=[1,2],scale=[.5,2],weights=[1,2],intercept=.1)
        stream=DecisionStream(model,.5,48000,256)
        x=np.array([[1,2],[0,-10],[10,10]])
        np.testing.assert_allclose([stream.score(r) for r in x],predict_score(model,x))
        self.assertTrue(stream.push(x[2],20)[1])
        self.assertFalse(stream.push(x[2],31)[1])
        self.assertTrue(stream.push(x[2],32)[1])


if __name__ == '__main__':
    unittest.main()
