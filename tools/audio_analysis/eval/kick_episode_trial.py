"""One fixed recovery trial: retain v5 evidence inside a body-attack episode.

Start on body fast/slow >2 after rearm (<1.2), collect balance and combined rise
for at most35ms, and require two adjacent body-attack hops. A return below1.2
ends the episode. Original v5 eligibility is always retained. This is not the
previous unrestricted temporal-predicate accumulator. No threshold sweep.
"""
from __future__ import annotations
import argparse
import hashlib
import json
import math
from pathlib import Path
import numpy as np
from .kick_attack_rejection import causal_features, read_audio, sha
from .kick_dsp_experiments import detect_v5
from .kick_sustain_audit import validate_confirmed_fires
from .kick_hybrid_experiment import controls
from .run_kick_dsp_experiments import development_sources, evaluate, aggregate, ROOT
from .live_kick_baseline import score_events


def episode_eligibility(env, sr, hop):
    budget = math.floor(.035 * sr / hop)
    eligible = np.zeros(len(env), dtype=bool)
    armed, start, previous = True, None, False
    balance_seen = rise_seen = pair_seen = False
    for i, bands in enumerate(env):
        low, body, _ = bands
        power = low[0] + body[0]
        floor = power > 1e-6
        ratio = body[0] / (body[1] + 1e-12)
        attack = ratio > 2
        balance = body[0] > low[0] / 3
        rise = power > 1.8 * (low[1] + body[1])
        eligible[i] = floor and attack and balance and rise
        if ratio < 1.2 or not floor:
            armed, start, previous = True, None, False
        if start is not None and i - start > budget:
            start = None
        if armed and attack and floor:
            start, armed = i, False
            balance_seen = rise_seen = pair_seen = False
            previous = False
        if start is not None:
            balance_seen |= balance
            rise_seen |= rise
            pair_seen |= attack and previous
            if floor and balance_seen and rise_seen and pair_seen:
                eligible[i] = True
                start = None
        previous = attack
    return eligible


def detect_episode(env, sr, hop):
    raw = detect_v5(env, sr, hop, eligible_mask=episode_eligibility(env, sr, hop))
    return validate_confirmed_fires(env, raw, sr, hop)[0]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--audio-root',type=Path,required=True)
    parser.add_argument('--cache',type=Path,required=True)
    parser.add_argument('--reference',type=Path,required=True)
    parser.add_argument('--out',type=Path,required=True)
    args = parser.parse_args()
    ref=json.loads(args.reference.read_text())['variants']['confirmed_persistence']
    refs={t['track']:t for t in ref['tracks']}
    rows=[]
    for s in development_sources(args.audio_root):
        assert sha(Path(s['audio_path'])) == s['audio_sha256']
        sr,samples=read_audio(Path(s['audio_path']))
        key=hashlib.sha256((s['audio_sha256']+sha(ROOT/'tools/audio_analysis/eval/kick_attack_rejection.py')).encode()).hexdigest()
        with np.load(args.cache/f'{key}.npz') as z:env=z['env'];hop=int(z['hop'])
        original=detect_v5(env,sr,hop)
        assert original==s['native_hops']
        assert validate_confirmed_fires(env,original,sr,hop)[0]==refs[s['track']]['kick_hops']
        fires=detect_episode(env,sr,hop)
        scores=evaluate(s,[(i+1)*hop/sr for i in fires])
        rows.append(dict(track=s['track'],group=s['group'],sample_rate=sr,hop=hop,audio_sha256=s['audio_sha256'],native_and_improved_reference_replay_exact=True,kick_hops=fires,scores=scores))
        print(s['track'],[{k:p['accuracy_by_tolerance_ms']['50'][k] for k in ('matched','missed','extra')} for p in scores],flush=True)
    control_rows=[]
    for sr in (44100,48000):
        for name,audio,truth in controls(sr):
            env,hop=causal_features(audio,sr)
            baseline=validate_confirmed_fires(env,detect_v5(env,sr,hop),sr,hop)[0]
            for variant,fires in [('improved_reference',baseline),('episode',detect_episode(env,sr,hop))]:
                control_rows.append(dict(name=name,sample_rate=sr,variant=variant,scores=score_events([(i+1)*hop/sr for i in fires],truth,[dict(start_s=0,end_s=.3,reason='startup')])) )
    source_names=['kick_episode_trial.py','kick_dsp_experiments.py','kick_sustain_audit.py','kick_attack_rejection.py','kick_hybrid_experiment.py','run_kick_dsp_experiments.py','live_kick_baseline.py','master_kick_comparison.py']
    labels=ROOT/'tests/fixtures/audio_labels'
    report=dict(method=__doc__,tracks=rows,totals=aggregate(rows),controls=control_rows,reference_sha256=sha(args.reference),sources_sha256={n:sha(ROOT/'tools/audio_analysis/eval'/n) for n in source_names},labels_sha256={p.name:sha(p) for p in [*labels.glob('*.csv'),labels/'master_passages_2026-10-09.json']},limitations='One fixed sequential development experiment. All parameters global. Actual hop availability, no backdating. Heldouts untouched; app unchanged.')
    args.out.parent.mkdir(parents=True,exist_ok=True)
    args.out.write_text(json.dumps(report,indent=2)+'\n')
    print(report['totals'])

if __name__=='__main__':main()
