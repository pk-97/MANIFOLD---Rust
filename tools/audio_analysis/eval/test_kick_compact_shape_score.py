"""Exact inherited shape order, causal prefixes and fold-isolated tree inputs."""
import unittest
from unittest.mock import patch

import numpy as np

from .kick_boosted_score import FoldFitter,predict_score
from .kick_compact_shape_score import CONFIGURATIONS,CompactShapeStream,configured_features,feature_names
from .kick_shape_features import FEATURE_NAMES,from_cached_features,shape_rows
from .kick_trajectory_features import fusion_features
from .kick_subspace_score import _training_data
from .test_kick_streaming_reference import stimulus


class CompactShapeTests(unittest.TestCase):
    def test_exact_inherited_order_and_prefixes(self):
        self.assertEqual(FEATURE_NAMES[15:18],('low_after_body','low_after_upper','body_after_upper'))
        self.assertEqual(FEATURE_NAMES[18:21],('low_body_shape_agreement','low_upper_shape_agreement','body_upper_shape_agreement'))
        self.assertEqual(FEATURE_NAMES[21:],('low_temporal_concentration','body_temporal_concentration','upper_temporal_concentration'))
        x=np.random.default_rng(4).normal(size=(20,39));expected=from_cached_features(x)
        for c,n in zip(CONFIGURATIONS,(18,21,24)):
            got=configured_features(x,c)
            np.testing.assert_array_equal(got,expected[:,:n]);np.testing.assert_array_equal(got[:,:15],x[:,:15])
            self.assertEqual(tuple(feature_names(c)),FEATURE_NAMES[:n])
            for i in range(len(x)):np.testing.assert_array_equal(got[i:i+1],configured_features(x[i:i+1],c))

    def test_signed_delays_and_time_reversal(self):
        power=np.ones((1,3,8));power[0,0,7]=9;power[0,1,0]=9
        shape=shape_rows(np.log(power))[0]
        np.testing.assert_allclose(shape[:3],[.5,.25,-.25],atol=1e-14)
        np.testing.assert_allclose(shape[6:],[.25,.25,0],atol=1e-14)
        reverse=shape_rows(np.log(power[:,:,::-1]))[0]
        np.testing.assert_allclose(shape[:3],-reverse[:3],atol=1e-14)
        np.testing.assert_allclose(shape[3:],reverse[3:],atol=1e-14)
        np.testing.assert_array_equal(shape_rows(np.zeros((1,3,8))),np.zeros((1,9)))

    def test_native_stream_shared_fft_and_startup_prefix(self):
        for sr in (44100,48000):
            audio=stimulus(sr);c,a,f,hop=fusion_features(audio,sr);expected=from_cached_features(f)
            stream=CompactShapeStream(sr);retained=stream.retained_array_bytes();rows=[]
            with patch('numpy.fft.rfft',wraps=np.fft.rfft) as transform:
                for i in range(len(audio)//hop):
                    row=stream.push_hop(audio[i*hop:(i+1)*hop])
                    if row:rows.append((row[0],row[1],row[2].copy()))
                self.assertEqual(transform.call_count,len(audio)//hop)
            self.assertEqual(retained,stream.retained_array_bytes())
            np.testing.assert_array_equal([r[0] for r in rows],c);np.testing.assert_array_equal([r[1] for r in rows],a)
            np.testing.assert_allclose([r[2] for r in rows],expected,atol=1e-11,rtol=1e-11)
            for cut in (9,12,20,90):
                stream=CompactShapeStream(sr);prefix=[]
                for i in range(cut):
                    row=stream.push_hop(audio[i*hop:(i+1)*hop])
                    if row:prefix.append(row[2].copy())
                np.testing.assert_array_equal(prefix,[r[2] for r in rows if r[1]<cut])

    def test_exact_fold_weights_excluded_perturbation_and_scalar_prediction(self):
        rng=np.random.default_rng(13);records=[]
        for name in ('a','b','outer','inner'):
            y=np.tile([0,1],24)
            records.append(dict(track=name,features=configured_features(rng.normal(size=(48,39)),'concentration24'),labels=y,
                event_ids=np.where(y,np.arange(48)//4,-1),training_mask=np.ones(48,bool)))
        fitter=FoldFitter();training=records[:2]+records[3:];m=fitter.fit(training,'inner',3)
        _,x,y,w=_training_data(training,'inner');np.testing.assert_array_equal(m['mean'],np.sum(x*w[:,None],axis=0))
        self.assertAlmostEqual(w[y==1].sum(),.5);self.assertAlmostEqual(w[y==0].sum(),.5)
        records[2]['features'][:]=np.nan;records[3]['features'][:]=np.nan
        self.assertEqual(m,fitter.fit(training,'inner',3));self.assertEqual(m['training_tracks'],['a','b'])
        got=predict_score(m,records[0]['features'])
        np.testing.assert_array_equal(got,[predict_score(m,row[None,:])[0] for row in records[0]['features']])

    def test_rejects_undeclared_configuration_and_bad_schema(self):
        with self.assertRaises(ValueError):configured_features(np.zeros((1,39)),'extra_trial')
        with self.assertRaises(ValueError):configured_features(np.zeros((1,24)),'delays18')
        with self.assertRaises(ValueError):configured_features(np.full((1,39),np.nan),'delays18')
        self.assertEqual(configured_features(np.zeros((0,39)),'concentration24').shape,(0,24))


if __name__=='__main__':unittest.main()
