"""Run fixed causal DSP experiments on seven development recordings only.

Python CPU measurements are exploratory throughput, not native callback or app
latency guarantees. Actual event time is always the completed input hop.
"""
from __future__ import annotations

import argparse
import csv
import hashlib
import importlib
import json
from pathlib import Path
import time

import numpy as np

from .kick_attack_rejection import causal_features, read_audio
from .kick_dsp_experiments import detect_temporal, detect_v5
from .live_kick_baseline import exclusion_regions, score_events, delay_stats, sha
from .master_kick_comparison import score_passage

ROOT = Path(__file__).resolve().parents[3]
LABELS = ROOT / 'tests/fixtures/audio_labels'
SCORES = ROOT / 'tools/audio_analysis/eval/scoreboard'


def development_sources(audio_root):
    old = json.loads((SCORES / 'kick_attack_trial_2026-10-09.json').read_text())
    review = list(csv.DictReader((LABELS / 'attack_review.csv').open()))
    sources = []
    for track in old['tracks']:
        name = track['track']
        truth = [float(r['mix_time_s']) for r in csv.DictReader((LABELS / f'{name}.csv').open())]
        assert sha(LABELS / f'{name}.csv') == track['labels_sha256']
        sources.append(dict(track=name, group='original_five',
            audio_path=str(audio_root / name / 'mix.wav'), audio_sha256=track['audio_sha256'],
            native_hops=track['kick_hops'], truth=truth,
            regions=exclusion_regions([r for r in review if r['track']==name],track['duration_s'])))
    masters = json.loads((SCORES / 'master_kick_runs_2026-10-09.json').read_text())
    labels = json.loads((LABELS / 'master_passages_2026-10-09.json').read_text())
    for track in labels['tracks']:
        run = next(r for r in masters if r['track']==track['track'] and r['detector']=='kick_attack_probe')
        sources.append(dict(track=track['track'],group='new_masters',audio_path=run['audio_path'],
            audio_sha256=track['audio_sha256'],native_hops=run['kick_hops'],passages=track['passages']))
    return sources


def evaluate(source, times):
    if source['group']=='original_five':
        return [dict(id=source['track'], **score_events(times,source['truth'],source['regions']))]
    return [dict(id=p['id'], **score_passage(times,p)) for p in source['passages']]


def aggregate(tracks):
    results = {}
    for group in ('original_five','new_masters'):
        passages = [p for t in tracks if t['group']==group for p in t['scores']]
        pairs = [p for s in passages for p in s['association_early_35_late_200_ms']['pairs']]
        results[group] = dict(labels=sum(p['labels'] for p in passages),
            tolerance_ms={str(ms):{key:sum(p['accuracy_by_tolerance_ms'][str(ms)][key] for p in passages)
                                  for key in ('matched','missed','extra')} for ms in (35,50,70)},
            association_delay=delay_stats([p['delay_ms'] for p in pairs]))
    return results


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument('--variant',choices=('baseline','temporal','tonal','pcen'),required=True)
    ap.add_argument('--cache',type=Path,required=True)
    ap.add_argument('--audio-root',type=Path,required=True)
    args = ap.parse_args()
    args.cache.mkdir(parents=True,exist_ok=True)
    if args.variant in ('tonal','pcen'):
        module = importlib.import_module(f'eval.kick_{args.variant}_experiment')
        detector = module.detect
    elif args.variant=='temporal':
        detector = detect_temporal
    else:
        detector = lambda samples,sr,env,hop: detect_v5(env,sr,hop)
    tracks=[]
    for source in development_sources(args.audio_root):
        path=Path(source['audio_path'])
        if sha(path)!=source['audio_sha256']:
            raise ValueError(f'{path}: audio hash changed')
        sr,samples=read_audio(path)
        feature_key=hashlib.sha256((source['audio_sha256']+sha(ROOT/'tools/audio_analysis/eval/kick_attack_rejection.py')).encode()).hexdigest()
        feature_file=args.cache/f'{feature_key}.npz'
        if feature_file.exists():
            with np.load(feature_file) as z:
                env=z['env'];hop=int(z['hop']);feature_cpu=float(z['feature_cpu_s'])
        else:
            start=time.process_time();env,hop=causal_features(samples,sr)
            feature_cpu=time.process_time()-start
            np.savez(feature_file,env=env,hop=hop,feature_cpu_s=feature_cpu)
        # Every branch must agree with the frozen native baseline before comparing.
        if detect_v5(env,sr,hop)!=source['native_hops']:
            raise ValueError(f'{source["track"]}: native v5 replay mismatch')
        start=time.process_time();fires=detector(samples,sr,env,hop)
        detector_cpu=time.process_time()-start
        if fires!=sorted(set(fires)) or not all(0<=i<len(env) for i in fires):
            raise ValueError('invalid detector event indices')
        times=[(i+1)*hop/sr for i in fires]
        row=dict(track=source['track'],group=source['group'],audio_sha256=source['audio_sha256'],
                 sample_rate=sr,hop=hop,duration_s=len(samples)/sr,native_baseline_replay_exact=True,
                 feature_cpu_s=feature_cpu,detector_cpu_s=detector_cpu,
                 python_cpu_seconds_per_audio_second=(feature_cpu+detector_cpu)/(len(samples)/sr),
                 kick_hops=fires,scores=evaluate(source,times))
        tracks.append(row)
        print(args.variant,source['track'],[{k:s['accuracy_by_tolerance_ms']['50'][k] for k in ('matched','missed','extra')} for s in row['scores']],flush=True)
    files=['kick_dsp_experiments.py','kick_attack_rejection.py','run_kick_dsp_experiments.py','live_kick_baseline.py','master_kick_comparison.py']
    if args.variant in ('pcen','tonal'): files.append(f'kick_{args.variant}_experiment.py')
    result=dict(variant=args.variant,method='Fixed settings, CPU-only causal research implementation; full chronological audio. Visual provisional labels; held-out tracks excluded. Event availability is unshifted hop end. Python process CPU throughput includes feature extraction but excludes decoding, scoring and native baseline verification; not a native real-time callback benchmark.',
                sources_sha256={f:sha(ROOT/'tools/audio_analysis/eval'/f) for f in files},
                labels_sha256={p.name:sha(p) for p in LABELS.glob('*.csv')},
                master_labels_sha256=sha(LABELS/'master_passages_2026-10-09.json'),
                totals=aggregate(tracks),tracks=tracks)
    (args.cache/f'{args.variant}.json').write_text(json.dumps(result,indent=2)+'\n')
    print(json.dumps(result['totals'],indent=2))

if __name__=='__main__': main()
