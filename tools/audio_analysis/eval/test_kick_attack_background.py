"""Focused algebra, history, native-grid and fold-isolation checks for H8."""
import copy
import unittest

import numpy as np

from .kick_attack_background import (AttackBackgroundStream, EnvelopeStream,
    background_rows, configured_features, feature_names, CONFIGURATIONS)
from .kick_boosted_score import FoldFitter, predict_score
from .kick_subspace_score import _training_data
from .kick_trajectory_features import fusion_features, trajectory_rows
from .test_kick_streaming_reference import stimulus


class BackgroundTests(unittest.TestCase):
    def test_zero_and_partial_history_have_defined_values(self):
        fast = np.zeros((20, 3))
        np.testing.assert_array_equal(background_rows(fast, np.array([0, 1]), np.array([8, 9]), 48000, 256), 0.)
        fast[:] = np.arange(20)[:, None]
        rows = background_rows(fast, np.array([0, 2]), np.array([8, 10]), 48000, 256)
        np.testing.assert_array_equal(rows[0, :3], 6.)
        np.testing.assert_array_equal(rows[0, 3:], 0.)
        np.testing.assert_allclose(rows[1, :3], np.log1p((10-.5)/(.5+1e-12)), rtol=0, atol=1e-12)
        np.testing.assert_allclose(rows[1, 3:], np.log1p(.5/(.5+1e-12)), rtol=0, atol=1e-12)

    def test_additive_constant_power_preserves_novelty_not_source_separation(self):
        rng = np.random.default_rng(11);fast = rng.uniform(.1, 1., (40, 3));fast[20:29] += .8
        c, a = np.array([20]), np.array([28])
        base = background_rows(fast, c, a, 48000, 256)
        shifted = background_rows(fast+12.3, c, a, 48000, 256)
        np.testing.assert_allclose(base[:, :3], shifted[:, :3], atol=1e-12, rtol=0)
        self.assertTrue(np.all(shifted[:, 3:] < base[:, 3:]))
        # The invariance is not claimed for addition of two waveforms.
        x, background = np.array([1., -1.]), np.array([1., 1.])
        self.assertFalse(np.array_equal((x+background)**2, x*x+background*background))

    def test_prior_modulation_adds_information_absent_from_cached_trajectory(self):
        stable = np.ones((40, 3));variable = stable.copy()
        variable[5:20] = np.linspace(.2, 1.8, 15)[:, None]
        stable[20:29] = variable[20:29] = 2.
        c, a = np.array([20]), np.array([28]);slow = np.ones_like(stable)
        np.testing.assert_array_equal(trajectory_rows(stable, slow, c, a), trajectory_rows(variable, slow, c, a))
        a1, a2 = [background_rows(v, c, a, 48000, 256) for v in (stable, variable)]
        self.assertTrue(np.all(a1[:, :3] > a2[:, :3]))
        self.assertTrue(np.all(a1[:, 3:] < a2[:, 3:]))

    def test_repeated_attacks_and_ring_wrap_match_batch_and_prefix(self):
        # Repeated 125ms-spaced power pulses atop amplitude-modulated background.
        n = 420;t = np.arange(n)*256/48000
        fast = np.tile((1+.3*np.sin(2*np.pi*5*t))[:, None], (1, 3))
        for c0 in range(0, n, 23):
            age = np.arange(n-c0)*256/48000
            fast[c0:] += .5*np.exp(-age[:, None]/.03)*np.array([1, 2, .3])
        slow = np.full_like(fast, 1.)
        c = np.arange(0, n-8, 7);a = c+8
        expected = np.column_stack((trajectory_rows(fast, slow, c, a), background_rows(fast, c, a, 48000, 256)))
        stream = EnvelopeStream(48000, 256);before = stream.retained_array_bytes();got=[];row=0
        for i in range(n):
            value = stream.push(fast[i], slow[i], int(c[row]) if row < len(c) and a[row] == i else None)
            if value is not None:
                got.append(value.copy());row += 1
        np.testing.assert_array_equal(got, expected)
        self.assertEqual(before, stream.retained_array_bytes())
        self.assertEqual(stream.length, 24)
        self.assertLess(before, 3000)
        cut=181;keep=a<cut
        np.testing.assert_array_equal(background_rows(fast[:cut], c[keep], a[keep], 48000, 256), expected[keep, 24:])
        changed=fast.copy();changed[cut:]=1000
        np.testing.assert_array_equal(background_rows(changed, c[keep], a[keep], 48000, 256), expected[keep, 24:])

    def test_native_audio_composition_and_prefix_at_both_rates(self):
        for sr in (44100, 48000):
            samples=stimulus(sr);c,a,base,hop=fusion_features(samples,sr)
            stream=AttackBackgroundStream(sr);rows=[];initial=stream.retained_array_bytes()
            for i in range(len(samples)//hop):
                result=stream.push_hop(samples[i*hop:(i+1)*hop])
                if result:rows.append((result[0],result[1],result[2].copy()))
            np.testing.assert_array_equal([r[0] for r in rows],c)
            np.testing.assert_array_equal([r[1] for r in rows],a)
            np.testing.assert_allclose(np.array([r[2][:39] for r in rows]),base,atol=1e-11,rtol=1e-11)
            self.assertEqual(initial,stream.retained_array_bytes())
            self.assertLess(initial,50000)
            cut=len(samples)//hop//2;prefix=AttackBackgroundStream(sr);got=[]
            for i in range(cut):
                r=prefix.push_hop(samples[i*hop:(i+1)*hop])
                if r:got.append(r[2].copy())
            np.testing.assert_array_equal(got,[r[2] for r in rows if r[1]<cut])

    def test_configuration_columns_and_fold_isolation(self):
        rng=np.random.default_rng(9);records=[]
        for name in ('a','b','outer','inner'):
            base=rng.normal(size=(32,39));extra=rng.uniform(size=(32,6));y=np.tile([0,1],16)
            records.append(dict(track=name,features=configured_features(base,extra,'tree39_background6'),
                labels=y,event_ids=np.where(y,np.arange(32)//4,-1),training_mask=np.ones(32,bool)))
        for config,dimension in zip(CONFIGURATIONS,(39,21,45)):
            self.assertEqual(configured_features(base,extra,config).shape,(32,dimension))
            self.assertEqual(len(feature_names(config)),dimension)
        fitter=FoldFitter();training=[r for r in records if r['track']!='outer']
        model=fitter.fit(training,'inner',3)
        _,x,y,w=_training_data(training,'inner')
        np.testing.assert_allclose(model['mean'],(x*w[:,None]).sum(axis=0),atol=0,rtol=0)
        self.assertAlmostEqual(w[y==1].sum(),.5)
        records[-2]['features'][:]=np.nan;records[-1]['features'][:]=np.nan
        self.assertEqual(model,fitter.fit(training,'inner',3))
        self.assertEqual(model['training_tracks'],['a','b'])
        scores=predict_score(model,records[0]['features'])
        np.testing.assert_array_equal(scores,[predict_score(model,r[None,:])[0] for r in records[0]['features']])

    def test_invalid_input_is_rejected(self):
        for fast in (np.ones((10,2)),np.full((10,3),np.nan),np.full((10,3),-1.)):
            with self.assertRaises(ValueError):background_rows(fast,np.array([0]),np.array([8]),48000,256)
        with self.assertRaises(ValueError):configured_features(np.zeros((2,39)),np.zeros((2,6)),'new_config')
        with self.assertRaises(ValueError):EnvelopeStream(48000,256).push([1,1,1],[1,1,1],0)


if __name__=='__main__':
    unittest.main()
