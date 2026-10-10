"""Fixed algebra, shared FFT, startup causality and fold-isolation proofs."""
import unittest
from unittest.mock import patch

import numpy as np

from .kick_boosted_score import FoldFitter, predict_score
from .kick_fusion_bandwise import _bandwise_features, fusion_features
from .kick_fusion_features import _spectra
from .kick_spectral_shape import (CONFIGURATIONS, SpectralShapeStream, SpectralStream,
    band_masks, configured_features, descriptors, feature_names, frequency_grid, shape_rows)
from .kick_subspace_score import _training_data
from .test_kick_streaming_reference import stimulus


class SpectralShapeTests(unittest.TestCase):
    def test_silence_uniform_sparse_and_one_bin_bands(self):
        f=frequency_grid(48000);masks=band_masks(f)
        np.testing.assert_array_equal(descriptors(np.zeros(len(f)),f),np.zeros(6))
        flat=descriptors(np.ones(len(f)),f)
        np.testing.assert_allclose(flat[2:5],0,atol=1e-14)
        for band,mask in enumerate(masks):
            q=np.zeros(len(f));q[np.flatnonzero(mask)[0]]=1.
            got=descriptors(q,f);self.assertEqual(got[band+2],1.)
            self.assertAlmostEqual(got[5],0.)
        np.testing.assert_allclose(descriptors([2.,3.,5.],[70.,300.,2000.])[:5],[.2,.3,1,1,1],atol=1e-15)

    def test_gain_scaling_away_from_silence_epsilon(self):
        rng=np.random.default_rng(1);f=frequency_grid(44100);q=rng.uniform(.01,1,len(f))
        for scale in (.01,3.,100.):
            np.testing.assert_allclose(descriptors(q*scale,f),descriptors(q,f),atol=1e-14,rtol=0)
        mag=rng.uniform(.1,1.,(30,len(f)));c=np.array([0,1,16]);a=c+8
        np.testing.assert_allclose(shape_rows(mag*3,c,a,44100,235),shape_rows(mag,c,a,44100,235),atol=1e-13,rtol=0)

    def test_equal_old_flux_and_centroid_changes_can_have_different_shape(self):
        f=frequency_grid(48000);narrow=np.zeros(len(f));broad=np.ones(len(f))
        # Equal per-band magnitude sums also equalise the old epsilon denominators.
        for mask in band_masks(f):
            indices=np.flatnonzero(mask);narrow[indices[len(indices)//2]]=len(indices)
        gain=np.linspace(1,2,20)[:,None];c=np.array([4]);a=np.array([12])
        first,second=gain*narrow,gain*broad
        np.testing.assert_allclose(_bandwise_features(first,48000,c,a,256),_bandwise_features(second,48000,c,a,256),atol=1e-12,rtol=0)
        self.assertFalse(np.allclose(shape_rows(first,c,a,48000,256),shape_rows(second,c,a,48000,256)))

    def test_frozen_prior_partial_startup_and_constant_spectrum(self):
        f=frequency_grid(48000);mag=np.full((40,len(f)),2.);c=np.array([0,1,14,15,16]);a=c+8
        result=shape_rows(mag,c,a,48000,256)
        np.testing.assert_array_equal(result[0,:6],result[0,6:])
        np.testing.assert_array_equal(result[1:,6:],0.)
        mag[1:10]=3.
        actual=shape_rows(mag,np.array([1]),np.array([9]),48000,256)[0]
        np.testing.assert_allclose(actual[6:],descriptors(np.full(len(f),5.),f),atol=0,rtol=0)
        # A moving tone creates residual shape too; it is not isolated kick energy.
        mag[:]=0.;mag[:16,4]=1.;mag[16:,5]=1.
        shifted=shape_rows(mag,np.array([16]),np.array([24]),48000,256)[0]
        self.assertTrue(np.any(shifted[6:]!=0))

    def test_ring_wrap_and_prefix_match_batch_at_both_rates(self):
        rng=np.random.default_rng(4)
        for sr,hop in ((44100,235),(48000,256)):
            mag=rng.uniform(0,2,(120,len(frequency_grid(sr))));c=np.arange(0,110);a=c+8
            expected=shape_rows(mag,c,a,sr,hop);stream=SpectralStream(sr,hop);before=stream.retained_array_bytes();got=[]
            for i,row in enumerate(mag):
                value=stream.push(row,i-8 if 8<=i<118 else None)
                if value is not None:got.append(value.copy())
            np.testing.assert_array_equal(got,expected)
            self.assertEqual(stream.retained_array_bytes(),before)
            self.assertIn(stream.length,(24,25))
            cut=19;keep=a<cut
            np.testing.assert_array_equal(shape_rows(mag[:cut],c[keep],a[keep],sr,hop),expected[keep])
            changed=mag.copy();changed[cut:]=999.
            np.testing.assert_array_equal(shape_rows(changed,c[keep],a[keep],sr,hop),expected[keep])

    def test_native_shared_single_fft_and_startup_prefix(self):
        for sr in (44100,48000):
            audio=stimulus(sr);c,a,base,hop=fusion_features(audio,sr)
            extra=shape_rows(_spectra(audio,sr,hop),c,a,sr,hop);stream=SpectralShapeStream(sr);rows=[]
            with patch('numpy.fft.rfft',wraps=np.fft.rfft) as transform:
                for i in range(len(audio)//hop):
                    row=stream.push_hop(audio[i*hop:(i+1)*hop])
                    if row:rows.append((row[0],row[1],row[2].copy()))
                self.assertEqual(transform.call_count,len(audio)//hop)
            np.testing.assert_array_equal([r[0] for r in rows],c);np.testing.assert_array_equal([r[1] for r in rows],a)
            np.testing.assert_allclose([r[2] for r in rows],np.column_stack((base,extra)),atol=1e-11,rtol=1e-11)
            for cut in (9,12,20):
                prefix=SpectralShapeStream(sr);got=[]
                for i in range(cut):
                    row=prefix.push_hop(audio[i*hop:(i+1)*hop])
                    if row:got.append(row[2].copy())
                np.testing.assert_array_equal(got,[r[2] for r in rows if r[1]<cut])

    def test_configurations_and_exact_fold_hashes(self):
        rng=np.random.default_rng(19);records=[]
        for name in ('a','b','outer','inner'):
            base=rng.normal(size=(40,15));extra=rng.uniform(size=(40,12));y=np.tile([0,1],20)
            records.append(dict(track=name,features=configured_features(base,extra,'raw_residual12'),labels=y,
                event_ids=np.where(y,np.arange(40)//4,-1),training_mask=np.ones(40,bool)))
        for config,n in zip(CONFIGURATIONS,(21,21,27)):
            self.assertEqual(configured_features(base,extra,config).shape,(40,n));self.assertEqual(len(feature_names(config)),n)
        fitter=FoldFitter();training=records[:2]+records[3:];model=fitter.fit(training,'inner',3)
        _,x,y,w=_training_data(training,'inner')
        np.testing.assert_array_equal(model['mean'],np.sum(x*w[:,None],axis=0));self.assertAlmostEqual(w[y==1].sum(),.5)
        records[2]['features'][:]=np.nan;records[3]['features'][:]=np.nan
        self.assertEqual(model,fitter.fit(training,'inner',3));self.assertEqual(model['training_tracks'],['a','b'])
        got=predict_score(model,records[0]['features'])
        np.testing.assert_array_equal(got,[predict_score(model,r[None,:])[0] for r in records[0]['features']])

    def test_invalid_input(self):
        for q in ([np.nan],[np.inf],[-1.]):
            with self.assertRaises(ValueError):descriptors(q,[100.])
        with self.assertRaises(ValueError):configured_features(np.zeros((2,15)),np.zeros((2,12)),'extra_trial')
        with self.assertRaises(ValueError):SpectralStream(48000,256).push(np.zeros(len(frequency_grid(48000))),0)


if __name__=='__main__':unittest.main()
