"""H16: frozen classical RBF margins with bounded training CPU and JSON inference."""
from __future__ import annotations

import copy
import hashlib
import json
import multiprocessing
from pathlib import Path
import signal
import time

import numpy as np
from scipy.spatial.distance import cdist
from scipy.special import expit
import sklearn
from sklearn.svm import SVC

from .kick_boosted_score import training_key
from .kick_subspace_score import _training_data

GAMMAS = (1/60,1/30,1/15)
FIT_CPU_LIMIT = 30.
TOTAL_CPU_LIMIT = 240.


def parameters(gamma):
    if isinstance(gamma,bool) or gamma not in GAMMAS:
        raise ValueError('gamma was not predeclared')
    return dict(kernel='rbf',C=100.,gamma=gamma,probability=False,shrinking=True,
                tol=1e-5,cache_size=128,max_iter=1000000)


def frozen_rule():
    return dict(hypothesis='H16',gammas=list(GAMMAS),parameters=[parameters(g) for g in GAMMAS],
        features='Unchanged15 measurements; exact H10 accepted73 training coverage. Original381 evaluation objects/calibration passages unchanged; noD2 training.',
        normalization='Fold-only original song/class/event weighted mean and scale>=1e-3; z clipped8. Original sample weights sum1 without rescaling.',
        score='expit(sum(dual_i*exp(-gamma*||z-support_i||²))+intercept); not a calibrated probability.',
        calibration='Unchanged nested whole-song coarse plus single-bracket refinement,40ms rounded candidate horizon and60ms refractory.',
        cpu=dict(per_fit_seconds=FIT_CPU_LIMIT,total_protected_training_seconds=TOTAL_CPU_LIMIT,
            enforcement='A single spawned child uses default-fatal ITIMER_PROF; parent waits. Timer covers fit, convergence validation and export parity. A killed attempt saves no model; any limit stops the experiment with partial feasibility evidence.'),
        inference='Bounded32-row chunks use squared Euclidean distance; scalar runtime reuses per-support-vector scratch. Support count, memory and CPU reported separately from native real-time guarantees.',
        expected_failure='Local kernel similarity may overfit song/timbre, confuse bass and percussion, or require too many support vectors; no missing acoustic information is invented.',
        prohibition='Exactly three gammas; no probability fitting, extra tuning, neural/GPU work, reserved material or app integration.')


class TrainingBudgetExceeded(RuntimeError):
    pass


def _timed_child(sender,function,args,limit):
    # Default OS action terminates a long C fit even while Python cannot run handlers.
    signal.signal(signal.SIGPROF,signal.SIG_DFL)
    started=time.process_time();signal.setitimer(signal.ITIMER_PROF,limit)
    try:
        result=function(*args)
        elapsed=time.process_time()-started
        signal.setitimer(signal.ITIMER_PROF,0)
        sender.send(('ok',result,elapsed))
    except BaseException as error:
        signal.setitimer(signal.ITIMER_PROF,0)
        sender.send(('error',repr(error),time.process_time()-started))
    finally:
        sender.close()


def cpu_limited(function,args,limit):
    if limit<=0:raise TrainingBudgetExceeded('total training CPU budget exhausted')
    context=multiprocessing.get_context('spawn');receiver,sender=context.Pipe(duplex=False)
    process=context.Process(target=_timed_child,args=(sender,function,args,limit))
    process.start();sender.close()
    try:
        payload=receiver.recv()
    except EOFError:
        payload=None
    finally:
        receiver.close();process.join()
    if payload is None and process.exitcode==-signal.SIGPROF:
        raise TrainingBudgetExceeded(f'protected training reached {limit:.6f}CPU seconds')
    if process.exitcode or payload is None:
        raise RuntimeError(f'training child failed with exit{process.exitcode}')
    if payload[0]!='ok':raise RuntimeError(payload[1])
    return payload[1],payload[2]


def standardise(model,features):
    x=np.asarray(features,dtype=np.float64);mean=np.asarray(model['mean']);scale=np.asarray(model['scale'])
    if x.ndim!=2 or x.shape[1]!=15 or mean.shape!=(15,) or scale.shape!=(15,) or not np.isfinite(x).all() or not np.isfinite(mean).all() or not np.isfinite(scale).all() or np.any(scale<=0):
        raise ValueError('fifteen finite features and valid model scale required')
    with np.errstate(over='ignore'):return np.clip((x-mean)/scale,-8,8)


def decision_values(model,features):
    z=standardise(model,features);support=np.asarray(model['support_vectors']);dual=np.asarray(model['dual_coefficients']);out=np.empty(len(z))
    if support.ndim!=2 or support.shape[1]!=15 or dual.shape!=(len(support),):raise ValueError('invalid exported support vectors')
    for start in range(0,len(z),32):
        kernel=cdist(z[start:start+32],support,'sqeuclidean')
        kernel*=-model['gamma'];np.exp(kernel,out=kernel)
        out[start:start+32]=kernel@dual+model['intercept']
    return out


def predict_score(model,features):
    return expit(decision_values(model,features))


class KernelRuntime:
    """Compiled constants and O(support-count) scalar scratch; no event policy."""
    def __init__(self,model):
        self.mean=np.asarray(model['mean']);self.scale=np.asarray(model['scale'])
        self.support=np.asarray(model['support_vectors']);self.dual=np.asarray(model['dual_coefficients'])
        self.gamma=model['gamma'];self.intercept=model['intercept'];self.z=np.zeros(15)
        self.distance=np.zeros(len(self.support));self.scratch=np.zeros(len(self.support))

    def retained_array_bytes(self):
        return sum(x.nbytes for x in vars(self).values() if isinstance(x,np.ndarray))

    def score(self,features):
        if np.shape(features)!=(15,) or not np.isfinite(features).all():raise ValueError('finite15 row required')
        np.subtract(features,self.mean,out=self.z);np.divide(self.z,self.scale,out=self.z);np.clip(self.z,-8,8,out=self.z)
        self.distance[:]=0
        for j in range(15):
            np.subtract(self.support[:,j],self.z[j],out=self.scratch);np.square(self.scratch,out=self.scratch);self.distance+=self.scratch
        self.distance*=-self.gamma;np.exp(self.distance,out=self.distance)
        return float(expit(self.distance@self.dual+self.intercept))


def _fit_export(x,y,w,mean,scale,config):
    z=np.clip((x-mean)/scale,-8,8);started=time.process_time()
    classifier=SVC(**config).fit(z,y,sample_weight=w);fit_cpu=time.process_time()-started
    if classifier.fit_status_!=0 or not np.array_equal(classifier.classes_,[0,1]):raise ValueError('SVC did not converge as binary0/1')
    model=dict(format_version=1,mean=mean.tolist(),scale=scale.tolist(),gamma=config['gamma'],
        support_vectors=classifier.support_vectors_.tolist(),dual_coefficients=classifier.dual_coef_[0].tolist(),
        intercept=float(classifier.intercept_[0]),support_counts=classifier.n_support_.tolist(),iterations=classifier.n_iter_.tolist(),
        fit_status=int(classifier.fit_status_),fit_cpu_s=fit_cpu,score_kind='RBF margin through sigmoid; not calibrated probability')
    model=json.loads(json.dumps(model));indices=np.unique(np.linspace(0,len(x)-1,min(256,len(x)),dtype=int))
    expected=classifier.decision_function(z[indices]);actual=decision_values(model,x[indices]);error=float(abs(actual-expected).max())
    runtime=KernelRuntime(model);scalar=np.array([runtime.score(row) for row in x[indices]])
    scalar_error=float(abs(scalar-expit(expected)).max())
    if error>1e-9 or scalar_error>1e-10:raise ValueError('exported RBF predictions differ')
    model.update(export_margin_max_abs_error=error,scalar_score_max_abs_error=scalar_error,export_probe_count=len(indices))
    return model


class FoldFitter:
    def __init__(self,cache):
        self.cache=Path(cache);self.cache.mkdir(parents=True,exist_ok=True)
        self.models={};self.fits=self.memory_hits=self.disk_hits=0;self.protected_cpu_s=0.;self.attempts=[]
        self.sources={n:hashlib.sha256((Path(__file__).parent/n).read_bytes()).hexdigest() for n in ('kick_kernel_score.py','kick_subspace_score.py')}

    def fit(self,records,held_out,gamma):
        if len({r['track'] for r in records})!=len(records):raise ValueError('unique training families required')
        config=parameters(gamma);training,x,y,w=_training_data(records,held_out)
        if x.shape[1]!=15:raise ValueError('only unchanged15 features allowed')
        keys=[list(training_key(r)) for r in training]
        signature=dict(training_input_keys=keys,parameters=config,source_sha256=self.sources,sklearn_version=sklearn.__version__)
        key=hashlib.sha256(json.dumps(signature,sort_keys=True).encode()).hexdigest();path=self.cache/f'{key}.json'
        if key in self.models:self.memory_hits+=1
        elif path.exists():
            stored=json.loads(path.read_text());assert stored['signature']==signature
            assert stored['model_sha256']==hashlib.sha256(json.dumps(stored['model'],sort_keys=True).encode()).hexdigest()
            self.models[key]=stored['model'];self.disk_hits+=1
        else:
            mean=np.sum(x*w[:,None],axis=0);scale=np.maximum(np.sqrt(np.sum((x-mean)**2*w[:,None],axis=0)),1e-3)
            limit=min(FIT_CPU_LIMIT,TOTAL_CPU_LIMIT-self.protected_cpu_s)
            attempt=dict(training_input_sha256=key,training_tracks=[r['track'] for r in training],gamma=gamma,samples=len(y),cpu_limit=limit)
            try:model,elapsed=cpu_limited(_fit_export,(x,y,w,mean,scale,config),limit)
            except TrainingBudgetExceeded as error:
                self.protected_cpu_s+=max(limit,0);attempt.update(status='cpu_limit_no_model',reason=str(error));self.attempts.append(attempt);raise
            self.protected_cpu_s+=elapsed;attempt.update(status='converged',protected_cpu_s=elapsed,fit_cpu_s=model['fit_cpu_s']);self.attempts.append(attempt)
            model.update(training_tracks=attempt['training_tracks'],training_input_keys=keys,training_input_sha256=key,parameters=config,
                training_samples=len(y),training_weight_mass=float(w.sum()),positive_weight_mass=float(w[y==1].sum()),negative_weight_mass=float(w[y==0].sum()),protected_training_cpu_s=elapsed)
            self.models[key]=model;self.fits+=1
            digest=hashlib.sha256(json.dumps(model,sort_keys=True).encode()).hexdigest()
            path.write_text(json.dumps(dict(signature=signature,model=model,model_sha256=digest),separators=(',',':'))+'\n')
        result=copy.deepcopy(self.models[key]);result['held_out']=held_out;return result

    def statistics(self):
        return dict(fits=self.fits,memory_hits=self.memory_hits,disk_hits=self.disk_hits,unique_models=len(self.models),protected_training_cpu_s=self.protected_cpu_s,
            fit_cpu_s=sum(m['fit_cpu_s'] for m in self.models.values()),attempts=self.attempts)
