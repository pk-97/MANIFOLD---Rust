"""Hybrid DSP development trials: raw attack evidence plus stable low-band power.

Rules were tried sequentially: PCEN failed synthetic controls, attack-led rise
confirmation lost rapid kicks, then presence-only confirmation isolated that
requirement. These are development results, not independent validation.
"""
from __future__ import annotations

import argparse
import importlib
import json
from pathlib import Path
import time
import numpy as np

from .kick_attack_rejection import read_audio, causal_features
from .kick_dsp_experiments import detect_v5
from .kick_pcen_experiment import pcen_features
from .live_kick_baseline import score_events, sha
from .run_kick_dsp_experiments import development_sources, evaluate, aggregate

ROOT = Path(__file__).resolve().parents[3]
FAST_ATTACK_RATIO = 1.5


def detect_from_features(raw, stable, sr, hop, require_attack=True):
    features = pcen_features(stable, sr, hop)
    eligible = features['eligible_mask'].copy()
    if require_attack:
        eligible &= raw[:, 1, 0] / (raw[:, 1, 1] + 1e-12) > FAST_ATTACK_RATIO
    # Stable low-frequency confirmation; retain fast body/mid measurements.
    confirmation = raw.copy()
    confirmation[:, 0, :] = stable[:, 0, :]
    return detect_v5(confirmation, sr, hop, eligible_mask=eligible,
                     rearm_mask=features['rearm_mask'])


def detect_attack_led(raw, stable, sr, hop, require_low_rise=True):
    # Keep the v5 fast body attack and balance criteria. Remove only the
    # simultaneous combined-power jump; stable low power confirms the event.
    low, body = raw[:, 0, :], raw[:, 1, :]
    eligible = ((low[:, 0] + body[:, 0] > 1e-6)
                & (body[:, 0] / (body[:, 1] + 1e-12) > 2.0)
                & (body[:, 0] > low[:, 0] / 3.0))
    confirmation = raw.copy()
    confirmation[:, 0, :] = stable[:, 0, :]
    return detect_v5(confirmation, sr, hop, eligible_mask=eligible,
                     require_low_rise=require_low_rise)


def variants(audio, sr, raw, hop, rule='presence'):
    rows = {}
    for method in ('rms', 'quadrature'):
        module = importlib.import_module(f'eval.kick_stable_{method}')
        start = time.process_time()
        stable = module.stable_features(audio, sr, raw, hop)
        feature_cpu = time.process_time() - start
        start = time.process_time()
        if rule == 'pcen':
            for require_attack, suffix in ((False, 'measurement_only'), (True, 'hybrid')):
                start = time.process_time()
                fires = detect_from_features(raw, stable, sr, hop, require_attack)
                rows[method + '_' + suffix] = dict(kick_hops=fires,
                    stable_feature_cpu_s=feature_cpu, decision_cpu_s=time.process_time()-start)
        else:
            fires = detect_attack_led(raw, stable, sr, hop, require_low_rise=rule == 'rise')
            suffix = 'attack_led' if rule == 'rise' else 'presence'
            rows[method + '_' + suffix] = dict(kick_hops=fires,
                stable_feature_cpu_s=feature_cpu, decision_cpu_s=time.process_time()-start)
    return rows


def controls(sr):
    t = np.arange(sr * 4) / sr
    bass = (.25*np.sin(2*np.pi*80*t) + .35*np.sin(2*np.pi*160*t)
            + .25*np.sin(2*np.pi*240*t)) * np.minimum(t/.02, 1)
    isolated = [.5, 1., 1.5, 2.]
    rolls = [2.5, 2.625, 2.75, 2.875, 3., 3.125, 3.25, 3.375]
    def kicks(onsets):
        audio = np.zeros(sr * 4)
        local = np.arange(round(.22 * sr)) / sr
        frequency = 220*np.exp(-local/.22*np.log(220/55))
        phase = np.cumsum(2*np.pi*frequency/sr)
        pulse = .8*(1-np.exp(-local/.0015))*np.exp(-local/.095)*np.sin(phase)
        for onset in onsets:
            start = round(onset*sr)
            audio[start:start+len(pulse)] += pulse
        return audio
    return [('stationary_bass', bass, []),
            ('wobbling_bass', bass*(.55+.45*np.sin(2*np.pi*5*t)), []),
            ('isolated_kicks', kicks(isolated), isolated),
            ('rapid_kicks', kicks(rolls), rolls),
            ('kicks_over_bass', kicks(isolated)+bass, isolated)]


def run_controls(rule='presence'):
    results=[]
    for sr in (44100,48000):
        for name,audio,truth in controls(sr):
            raw,hop=causal_features(audio,sr)
            rows=variants(audio,sr,raw,hop,rule)
            rows['v5']=dict(kick_hops=detect_v5(raw,sr,hop))
            for variant,row in rows.items():
                times=[(i+1)*hop/sr for i in row['kick_hops']]
                scores=score_events(times,truth,[dict(start_s=0,end_s=.3,reason='startup')])
                results.append(dict(signal=name,sample_rate=sr,variant=variant,**row,scores=scores))
            print(sr,name,{k:len(v['kick_hops']) for k,v in rows.items()},flush=True)
    return dict(phase='controls',rows=results)


def run_mixes(audio_root, rule='presence'):
    groups={}
    for source in development_sources(audio_root):
        audio_path=Path(source['audio_path'])
        if sha(audio_path)!=source['audio_sha256']:
            raise ValueError('audio differs from frozen reference')
        sr,audio=read_audio(audio_path)
        start=time.process_time();raw,hop=causal_features(audio,sr)
        raw_cpu=time.process_time()-start
        if detect_v5(raw,sr,hop)!=source['native_hops']:
            raise ValueError('native v5 replay mismatch')
        for variant,row in variants(audio,sr,raw,hop,rule).items():
            scores=evaluate(source,[(i+1)*hop/sr for i in row['kick_hops']])
            result=dict(track=source['track'],group=source['group'],audio_sha256=source['audio_sha256'],
                native_replay_exact=True,sample_rate=sr,hop=hop,duration_s=len(audio)/sr,
                raw_feature_cpu_s=raw_cpu,**row,scores=scores,
                cpu_seconds_per_audio_second=(raw_cpu+row['stable_feature_cpu_s']+row['decision_cpu_s'])/(len(audio)/sr))
            groups.setdefault(variant,[]).append(result)
            print(source['track'],variant,[{k:s['accuracy_by_tolerance_ms']['50'][k] for k in ('matched','missed','extra')} for s in scores],flush=True)
    return dict(phase='mixes',variants={k:dict(totals=aggregate(v),tracks=v) for k,v in groups.items()})


def main():
    ap=argparse.ArgumentParser(description=__doc__)
    ap.add_argument('--phase',required=True,choices=('controls','mixes'))
    ap.add_argument('--rule',choices=('pcen','rise','presence'),default='presence')
    ap.add_argument('--out',type=Path,required=True)
    ap.add_argument('--audio-root',type=Path)
    args=ap.parse_args()
    if args.phase=='mixes' and args.audio_root is None:
        ap.error('--audio-root required for mixes')
    result=run_controls(args.rule) if args.phase=='controls' else run_mixes(args.audio_root,args.rule)
    files=['kick_hybrid_experiment.py','kick_stable_rms.py','kick_stable_quadrature.py',
           'kick_dsp_experiments.py','kick_pcen_experiment.py','kick_attack_rejection.py',
           'run_kick_dsp_experiments.py','live_kick_baseline.py','master_kick_comparison.py']
    result['sources_sha256']={f:sha(ROOT/'tools/audio_analysis/eval'/f) for f in files}
    result['rule']=args.rule
    result['method']={
        'pcen':'PCEN on stable low/body envelopes, with and without raw body fast/slow>1.5 candidate requirement. Stable low confirmation; original v5 timeout, floor, rise and body/mid balance.',
        'rise':'Raw v5 body attack>2 and body/low balance; omit simultaneous combined-power jump. Stable low confirmation with original floor, rise, body/mid balance and timeout.',
        'presence':'Post-outcome diagnostic: attack-led rise rule without low-fast/slow rise requirement. Stable low must exceed absolute floor and 15% of recent peak; original body/mid balance and timeout.'}[args.rule]
    result['limitations']='Sequential development trials; no per-song settings, model, GPU or backdating. Native callback deadlines unverified. Earlier results and source snapshots preserved in cache.'
    labels=ROOT/'tests/fixtures/audio_labels'
    result['labels_sha256']={p.name:sha(p) for p in [*labels.glob('*.csv'),labels/'master_passages_2026-10-09.json']}
    args.out.parent.mkdir(parents=True,exist_ok=True)
    args.out.write_text(json.dumps(result,indent=2)+'\n')

if __name__=='__main__':main()
