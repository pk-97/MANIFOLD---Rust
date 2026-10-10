"""Bounded-state, hop-at-a-time reference for the unchanged fifteen DSP features.

Offline verification only. SciPy/NumPy calls allocate bounded temporary arrays;
this is not an allocation-free audio callback implementation or app integration.
"""
from __future__ import annotations

import math

import numpy as np
from scipy.signal import butter, lfilter, sosfilt
from scipy.special import expit

from .kick_fusion_features import EPSILON, FFT_SAMPLES


class FeatureStream:
    def __init__(self, sample_rate):
        if sample_rate <= 16000:
            raise ValueError('sample rate must exceed16000Hz')
        self.sample_rate = sample_rate
        self.hop = max(1, round(sample_rate*256/48000))
        self.horizon = math.ceil(.040*sample_rate/self.hop)
        self.span = math.floor(.015*sample_rate/self.hop)
        self.length = self.horizon+1
        bands = ((45,140), (140,400), (1000,2000), (2000,4000), (4000,8000))
        self.filters = [butter(2, band, btype='bandpass', fs=sample_rate, output='sos') for band in bands]
        self.filter_state = [np.zeros((len(sos), 2)) for sos in self.filters]
        self.alpha = [math.exp(-1/(tau*sample_rate)) for tau in (.003,.080)]
        self.envelope_state = np.zeros((5,2,1))
        self.envelopes = np.zeros((5,2))
        self.wave = np.zeros(FFT_SAMPLES)
        self.window = np.hanning(FFT_SAMPLES)
        frequencies = np.fft.rfftfreq(FFT_SAMPLES, 1/sample_rate)
        self.frequency_mask = (frequencies >= 30) & (frequencies <= 8000)
        frequencies = frequencies[self.frequency_mask]
        self.log_frequency = np.log2(frequencies)
        self.band_masks = [np.ones(len(frequencies), bool),
            (frequencies >=30)&(frequencies <140),
            (frequencies >=140)&(frequencies <1000),
            (frequencies >=1000)&(frequencies <=8000)]
        self.previous_spectrum = np.zeros(len(frequencies))
        # Six envelope summaries, then four (relative flux, centroid) pairs.
        self.history = np.zeros((self.length,14))
        self.ordered = np.zeros_like(self.history)
        self.edges = np.zeros(self.length, bool)
        self.previous_active = np.zeros(3, bool)
        self.output = np.zeros(15)
        self.index = -1

    def retained_array_bytes(self):
        arrays = [value for value in vars(self).values() if isinstance(value,np.ndarray)]
        arrays += self.filters+self.filter_state+self.band_masks
        return sum(a.nbytes for a in arrays)

    def push_hop(self, samples):
        """Consume exactly one completed hop; output is borrowed until next call."""
        if np.shape(samples) != (self.hop,) or not np.isfinite(samples).all():
            raise ValueError('one finite mono hop required')
        self.index += 1
        for band, sos in enumerate(self.filters):
            filtered, self.filter_state[band] = sosfilt(sos, samples, zi=self.filter_state[band])
            power = filtered*filtered
            for follower, alpha in enumerate(self.alpha):
                smoothed, state = lfilter([1-alpha], [1,-alpha], power,
                                         zi=self.envelope_state[band,follower])
                self.envelope_state[band,follower] = state
                self.envelopes[band,follower] = smoothed[-1]
        low, body = self.envelopes[:2]
        upper = self.envelopes[2:]
        upper_fast, upper_slow = upper.sum(axis=0)
        active = np.array([low[0]>1e-6 and low[0]>1.2*low[1],
            body[0]>1e-6 and body[0]/(body[1]+EPSILON)>2,
            upper_fast>1e-6 and np.count_nonzero(upper[:,0]/(upper[:,1]+EPSILON)>2)>=2])
        slot = self.index%self.length
        self.edges[slot] = np.any(active & ~self.previous_active)
        self.previous_active[:] = active
        self.wave[:-self.hop] = self.wave[self.hop:]
        self.wave[-self.hop:] = samples
        spectrum = np.abs(np.fft.rfft(self.wave*self.window))[self.frequency_mask]
        positive_difference = np.maximum(spectrum-self.previous_spectrum,0)
        row = self.history[slot]
        row[:6] = low[0],low[1],body[0],body[1],upper_fast,upper_slow
        for band, mask in enumerate(self.band_masks):
            magnitude = spectrum[mask].sum()
            row[6+2*band] = positive_difference[mask].sum()/(magnitude+EPSILON)
            row[7+2*band] = (spectrum[mask]*self.log_frequency[mask]).sum()/(magnitude+EPSILON)
        self.previous_spectrum[:] = spectrum
        candidate = self.index-self.horizon
        if candidate < 0 or not self.edges[candidate%self.length]:
            return None
        for j in range(self.length):
            self.ordered[j] = self.history[(candidate+j)%self.length]
        values = self.ordered
        first, last = values[:self.span+1],values[-self.span-1:]
        clip = lambda value: float(np.clip(value,-6,6))
        for j in range(3):
            self.output[j] = clip(np.max(np.log((values[:,2*j]+EPSILON)/(values[:,2*j+1]+EPSILON))))
        self.output[3] = np.clip(values[:,6].max(),0,1)
        self.output[4] = clip(first[:,7].mean()-last[:,7].mean())
        self.output[5] = clip(np.log((values[-1,4]+EPSILON)/(first[:,4].max()+EPSILON)))
        self.output[6] = clip(np.log((values[:,2].max()+EPSILON)/(values[:,0].max()+EPSILON)))
        self.output[7] = clip(np.log((last[:,0].mean()+EPSILON)/(first[:,0].mean()+EPSILON)))
        self.output[8] = np.clip(abs(np.argmax(values[:,0])-np.argmax(values[:,4]))
                                  *self.hop/self.sample_rate/.040,0,1)
        for band in range(3):
            self.output[9+2*band] = np.clip(values[:,8+2*band].max(),0,1)
            self.output[10+2*band] = clip(first[:,9+2*band].mean()-last[:,9+2*band].mean())
        return candidate,self.index,self.output


class DecisionStream:
    """Precompiled model constants and fixed scratch, with emitted-hop refractory."""
    def __init__(self, model, threshold, sample_rate, hop):
        self.threshold,self.sample_rate,self.hop = threshold,sample_rate,hop
        self.last = -math.inf
        self.mean,self.scale = np.asarray(model['mean']),np.asarray(model['scale'])
        self.z = np.zeros(len(self.mean))
        self.z32 = np.zeros(len(self.mean),np.float32)
        self.kind = 'tree' if 'trees' in model else 'linear'
        if self.kind == 'tree':
            self.initial,self.learning_rate = model['initial_log_odds'],model['learning_rate']
            self.max_depth = model['max_depth']
            self.trees = [tuple(np.asarray(t[k]) for k in ('left','right','feature','threshold','value'))
                          for t in model['trees']]
            if len(self.trees) !=64 or self.max_depth not in (1,2,3):
                raise ValueError('only the bounded experimental tree models are supported')
        else:
            self.weights,self.intercept = np.asarray(model['weights']),model['intercept']

    def score(self, features):
        np.subtract(features,self.mean,out=self.z)
        np.divide(self.z,self.scale,out=self.z)
        np.clip(self.z,-8,8,out=self.z)
        if self.kind == 'linear':
            return float(expit(self.z @ self.weights+self.intercept))
        self.z32[:] = self.z
        value = self.initial
        for left,right,feature,threshold,leaf in self.trees:
            node = 0
            for _ in range(self.max_depth):
                if left[node] == -1:
                    break
                node = left[node] if self.z32[feature[node]] <= threshold[node] else right[node]
            if left[node] != -1:
                raise ValueError('tree exceeds declared bound')
            value += self.learning_rate*leaf[node]
        return float(expit(value))

    def push(self, features, available_hop):
        score = self.score(features)
        fire = score >= self.threshold and (available_hop-self.last)*self.hop/self.sample_rate >= .060-1e-12
        if fire:
            self.last = available_hop
        return score,fire
