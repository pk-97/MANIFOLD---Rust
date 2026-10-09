"""H11: conditional raw/residual spectral shape, never isolated kick energy."""
from __future__ import annotations

import hashlib
import json
import math
from pathlib import Path
import time

import numpy as np

from .kick_attack_rejection import read_audio, sha
from .kick_fusion_bandwise import BANDS, FEATURE_NAMES as BASE_NAMES, _bandwise_features
from .kick_fusion_features import EPSILON, FFT_SAMPLES, _spectra
from .kick_streaming_reference import FeatureStream

BACKGROUND_S = .080
CONFIGURATIONS = ('raw6', 'residual6', 'raw_residual12')
DESCRIPTOR_NAMES = ('low_power_share', 'body_power_share', 'low_concentration',
                    'body_concentration', 'upper_concentration', 'log_frequency_width')
DEPENDENCIES = ('kick_spectral_shape.py', 'kick_fusion_features.py',
                'kick_fusion_bandwise.py', 'kick_streaming_reference.py',
                'kick_tonal_experiment.py', 'kick_attack_rejection.py')


def frozen_rule():
    return dict(hypothesis='H11', configurations=list(CONFIGURATIONS),
        model=dict(n_estimators=64, learning_rate=.05, max_depth=3,
                   min_weight_fraction_leaf=.005, random_state=0, subsample=1., n_iter_no_change=None),
        base='Unchanged cached15 features and depth3 scorer; dimensions21/21/27.',
        spectra='Exactly existing trailing2048-sample np.hanning FFT;30–8000Hz native bins and existing hop. Runtime reuses FeatureStream.previous_spectrum, one FFT per hop.',
        prior='B[k]=mean(|S[t,k]|²) over preceding ceil(.080*sample_rate/hop) completed hops t<c; partial startup and zero prior when c=0.',
        raw='Qraw[k]=mean(|S[t,k]|²), t=c..deadline inclusive.',
        residual='Qres[k]=mean(max(|S[t,k]|²-B[k],0)), t=c..deadline inclusive; prior frozen for the candidate.',
        descriptors=list(DESCRIPTOR_NAMES),
        shares='Power in30–140 and140–1000Hz divided by full30–8000Hz power; final upper band1000–8000Hz inclusive.',
        concentration='Within each existing low/body/upper band normalize p=Q/sum(Q); clip((n*sum(p²)-1)/(n-1),0,1). One-bin positive band gives1; empty/silent band gives0.',
        width='Std(log2(frequency)) under globally normalized Q, divided by log2(8000/30), clipped0..1.',
        silence='Total/band power <=1e-12 gives zero descriptors/concentration. No pseudopower added to normalization.',
        caveat='Residual shape describes changed spectral energy, not isolated kick energy. Waveform phase cross terms, frequency motion and prior-window contamination remain.',
        preserved='Same381 labels,174+207 cohorts, candidate grid,42.6ms rounded evidence horizon,60ms refractory, song/class/event weights, nested whole-song exclusions, fold normalization/z clipping8 and existing coarse-plus-one-refinement calibration.',
        rationale='Current15/39 retain flux and centroid changes but not static within-band concentration, bandwidth or these spectral power shares. Prior tonal/NNLS hard gates failed; these are soft conditional descriptors under the established depth3 model.',
        prohibition='Exactly3 configurations and12 new scalar measurements; no templates, oracle cutoffs, extra features/configurations,73-label training, reserved data, neural/GPU work or integration.')


def frequency_grid(sample_rate):
    if sample_rate <= 16000:
        raise ValueError('sample rate must exceed16000Hz')
    f=np.fft.rfftfreq(FFT_SAMPLES,1/sample_rate)
    return f[(f>=30)&(f<=8000)]


def band_masks(frequency):
    return tuple((frequency>=lo)&((frequency<=hi) if i==2 else (frequency<hi))
                 for i,(lo,hi) in enumerate(BANDS))


def _descriptors(power, frequency, masks):
    result=np.zeros(6);total=float(power.sum())
    if total<=EPSILON:return result
    for i,mask in enumerate(masks):
        band=power[mask];mass=float(band.sum())
        if i<2:result[i]=mass/total
        if mass>EPSILON:
            p=band/mass;n=len(band)
            result[i+2]=1. if n==1 else np.clip((n*float(p@p)-1)/(n-1),0,1)
    p=power/total;logf=np.log2(frequency);centre=float(p@logf)
    result[5]=np.clip(np.sqrt(float(p@((logf-centre)**2)))/np.log2(8000/30),0,1)
    return result


def descriptors(power, frequency):
    power,frequency=np.asarray(power,dtype=float),np.asarray(frequency,dtype=float)
    if (power.ndim!=1 or frequency.shape!=power.shape or not np.isfinite(power).all()
            or np.any(power<0) or not np.isfinite(frequency).all()
            or np.any(frequency<30) or np.any(frequency>8000)):
        raise ValueError('matching finite nonnegative powers and30–8000Hz bins required')
    return _descriptors(power,frequency,band_masks(frequency))


def shape_rows(magnitudes,candidates,available,sample_rate,hop):
    magnitude=np.asarray(magnitudes,dtype=float);f=frequency_grid(sample_rate);masks=band_masks(f)
    c,a=np.asarray(candidates),np.asarray(available)
    if (magnitude.ndim!=2 or magnitude.shape[1]!=len(f) or not np.isfinite(magnitude).all()
            or np.any(magnitude<0) or hop<=0):raise ValueError('invalid native spectral grid')
    if (c.ndim!=1 or a.shape!=c.shape or not np.issubdtype(c.dtype,np.integer)
            or not np.issubdtype(a.dtype,np.integer) or np.any(c<0) or np.any(a<=c)
            or np.any(a>=len(magnitude))):raise ValueError('invalid candidate deadlines')
    power=magnitude*magnitude;history=math.ceil(BACKGROUND_S*sample_rate/hop);rows=np.zeros((len(c),12))
    for i,(start,end) in enumerate(zip(c,a)):
        prior=power[max(0,start-history):start]
        background=prior.mean(axis=0) if len(prior) else np.zeros(len(f))
        observed=power[start:end+1]
        rows[i,:6]=_descriptors(observed.mean(axis=0),f,masks)
        rows[i,6:]=_descriptors(np.maximum(observed-background,0).mean(axis=0),f,masks)
    return rows


class SpectralStream:
    """Bounded native power spectra; borrowed12-value output."""
    def __init__(self,sample_rate,hop):
        if hop<=0:raise ValueError('positive hop required')
        self.frequency=frequency_grid(sample_rate);self.masks=band_masks(self.frequency)
        self.horizon=math.ceil(.040*sample_rate/hop)
        self.history=math.ceil(BACKGROUND_S*sample_rate/hop)
        self.length=self.history+self.horizon+1
        self.ring=np.zeros((self.length,len(self.frequency)));self.ordered=np.zeros_like(self.ring)
        self.output=np.zeros(12);self.index=-1

    def retained_array_bytes(self):
        return sum(a.nbytes for a in (self.frequency,self.ring,self.ordered,self.output,*self.masks))

    def push(self,magnitude,candidate=None):
        magnitude=np.asarray(magnitude)
        if magnitude.shape!=self.frequency.shape or not np.isfinite(magnitude).all() or np.any(magnitude<0):
            raise ValueError('finite nonnegative native magnitude row required')
        self.index+=1;self.ring[self.index%self.length]=magnitude*magnitude
        if candidate is None:return None
        if candidate<0 or candidate!=self.index-self.horizon:raise ValueError('candidate deadline differs')
        start=max(0,candidate-self.history);count=self.index-start+1
        for j in range(count):self.ordered[j]=self.ring[(start+j)%self.length]
        before=candidate-start;prior=self.ordered[:before];observed=self.ordered[before:count]
        background=prior.mean(axis=0) if before else np.zeros(len(self.frequency))
        self.output[:6]=_descriptors(observed.mean(axis=0),self.frequency,self.masks)
        self.output[6:]=_descriptors(np.maximum(observed-background,0).mean(axis=0),self.frequency,self.masks)
        return self.output


class SpectralShapeStream:
    """Reuses the original stream's completed FFT, without a second transform."""
    def __init__(self,sample_rate):
        self.base=FeatureStream(sample_rate);self.hop=self.base.hop
        self.spectral=SpectralStream(sample_rate,self.hop);self.output=np.zeros(27)

    def retained_array_bytes(self):
        return self.base.retained_array_bytes()+self.spectral.retained_array_bytes()+self.output.nbytes

    def push_hop(self,samples):
        row=self.base.push_hop(samples)
        features=self.spectral.push(self.base.previous_spectrum,row[0] if row else None)
        if row is None:return None
        c,a,base=row;self.output[:15]=base;self.output[15:]=features
        return c,a,self.output


def configured_features(base,extra,configuration):
    if configuration not in CONFIGURATIONS:raise ValueError('configuration was not predeclared')
    if np.shape(base)[1:]!=(15,) or np.shape(extra)!=(len(base),12):raise ValueError('base15/extra12 rows must align')
    selected=extra[:,:6] if configuration=='raw6' else extra[:,6:] if configuration=='residual6' else extra
    return np.column_stack((base,selected))


def feature_names(configuration):
    if configuration not in CONFIGURATIONS:raise ValueError('configuration was not predeclared')
    kinds=('raw',) if configuration=='raw6' else ('residual',) if configuration=='residual6' else ('raw','residual')
    return BASE_NAMES+tuple(f'{kind}_{name}' for kind in kinds for name in DESCRIPTOR_NAMES)


def cached_shape(record,cache):
    cache=Path(cache);cache.mkdir(parents=True,exist_ok=True);directory=Path(__file__).parent
    signature=dict(rule=frozen_rule(),source_sha256={p:sha(directory/p) for p in DEPENDENCIES},original_cache=record['cache_metadata'])
    key=hashlib.sha256(json.dumps(signature,sort_keys=True).encode()).hexdigest();data_path,meta_path=cache/f'{key}.npz',cache/f'{key}.json'
    if meta_path.exists():
        meta=json.loads(meta_path.read_text())
        if meta['signature']!=signature or sha(data_path)!=meta['data_sha256']:raise ValueError('shape cache provenance differs')
        with np.load(data_path,allow_pickle=False) as z:extra=z['extras']
        return extra,meta
    if data_path.exists():raise ValueError('incomplete shape cache must be inspected')
    path=Path(record['source']['audio_path'])
    if sha(path)!=record['source']['audio_sha256']:raise ValueError('audio differs from frozen candidate source')
    sr,samples=read_audio(path);hop=record['hop'];c,a=record['candidates'],record['available']
    if sr!=record['sample_rate']:raise ValueError('sample rate differs')
    started=time.process_time();spectrum=_spectra(samples,sr,hop);fft_cpu=time.process_time()-started;del samples
    old=_bandwise_features(spectrum,sr,c,a,hop);error=float(abs(old-record['features'][:,9:15]).max())
    if error>1e-12:raise ValueError('existing six spectral features differ on FFT grid')
    started=time.process_time();extra=shape_rows(spectrum,c,a,sr,hop);batch_cpu=time.process_time()-started
    stream=SpectralStream(sr,hop);row=0;actual=[];started=time.process_time()
    for i,mag in enumerate(spectrum):
        candidate=int(c[row]) if row<len(c) and int(a[row])==i else None
        result=stream.push(mag,candidate)
        if result is not None:actual.append(result.copy());row+=1
    stream_cpu=time.process_time()-started;stream_error=float(abs(np.asarray(actual)-extra).max())
    if row!=len(c) or stream_error>1e-12:raise ValueError('full chronological spectral replay differs')
    np.savez(data_path,extras=extra)
    meta=dict(signature=signature,data_path=str(data_path),data_sha256=sha(data_path),track=record['track'],
        candidates=len(c),spectral_hops=len(spectrum),candidate_grid_unchanged=True,
        existing_six_spectral_max_abs_error=error,spectral_stream_max_abs_error=stream_error,
        spectral_retained_array_bytes=stream.retained_array_bytes(),fft_extraction_cpu_s=fft_cpu,
        shape_batch_cpu_s=batch_cpu,spectral_stream_cpu_s=stream_cpu,
        spectral_stream_cpu_fraction=stream_cpu/record['cache_metadata']['duration_s'],
        limitation='Full spectral-stage CPU excludes audio decode and original DSP/FFT. Bounded arrays and Python throughput do not establish allocation-free native callback timing.')
    meta_path.write_text(json.dumps(meta,indent=2)+'\n');return extra,meta
