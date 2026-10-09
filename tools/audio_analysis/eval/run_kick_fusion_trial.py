"""One fixed, song-excluded linear DSP fusion experiment; no neural network.

Nine features, a 40ms evidence horizon, L2-regularised logistic score, cutoff0.5.
Scores use balanced training weights and are NOT calibrated event probabilities.
The held-out song contributes neither labels nor feature normalisation statistics.
All recordings have already informed development; this is exploratory cross-validation.
"""
from __future__ import annotations

import argparse
import json
from pathlib import Path
import time

import numpy as np
from scipy.optimize import minimize
from scipy.special import expit
from scipy.signal import butter, sosfilt

from .kick_attack_rejection import read_audio, sha
from .kick_excess_balance_trial import compact_score
from .kick_fusion_features import FEATURE_NAMES, fusion_features
from .kick_hybrid_experiment import controls
from .live_kick_baseline import score_events
from .run_kick_dsp_experiments import ROOT, development_sources, evaluate

REGULARISATION = .01
SCORE_THRESHOLD = .5


def fit_without(records, held_out):
    training = [r for r in records if r['track'] != held_out]
    if not training:
        raise ValueError('no training songs')
    arrays, labels, weights = [], [], []
    for r in training:
        selected = r['training_mask']
        x, y, ids = r['features'][selected], r['labels'][selected], r['event_ids'][selected]
        positive, negative = y == 1, y == 0
        if not positive.any() or not negative.any():
            raise ValueError(f'both training classes required: {r["track"]}')
        # Every song has equal influence; each annotated event shares equal
        # positive weight even if the broad proposer generates several candidates.
        w = np.zeros(len(y))
        events, counts = np.unique(ids[positive], return_counts=True)
        for event, count in zip(events, counts):
            w[positive & (ids == event)] = .5 / len(events) / count
        w[negative] = .5 / np.count_nonzero(negative)
        arrays.append(x); labels.append(y); weights.append(w/len(training))
    x, y, w = np.vstack(arrays), np.concatenate(labels), np.concatenate(weights)
    mean = np.sum(x*w[:, None], axis=0)
    scale = np.maximum(np.sqrt(np.sum((x-mean)**2*w[:, None], axis=0)), 1e-3)
    z = np.clip((x-mean)/scale, -8, 8)
    def loss(beta):
        logits = z @ beta[:-1] + beta[-1]
        value = np.sum(w*(np.logaddexp(0, logits)-y*logits))
        value += .5*REGULARISATION*np.dot(beta[:-1], beta[:-1])
        residual = w*(expit(logits)-y)
        gradient = np.r_[z.T @ residual+REGULARISATION*beta[:-1], residual.sum()]
        return value, gradient
    fit = minimize(loss, np.zeros(x.shape[1]+1), jac=True, method='L-BFGS-B',
                   options=dict(maxiter=150, ftol=1e-10))
    if not fit.success:
        raise RuntimeError(f'fusion fit failed: {fit.message}')
    return dict(training_tracks=[r['track'] for r in training], held_out=held_out,
        mean=mean.tolist(), scale=scale.tolist(), weights=fit.x[:-1].tolist(),
        intercept=float(fit.x[-1]), training_loss=float(fit.fun), iterations=int(fit.nit))


def predict_score(model, features):
    z = np.clip((features-np.array(model['mean']))/np.array(model['scale']), -8, 8)
    return expit(z @ np.array(model['weights'])+model['intercept'])


def select_fires(scores, available, sr, hop):
    last, fires = -np.inf, []
    for score, index in zip(scores, available):
        if score >= SCORE_THRESHOLD and (index-last)*hop/sr >= .060-1e-12:
            fires.append(int(index)); last = index
    return fires


def training_labels(source, ref, candidates, available, sr, hop, duration):
    starts, ends = (candidates+1)*hop/sr, (available+1)*hop/sr
    mask = np.zeros(len(candidates), bool)
    y = np.zeros(len(candidates), int)
    ids = np.full(len(candidates), -1, int)
    if source['group'] == 'original_five':
        passages = [dict(start_s=0, end_s=duration, kick_times_s=source['truth'],
                         excluded_regions=source['regions'])]
    else:
        passages = [dict(p, excluded_regions=s['excluded_regions'])
                    for p,s in zip(source['passages'],ref['scores']['v5'])]
    offset = 0
    for p in passages:
        selected = (starts >= p['start_s']) & (ends < p['end_s'])
        for region in p['excluded_regions']:
            selected &= ~((starts <= region['end_s']) & (ends >= region['start_s']))
        truth = np.array(p['kick_times_s'])
        if len(truth):
            distances = np.abs(starts[:,None]-truth[None,:])
            closest = np.argmin(distances, axis=1)
            positive = selected & (distances[np.arange(len(starts)), closest] <= .035+1e-12)
            y[positive] = 1
            ids[positive] = closest[positive]+offset
        mask |= selected
        offset += len(truth)
    return mask, y, ids


def control_scores(model):
    rows = []
    for sr in (44100,48000):
        cases = controls(sr)
        bass = next(audio for name,audio,_ in cases if name=='stationary_bass')
        rng = np.random.default_rng(14)
        n = round(.050*sr)
        burst = sosfilt(butter(2,[1000,8000],btype='bandpass',fs=sr,output='sos'),rng.normal(size=n))
        burst *= .25*np.exp(-np.arange(n)/sr/.008)
        hats = np.zeros_like(bass)
        for onset in [.5,1.,1.5,2.]:
            start=round(onset*sr);hats[start:start+n]+=burst
        cases.append(('hats_over_stationary_bass',bass+hats,[]))
        for name,audio,truth in cases:
            candidate,available,features,hop=fusion_features(audio,sr)
            fires=select_fires(predict_score(model,features),available,sr,hop)
            scores=score_events([(i+1)*hop/sr for i in fires],truth,
                                [dict(start_s=0,end_s=.3,reason='startup')])
            rows.append(dict(name=name,sample_rate=sr,scores=scores))
    return rows


def run(audio_root, baseline_path, out):
    baseline=json.loads(baseline_path.read_text())
    refs={t['track']:t for t in baseline['tracks']}
    sources=development_sources(audio_root)
    for t in json.loads((ROOT/'tests/fixtures/audio_labels/heldout_passages_2026-10-09.json').read_text())['tracks']:
        sources.append(dict(track=t['track'],group='former_heldout',audio_path=t['master_path'],
                            audio_sha256=t['audio_sha256'],passages=t['passages']))
    for name,digest in baseline['label_sha256'].items():
        if sha(ROOT/'tests/fixtures/audio_labels'/name)!=digest:
            raise ValueError('frozen labels changed')
    records=[]
    for source in sources:
        ref=refs[source['track']]
        if source['audio_sha256']!=ref['audio_sha256'] or sha(Path(source['audio_path']))!=ref['audio_sha256']:
            raise ValueError('audio differs from frozen reference')
        sr,audio=read_audio(Path(source['audio_path']))
        start=time.process_time()
        candidates,available,features,hop=fusion_features(audio,sr)
        cpu=time.process_time()-start
        mask,y,ids=training_labels(source,ref,candidates,available,sr,hop,len(audio)/sr)
        records.append(dict(track=source['track'],source=source,ref=ref,sample_rate=sr,hop=hop,
            candidates=candidates,available=available,features=features,training_mask=mask,
            labels=y,event_ids=ids,duration_s=len(audio)/sr,feature_cpu_s=cpu))
        print('features',source['track'],len(candidates),'training',int(mask.sum()),flush=True)
    tracks=[]
    for held in records:
        model=fit_without(records,held['track'])
        assert held['track'] not in model['training_tracks']
        start=time.process_time()
        scores=predict_score(model,held['features'])
        fires=select_fires(scores,held['available'],held['sample_rate'],held['hop'])
        decision_cpu=time.process_time()-start
        scored=evaluate(held['source'],[(i+1)*held['hop']/held['sample_rate'] for i in fires])
        ceilings=evaluate(held['source'],[(i+1)*held['hop']/held['sample_rate'] for i in held['available']])
        comparisons=[]
        for old,new in zip(held['ref']['scores']['v5'],scored):
            associated=lambda p:{x['attack_s'] for x in p['association_early_35_late_200_ms']['pairs']}
            a,b=associated(old),associated(new)
            comparisons.append(dict(passage=old['id'],lost_labels_s=sorted(a-b),recovered_labels_s=sorted(b-a)))
        row=dict(track=held['track'],audio_sha256=held['source']['audio_sha256'],
            sample_rate=held['sample_rate'],hop=held['hop'],model=model,kick_hops=fires,
            baseline_scores=held['ref']['scores']['v5'],scores=scored,comparisons=comparisons,
            candidate_ceiling=[dict(id=p['id'],labels=p['labels'],matched_50ms=p['accuracy_by_tolerance_ms']['50']['matched'],
                                   matched_70ms=p['accuracy_by_tolerance_ms']['70']['matched']) for p in ceilings],
            candidates=len(held['candidates']),training_samples=int(held['training_mask'].sum()),
            training_positive=int(held['labels'][held['training_mask']].sum()),
            processing_cpu_s_per_audio_s=(held['feature_cpu_s']+decision_cpu)/held['duration_s'])
        tracks.append(row)
        print('SCORE',held['track'],[compact_score(p) for p in scored],flush=True)
    totals={name:{metric:{k:sum(compact_score(p)[metric][k] for t in tracks for p in t[name])
        for k in ('matched','missed','extra')} for metric in ('strict_50ms','association')}
        for name in ('baseline_scores','scores')}
    synthetic=control_scores(next(t['model'] for t in tracks if t['track']=='heavy_on_mind'))
    files=('run_kick_fusion_trial.py','kick_fusion_features.py','kick_upper_cue.py',
           'kick_attack_rejection.py','kick_tonal_experiment.py','kick_hybrid_experiment.py',
           'run_kick_dsp_experiments.py','kick_excess_balance_trial.py',
           'live_kick_baseline.py','master_kick_comparison.py')
    report=dict(method=__doc__,feature_names=list(FEATURE_NAMES),
        training='Each song excluded in turn from fitting and standardisation. Equal '
        'song and class weight; positive candidates within35ms of a label share equal '
        'weight per labelled event. Other candidates in reviewed, non-uncertain cores '
        'are negative. L2=.01; training-weighted mean/std, std floor.001, z clipped8; '
        'fixed score cutoff.5,60ms refractory at actual40ms-horizon availability.',
        acceptance='More associated hits without losing original associated labels or '
        'increasing unmatched overall or any reviewed kick-free passage. Report '
        '35/50/70ms scores and actual delays; wide association is diagnostic only.',
        totals=totals,tracks=tracks,controls=synthetic,
        controls_model='Fold trained on all songs except Heavy On Mind; unchanged cutoff.',
        source_sha256={f:sha(ROOT/'tools/audio_analysis/eval'/f) for f in files},
        label_sha256=baseline['label_sha256'],baseline_sha256=sha(baseline_path),
        limitations='Provisional labels and nine previously explored songs; no untouched '
        'external validation. Candidate recall limits classifier recall. Balanced '
        'logistic score is not calibrated confidence. Timing includes fixed evidence '
        'wait, never backdated. Python CPU timing is offline throughput, not a native '
        'callback guarantee. No production integration.')
    out.parent.mkdir(parents=True,exist_ok=True)
    out.write_text(json.dumps(report,indent=2)+'\n')
    print('TOTALS',totals)
    print('CONTROLS',[(c['name'],c['sample_rate'],compact_score(c['scores'])) for c in synthetic])
    return report


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--audio-root',type=Path,required=True)
    parser.add_argument('--baseline',type=Path,required=True)
    parser.add_argument('--out',type=Path,required=True)
    args=parser.parse_args()
    run(args.audio_root,args.baseline,args.out)


if __name__=='__main__':main()
