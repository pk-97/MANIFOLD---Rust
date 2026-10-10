"""Fixed sustained-bass trials, preserving original v5 decisions and timestamps.

The candidate gate requires two successive body-attack hops before eligibility.
The recovery variant instead validates original v5 fires with two-hop evidence
within 35 ms before or after the original fire. Output time is validation time,
never backdated. This is an offline development experiment, not the live app.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import math
from pathlib import Path

import numpy as np

from .kick_attack_rejection import causal_features, read_audio
from .kick_dsp_experiments import detect_v5
from .kick_hybrid_experiment import controls
from .live_kick_baseline import score_events, sha
from .run_kick_dsp_experiments import aggregate, development_sources, evaluate, ROOT

WINDOW_S = .035
VARIANTS = ('candidate_persistence', 'confirmed_persistence')


def body_persistence_mask(envelopes):
    env = np.asarray(envelopes)
    if env.ndim != 3 or env.shape[1:] != (3, 2):
        raise ValueError('expected (hop, band, fast/slow) envelopes')
    low, body = env[:, 0, :], env[:, 1, :]
    attack = body[:, 0] / (body[:, 1] + 1e-12) > 2
    previous = np.zeros(len(env), dtype=bool)
    previous[1:] = attack[:-1]
    return (attack & previous & (low[:, 0] + body[:, 0] > 1e-6)
            & (low[:, 0] + body[:, 0] > 1.8 * (low[:, 1] + body[:, 1]))
            & (body[:, 0] > low[:, 0] / 3))


def detect_body_persistence(envelopes, sample_rate, hop):
    return detect_v5(envelopes, sample_rate, hop,
                     eligible_mask=body_persistence_mask(envelopes))


def validate_confirmed_fires(envelopes, original_fires, sample_rate, hop):
    """Causal output validation. Rejected outputs do not rearm the original v5.

    Two consecutive body attack ratios above 2 supply persistence evidence.
    A fire can use evidence from the preceding 35 ms, or remain pending for
    at most 35 ms. Original fires are >=60 ms apart, so only one can be pending.
    Output spacing can shrink below 60 ms when adjacent fires wait differently;
    this is reported, not silently suppressed or claimed as app-ready behavior.
    """
    if sample_rate <= 0 or hop <= 0:
        raise ValueError('invalid sample grid')
    budget = math.floor(WINDOW_S * sample_rate / hop)
    originals = set(original_fires)
    last_pair, pending = -math.inf, None
    previous = False
    fires, decisions = [], []
    for i, bands in enumerate(envelopes):
        attack = bands[1, 0] / (bands[1, 1] + 1e-12) > 2
        pair = attack and previous
        previous = attack
        if pair:
            last_pair = i
        if pending is not None:
            if i - pending > budget:
                decisions.append(dict(original_hop=pending, accepted_hop=None))
                pending = None
            elif pair:
                decisions.append(dict(original_hop=pending, accepted_hop=i))
                fires.append(i)
                pending = None
        if i in originals:
            if pending is not None:
                raise ValueError('overlapping original fires exceed single-pending contract')
            if i - last_pair <= budget:
                decisions.append(dict(original_hop=i, accepted_hop=i))
                fires.append(i)
            else:
                pending = i
    if pending is not None:
        decisions.append(dict(original_hop=pending, accepted_hop=None,
                              status='unconfirmed_eof'))
    return fires, decisions


def run(audio_root, out, cache=None):
    feature_source_sha = sha(ROOT / 'tools/audio_analysis/eval/kick_attack_rejection.py')
    rows = {v: [] for v in VARIANTS}
    for source in development_sources(audio_root):
        path = Path(source['audio_path'])
        if sha(path) != source['audio_sha256']:
            raise ValueError(f'{source["track"]}: audio hash differs from frozen source')
        sr, samples = read_audio(path)
        key = hashlib.sha256((source['audio_sha256'] + feature_source_sha).encode()).hexdigest()
        feature_path = cache / f'{key}.npz' if cache else None
        if feature_path is not None and feature_path.exists():
            with np.load(feature_path) as z:
                env, hop = z['env'], int(z['hop'])
        else:
            env, hop = causal_features(samples, sr)
        original = detect_v5(env, sr, hop)
        if original != source['native_hops']:
            raise ValueError(f'{source["track"]}: native replay mismatch')
        candidate = detect_body_persistence(env, sr, hop)
        recovered, decisions = validate_confirmed_fires(env, original, sr, hop)
        baseline_scores = evaluate(source, [(i+1)*hop/sr for i in original])
        for variant, fires in zip(VARIANTS, (candidate, recovered)):
            scores = evaluate(source, [(i+1)*hop/sr for i in fires])
            row = dict(track=source['track'], group=source['group'],
                       audio_sha256=source['audio_sha256'], sample_rate=sr, hop=hop,
                       native_baseline_replay_exact=True, kick_hops=fires,
                       scores=scores, baseline_scores=baseline_scores)
            if variant == 'confirmed_persistence':
                row['decisions'] = decisions
                row['output_gaps_under_60ms'] = [dict(available_s=(b+1)*hop/sr,
                    gap_ms=(b-a)*hop/sr*1000) for a,b in zip(fires,fires[1:])
                    if (b-a)*hop/sr < .060-1e-12]
            rows[variant].append(row)
            print(source['track'], variant,
                  [{k:s['accuracy_by_tolerance_ms']['50'][k] for k in
                    ('matched','missed','extra')} for s in scores], flush=True)
    control_rows = []
    for sr in (44100,48000):
        for name, audio, truth in controls(sr):
            env,hop = causal_features(audio,sr)
            original = detect_v5(env,sr,hop)
            confirmed,_ = validate_confirmed_fires(env,original,sr,hop)
            for variant,fires in [('v5',original),
                    ('candidate_persistence',detect_body_persistence(env,sr,hop)),
                    ('confirmed_persistence',confirmed)]:
                control_rows.append(dict(name=name,sr=sr,variant=variant,kick_hops=fires,
                    scores=score_events([(i+1)*hop/sr for i in fires],truth,
                    [dict(start_s=0,end_s=.3,reason='startup')])))
    source_names = ('kick_sustain_audit.py','kick_attack_rejection.py',
                    'kick_dsp_experiments.py','kick_hybrid_experiment.py',
                    'run_kick_dsp_experiments.py','live_kick_baseline.py',
                    'master_kick_comparison.py')
    labels = ROOT / 'tests/fixtures/audio_labels'
    report = dict(method=__doc__,settings=dict(body_ratio=2,completed_hops=2,
        validation_window_s=WINDOW_S),variants={v:dict(tracks=r,totals=aggregate(r))
        for v,r in rows.items()},controls=control_rows,
        sources_sha256={n:sha(ROOT/'tools/audio_analysis/eval'/n) for n in source_names},
        labels_sha256={p.name:sha(p) for p in [*labels.glob('*.csv'),
            labels/'master_passages_2026-10-09.json']},
        limitations='Sequential development trials, no independent held-out evaluation. '
        'Provisional visual labels plus qualitative user listening feedback. '
        'Output validation adds up to 35ms beyond original v5 latency. '
        'Unequal waits can shorten output spacing below60ms. No app change.')
    out.parent.mkdir(parents=True,exist_ok=True)
    out.write_text(json.dumps(report,indent=2)+'\n')
    return report


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--audio-root',type=Path,required=True)
    parser.add_argument('--out',type=Path,required=True)
    parser.add_argument('--cache',type=Path)
    args = parser.parse_args()
    run(args.audio_root,args.out,args.cache)


if __name__ == '__main__':
    main()
