"""H17 fixed covered-linear/kernel logits with hashed model and prediction caches."""
from __future__ import annotations

import copy
import hashlib
import json
from pathlib import Path

import numpy as np
from scipy.special import expit

from .kick_boosted_score import training_key
from .kick_coverage_blend import _models,component_signature
from .kick_kernel_score import decision_values,parameters,standardise

KERNEL_WEIGHTS=(.25,.5,.75)


def sha(path):return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def frozen_rule():
    return dict(hypothesis='H17',configurations=[dict(kernel_logit_weight=w,linear_logit_weight=1-w) for w in KERNEL_WEIGHTS],
        components='Frozen H16 gamma1/15 and H10 coverage_linear15, exact same73-coverage training sets; no refits.',
        score='expit((1-w)*raw_linear_logit+w*raw_kernel_margin); no inverse sigmoid or probability fitting.',
        protocol='Existing H14 nested whole-family exclusions and coarse-plus-one-refinement cutoff rules, original381/174+207 evaluation,9negative cores, unchanged candidates/42.6ms horizon/60ms refractory.',
        cache='Reports contain hashed kernel model references; component raw predictions keyed exact model/feature/source digests, reused across the3 weights.',
        scoring_cpu_limit_seconds=120,guard='One active CPU child, default-fatal ITIMER_PROF. A limit preserves partial report and prediction caches; no truncated result is accepted.',
        acceptance='223hits with<=70extras OR250with<=89extras;zero9cores;<=1previously caught baseline label lost pertrack.',
        prohibition='Exactly3 weights, no refitting, extra cutoff search, new DSP/labels, Pattern, D2, reserved material or app integration.',
        additional73='Known development labels used in training coverage within other-family folds; scored only after original381 report freezes, never untouched validation.')


class FrozenFitter:
    def __init__(self,linear_variant,kernel_variant,model_root):
        self.linear=_models(linear_variant);kernels=_models(kernel_variant);self.references={};self.requests=0
        if self.linear.keys()!=kernels.keys():raise ValueError('component families differ')
        for names,linear in self.linear.items():
            kernel=kernels[names]
            for key in ('training_input_keys','mean','scale'):
                if linear[key]!=kernel[key]:raise ValueError('component provenance/normalisation differs')
            if len(linear['weights'])!=15 or kernel['parameters']!=parameters(1/15) or kernel['fit_status']!=0:raise ValueError('wrong frozen components')
            path=Path(model_root)/f"{kernel['training_input_sha256']}.json"
            self.references[names]=dict(path=str(path),file_sha256=sha(path),
                training_input_sha256=kernel['training_input_sha256'],training_tracks=list(names),
                training_input_keys=copy.deepcopy(kernel['training_input_keys']),mean=kernel['mean'].copy(),scale=kernel['scale'].copy(),gamma=1/15)

    def fit(self,records,held_out,weight):
        if isinstance(weight,(bool,np.bool_)) or weight not in KERNEL_WEIGHTS:raise ValueError('only frozen .25/.5/.75 weights')
        if len({r['track'] for r in records})!=len(records):raise ValueError('unique families required')
        training=[r for r in records if r['track']!=held_out];names=tuple(r['track'] for r in training)
        if names not in self.linear:raise ValueError('no cached ordered training set')
        linear=self.linear[names];keys=[list(training_key(r)) for r in training]
        if keys!=linear['training_input_keys']:raise ValueError('current training provenance differs')
        self.requests+=1;component=copy.deepcopy(linear);component['held_out']=held_out
        return dict(training_tracks=list(names),held_out=held_out,training_input_keys=keys,mean=linear['mean'].copy(),scale=linear['scale'].copy(),
            kernel_logit_weight=weight,linear_logit_weight=1-weight,linear=component,kernel_reference=copy.deepcopy(self.references[names]),
            component_sha256=dict(linear=component_signature(component),kernel_file=self.references[names]['file_sha256']),score_kind='fixed linear/kernel raw-logit blend; not probability')


class PredictionCache:
    def __init__(self,cache=None):
        self.cache=Path(cache) if cache is not None else None
        if self.cache:self.cache.mkdir(parents=True,exist_ok=True)
        self.rows={};self.compiled={};self.computed=self.memory_hits=self.disk_hits=0
        self.sources={n:sha(Path(__file__).parent/n) for n in ('kick_kernel_blend.py','kick_kernel_score.py')}

    def kernel(self,reference):
        key=reference['file_sha256']
        if key not in self.compiled:
            path=Path(reference['path'])
            if sha(path)!=key:raise ValueError('kernel model file checksum differs')
            entry=json.loads(path.read_text());model=entry['model']
            if hashlib.sha256(json.dumps(model,sort_keys=True).encode()).hexdigest()!=entry['model_sha256']:raise ValueError('kernel model payload checksum differs')
            for name in ('training_input_sha256','training_tracks','training_input_keys','mean','scale','gamma'):
                if model[name]!=reference[name]:raise ValueError('kernel reference provenance differs')
            for name in ('support_vectors','dual_coefficients'):model[name]=np.asarray(model[name])
            self.compiled[key]=model
        return self.compiled[key]

    def logits(self,model,features):
        x=np.asarray(features,dtype=np.float64);reference=model['kernel_reference'];linear=model['linear']
        for key in ('training_tracks','training_input_keys','mean','scale'):
            if linear[key]!=model[key] or reference[key]!=model[key]:raise ValueError('component/root provenance differs')
        if component_signature(linear)!=model['component_sha256']['linear'] or reference['file_sha256']!=model['component_sha256']['kernel_file']:raise ValueError('component signature differs')
        digest=hashlib.sha256();digest.update(str((x.shape,x.dtype.str)).encode());digest.update(x.tobytes())
        signature=dict(component_sha256=model['component_sha256'],features_sha256=digest.hexdigest(),sources=self.sources)
        key=hashlib.sha256(json.dumps(signature,sort_keys=True).encode()).hexdigest()
        data=self.cache/f'{key}.npz' if self.cache else None;meta=self.cache/f'{key}.json' if self.cache else None
        if key in self.rows:self.memory_hits+=1
        elif meta is not None and meta.exists():
            receipt=json.loads(meta.read_text())
            if receipt['signature']!=signature or sha(data)!=receipt['data_sha256']:raise ValueError('prediction cache differs')
            with np.load(data,allow_pickle=False) as z:self.rows[key]=(z['linear'],z['kernel'])
            self.disk_hits+=1
        else:
            if data is not None and data.exists():raise ValueError('incomplete prediction cache requires inspection')
            z=standardise(linear,x);a=z@np.asarray(linear['weights'])+linear['intercept'];b=decision_values(self.kernel(reference),x)
            if not np.isfinite(a).all() or not np.isfinite(b).all():raise ValueError('finite component logits required')
            self.rows[key]=(a,b);self.computed+=1
            if data is not None:
                np.savez(data,linear=a,kernel=b);meta.write_text(json.dumps(dict(signature=signature,data_sha256=sha(data)))+'\n')
        return self.rows[key]

    def __call__(self,model,features):
        w=model['kernel_logit_weight']
        if not np.isfinite(w) or not 0<=w<=1 or model['linear_logit_weight']!=1-w:raise ValueError('invalid complementary weights')
        a,b=self.logits(model,features);return expit((1-w)*a+w*b)

    def statistics(self):return dict(computed_component_pairs=self.computed,memory_hits=self.memory_hits,disk_hits=self.disk_hits,loaded_kernel_models=len(self.compiled))


def predict_score(model,features):
    return PredictionCache()(model,features)
