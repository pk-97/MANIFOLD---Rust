"""Audit one causal complex-domain measurement; do not change detector rules.

Primary evidence is normalized complex error over 45-2000 Hz, with a trailing
20 ms maximum at actual prototype firing times. Missed-event evidence uses
label-guided windows ending at +20/+35/+50 ms: an oracle diagnostic, not a new
trigger or measured recall improvement. All settings are shared across tracks.
"""
from __future__ import annotations

import argparse
import json
from pathlib import Path
import time

import numpy as np

from .kick_attack_rejection import read_audio, causal_features
from .kick_complex_onset import complex_features
from .kick_dsp_experiments import detect_v5
from .kick_hybrid_experiment import controls
from .kick_tonal_experiment import _fft_size
from .live_kick_baseline import sha
from .run_kick_dsp_experiments import development_sources, evaluate

ROOT = Path(__file__).resolve().parents[3]
RECENT_SECONDS = .020
BUDGETS_MS = (20,35,50)


def describe(values):
    x = np.asarray(values,dtype=float)
    if not len(x):
        return dict(count=0)
    return dict(count=len(x),min=float(x.min()),p10=float(np.quantile(x,.1)),
                median=float(np.median(x)),p90=float(np.quantile(x,.9)),max=float(x.max()))


def auc(positive,negative):
    """Descriptive pairwise ranking, no threshold fitting or independence claim."""
    if not len(positive) or not len(negative):
        return None
    delta=np.asarray(positive)[:,None]-np.asarray(negative)[None,:]
    return float(np.mean((delta>0)+.5*(delta==0)))


def window_peak(values,grid,start,end):
    lo,hi=np.searchsorted(grid,[start-1e-10,end+1e-10])
    if lo>=hi:
        raise ValueError('window has no completed hops')
    index=int(lo+np.argmax(values[lo:hi]))
    return dict(value=float(values[index]),available_s=float(grid[index]))


def fire_evidence(values,grid,fire):
    index=int(np.argmin(np.abs(grid-fire)))
    if abs(grid[index]-fire)>1e-8:
        raise ValueError('fire must be an actual hop end')
    return dict(at_fire=float(values[index]),recent_peak=window_peak(values,grid,fire-RECENT_SECONDS,fire))


def firing_summary(events):
    positives=[e['recent_peak']['value'] for e in events if e['kind']=='hit_50ms' and e.get('full_prediction_history',True)]
    negatives=[e['recent_peak']['value'] for e in events if e['kind']=='unmatched_200ms']
    late=[e['recent_peak']['value'] for e in events if e['kind']=='late_associated']
    ghosts=[e['recent_peak']['value'] for e in events if e['kind']=='hybrid_kick_free']
    # Optimistic diagnostic bound only: even with labels, a lower-threshold veto
    # preserving every current tight hit can remove only scores below this min.
    bound=min(positives) if positives else None
    return dict(hits_with_full_history=describe(positives),
        warmup_hits=sum(e['kind']=='hit_50ms' and not e.get('full_prediction_history',True) for e in events),
        unmatched=describe(negatives),late=describe(late),
        hybrid_kick_free=describe(ghosts),hit_vs_unmatched_auc=auc(positives,negatives),
        hit_vs_hybrid_kick_free_auc=auc(positives,ghosts),
        optimistic_preserve_all_hits_bound=dict(min_hit_score=bound,
            unmatched_below_min=sum(v<bound for v in negatives) if bound is not None else None,
            hybrid_ghosts_below_min=sum(v<bound for v in ghosts) if bound is not None else None))


def run_controls():
    rows=[]
    for sr in (44100,48000):
        hop=round(sr*256/48000)
        for name,samples,onsets in controls(sr):
            values=complex_features(samples,sr,hop)['normalized']
            grid=(np.arange(len(values))+1)*hop/sr
            rows.append(dict(signal=name,sample_rate=sr,settled=describe(values[grid>=.3]),
                onset_peaks_50ms=[window_peak(values,grid,t,t+.05) for t in onsets]))
    return rows


def main():
    ap=argparse.ArgumentParser(description=__doc__)
    ap.add_argument('--audio-root',required=True,type=Path)
    ap.add_argument('--hybrid-report',required=True,type=Path)
    ap.add_argument('--out',required=True,type=Path)
    args=ap.parse_args()
    args.out.parent.mkdir(parents=True,exist_ok=True)
    hybrid=json.loads(args.hybrid_report.read_text())
    report=dict(method=__doc__,controls=run_controls(),tracks=[])
    for source in development_sources(args.audio_root):
        path=Path(source['audio_path'])
        if sha(path)!=source['audio_sha256']:
            raise ValueError('audio differs from reviewed source')
        sr,samples=read_audio(path)
        env,hop=causal_features(samples,sr)
        if detect_v5(env,sr,hop)!=source['native_hops']:
            raise ValueError('native v5 replay mismatch')
        start=time.process_time()
        features=complex_features(samples,sr,hop)
        cpu=time.process_time()-start
        values=features['normalized']
        grid=(np.arange(len(values))+1)*hop/sr
        np.savez(args.out.parent/(source['track']+'.npz'),**features,hop=hop,sample_rate=sr)
        scores=evaluate(source,[(i+1)*hop/sr for i in source['native_hops']])
        events,labels,background=[],[],[]
        for score in scores:
            strict=score['accuracy_by_tolerance_ms']['50']
            pairs=score['association_early_35_late_200_ms']['pairs']
            paired={p['available_s'] for p in pairs}
            strict_extras=set(strict['extra_times_s'])
            for p in pairs:
                events.append(dict(kind='late_associated' if p['available_s'] in strict_extras else 'hit_50ms',
                    time_s=p['available_s'],label_s=p['attack_s'],passage=score['id'],
                    **fire_evidence(values,grid,p['available_s'])))
            for fire in sorted(strict_extras-paired):
                events.append(dict(kind='unmatched_200ms',time_s=fire,passage=score['id'],
                    **fire_evidence(values,grid,fire)))
            truth=sorted(set(strict['missed_times_s'])|{p['attack_s'] for p in pairs})
            assert len(truth)==score['labels']
            for t in truth:
                labels.append(dict(time_s=t,kind='missed_50ms' if t in strict['missed_times_s'] else 'caught',
                    passage=score['id'],evidence={str(ms):window_peak(values,grid,t,t+ms/1000) for ms in BUDGETS_MS}))
        if source['group']=='new_masters':
            previous=next(t for t in hybrid['variants']['rms_presence']['tracks'] if t['track']==source['track'])
            assert previous['audio_sha256']==source['audio_sha256']
            for passage in source['passages']:
                if passage['kick_times_s']:
                    continue
                for fire in [(i+1)*hop/sr for i in previous['kick_hops']]:
                    if passage['start_s']<=fire<passage['end_s']:
                        events.append(dict(kind='hybrid_kick_free',time_s=fire,passage=passage['id'],
                            **fire_evidence(values,grid,fire)))
                count=int(np.floor((passage['end_s']-passage['start_s'])/.05+1e-8))
                for i in range(count):
                    t=passage['start_s']+i*.05
                    background.append(dict(start_s=t,passage=passage['id'],
                        evidence={str(ms):window_peak(values,grid,t,t+ms/1000) for ms in BUDGETS_MS}))
        warmup_end=(_fft_size(sr)+2*hop)/sr
        for event in events:
            event['full_prediction_history']=event['time_s']>=warmup_end-1e-10
        for label in labels:
            label['full_prediction_history_at_label']=label['time_s']>=warmup_end-1e-10
            label['endpoint_shift_sensitivity']={str(ms):[
                window_peak(values,grid,max(0,label['time_s']+shift),label['time_s']+shift+ms/1000)['value']
                for shift in (-.020,0,.020)] for ms in BUDGETS_MS
                if label['time_s']>=.020}
        track=dict(track=source['track'],audio_path=str(path),audio_sha256=source['audio_sha256'],
            native_v5_replay_exact=True,sample_rate=sr,hop=hop,feature_cpu_s=cpu,
            feature_cpu_per_audio_second=cpu/(len(samples)/sr),events=events,labels=labels,
            background=background,firing_summary=firing_summary(events))
        report['tracks'].append(track)
        print(source['track'],json.dumps(track['firing_summary']),flush=True)
    events=[e for t in report['tracks'] for e in t['events']]
    report['firing_summary']=firing_summary(events)
    report['label_summary']={}
    for ms in map(str,BUDGETS_MS):
        groups={kind:[l['evidence'][ms]['value'] for t in report['tracks'] for l in t['labels'] if l['kind']==kind]
                for kind in ('caught','missed_50ms')}
        groups['kick_free']=[b['evidence'][ms]['value'] for t in report['tracks'] for b in t['background']]
        report['label_summary'][ms]=dict(distributions={k:describe(v) for k,v in groups.items()},
            missed_vs_kick_free_auc=auc(groups['missed_50ms'],groups['kick_free']))
    report['sources_sha256']={name:sha(Path(__file__).parent/name) for name in (
        'run_kick_complex_audit.py','kick_complex_onset.py','kick_tonal_experiment.py','kick_attack_rejection.py',
        'kick_dsp_experiments.py','kick_hybrid_experiment.py','run_kick_dsp_experiments.py',
        'live_kick_baseline.py','master_kick_comparison.py')}
    report['labels_sha256']={p.name:sha(p) for p in (ROOT/'tests/fixtures/audio_labels').glob('*') if p.suffix in ('.csv','.json')}
    report['hybrid_report_sha256']=sha(args.hybrid_report)
    report['limitations']=[
        'Development tracks and provisional visual labels; no held-out or independent listening validation.',
        'Primary range 45-2000 Hz, 2048-at-48k trailing Hann window, normalized complex error; not the unnormalized full-spectrum paper pipeline.',
        'No lookahead or deliberate wait; trailing Hann still weights older samples and can spread/delay evidence. Actual hop availability is retained.',
        'Pairwise AUC and label-preserving bounds are descriptive, not classifier accuracy or generalisation estimates. Per-track bounds are not proposed song-specific settings.',
            'Label-guided peak windows are oracle diagnostics, not online onset localization. Peaks may belong to other sounds or offsets.',
            'Positive label windows also have -20/0/+20 ms alignment sensitivity; shifted windows are not evidence of a fixed latency guarantee. Startup hits remain baseline hits but are separate from full-history feature comparisons.',
        'Baseline unmatched events exclude wider late associations but do not establish source identity. Hybrid kick-free events are secondary negatives and may overlap baseline events; never add their counts.',
        'CPU measurement covers Python batch feature extraction only, not native callback deadlines or full app latency.',
        'No detector mutation, threshold selection or beat/template model. Miracle and Heavy On Mind remain untouched.']
    args.out.write_text(json.dumps(report,indent=2)+'\n')


if __name__=='__main__':main()
