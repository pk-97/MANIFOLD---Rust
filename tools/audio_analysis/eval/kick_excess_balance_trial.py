"""Fixed diagnostic trial: compare rising body/low energy above recent background.

Only v5's body/low eligibility balance changes: max(fast-slow, 0) in each
band replaces fast power. Keep the existing 1/3 ratio, other eligibility,
confirmation, rearm and timeout rules. No persistence veto or temporal pooling.
Slow power is a background estimate, not source separation.
"""
from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path

import numpy as np

from .kick_attack_rejection import causal_features, read_audio, sha
from .kick_dsp_experiments import detect_v5
from .kick_hybrid_experiment import controls
from .kick_sustain_audit import validate_confirmed_fires
from .live_kick_baseline import score_events
from .run_kick_dsp_experiments import ROOT, development_sources, evaluate


def excess_eligibility(envelopes):
    env = np.asarray(envelopes)
    if env.ndim != 3 or env.shape[1:] != (3, 2):
        raise ValueError('expected (hop, band, fast/slow) envelopes')
    low, body = env[:, 0, :], env[:, 1, :]
    power = low[:, 0] + body[:, 0]
    low_excess = np.maximum(low[:, 0] - low[:, 1], 0)
    body_excess = np.maximum(body[:, 0] - body[:, 1], 0)
    return ((power > 1e-6)
            & (body[:, 0] / (body[:, 1] + 1e-12) > 2)
            & (power > 1.8 * (low[:, 1] + body[:, 1]))
            & (body_excess > low_excess / 3))


def detect_excess(envelopes, sr, hop):
    return detect_v5(envelopes, sr, hop,
                     eligible_mask=excess_eligibility(envelopes))


def compact_score(score):
    wide = score['association_early_35_late_200_ms']
    return dict(strict_50ms={k: score['accuracy_by_tolerance_ms']['50'][k]
                            for k in ('matched', 'missed', 'extra')},
                association=dict(matched=wide['matched'],
                    missed=wide.get('missed', wide.get('unmatched_labels')),
                    extra=wide.get('extra', wide.get('unmatched_triggers'))))


def run(audio_root, feature_cache, heldout_run, out):
    sources = development_sources(audio_root)
    heldout = json.loads(heldout_run.read_text())
    label_path = ROOT / 'tests/fixtures/audio_labels/heldout_passages_2026-10-09.json'
    if sha(label_path) != heldout['labels_sha256']:
        raise ValueError('heldout annotations changed')
    for track in json.loads(label_path.read_text())['tracks']:
        ref = next(t for t in heldout['tracks'] if t['track'] == track['track'])
        sources.append(dict(track=track['track'], group='former_heldout',
            audio_path=track['master_path'], audio_sha256=track['audio_sha256'],
            native_hops=ref['baseline_hops'], passages=track['passages']))
    tracks = []
    for source in sources:
        path = Path(source['audio_path'])
        if sha(path) != source['audio_sha256']:
            raise ValueError(f'audio changed: {path}')
        sr, samples = read_audio(path)
        key = hashlib.sha256((source['audio_sha256'] +
            sha(ROOT / 'tools/audio_analysis/eval/kick_attack_rejection.py')).encode()).hexdigest()
        feature_path = feature_cache / f'{key}.npz'
        if feature_path.exists():
            with np.load(feature_path) as z:
                env, hop = z['env'], int(z['hop'])
        else:
            env, hop = causal_features(samples, sr)
        baseline = detect_v5(env, sr, hop)
        if baseline != source['native_hops']:
            raise ValueError(f'baseline mismatch: {source["track"]}')
        variants = dict(v5=baseline,
            persistence=validate_confirmed_fires(env, baseline, sr, hop)[0],
            excess_balance=detect_excess(env, sr, hop))
        scores = {name: evaluate(source, [(h+1)*hop/sr for h in fires])
                  for name, fires in variants.items()}
        comparisons = []
        for old, new in zip(scores['v5'], scores['excess_balance']):
            associated = lambda p: {r['attack_s'] for r in
                                   p['association_early_35_late_200_ms']['pairs']}
            before, after = associated(old), associated(new)
            comparisons.append(dict(passage=old['id'],
                lost_labels_s=sorted(before-after), recovered_labels_s=sorted(after-before)))
        tracks.append(dict(track=source['track'], group=source['group'],
            audio_sha256=source['audio_sha256'], sample_rate=sr, hop=hop,
            baseline_replay_exact=True, variants=variants, scores=scores,
            comparisons=comparisons))
        print(source['track'], {n: [compact_score(p) for p in s]
                               for n, s in scores.items()}, flush=True)
    control_rows = []
    for sr in (44100, 48000):
        for name, audio, truth in controls(sr):
            env, hop = causal_features(audio, sr)
            scores = {variant: score_events([(i+1)*hop/sr for i in detector(env, sr, hop)],
                truth, [dict(start_s=0, end_s=.3, reason='startup')])
                for variant, detector in [('v5', detect_v5), ('excess_balance', detect_excess)]}
            control_rows.append(dict(name=name, sample_rate=sr, scores=scores))
    totals = {name: {metric: {k: sum(compact_score(p)[metric][k]
        for t in tracks for p in t['scores'][name]) for k in ('matched', 'missed', 'extra')}
        for metric in ('strict_50ms', 'association')}
        for name in ('v5', 'persistence', 'excess_balance')}
    report = dict(method=__doc__, decision_rule='Accept only with more associated kicks, '
        'no original associated labels lost, no increase in aggregate unmatched or any '
        'reviewed bass-only passage. No threshold sweep; failure stays an offline diagnostic.',
        totals=totals, tracks=tracks, controls=control_rows,
        source_sha256={p: sha(ROOT / 'tools/audio_analysis/eval' / p) for p in (
            'kick_excess_balance_trial.py', 'kick_dsp_experiments.py',
            'kick_attack_rejection.py', 'kick_sustain_audit.py', 'kick_hybrid_experiment.py',
            'live_kick_baseline.py', 'master_kick_comparison.py', 'run_kick_dsp_experiments.py')},
        label_sha256={p.name: sha(p) for p in (ROOT / 'tests/fixtures/audio_labels').glob('*')
                      if p.is_file() and p.suffix in ('.csv', '.json')},
        heldout_run_sha256=sha(heldout_run), limitations='Nine recordings, scored existing '
        'short passages only, provisional labels. Heavy On Mind informed this change; '
        'Miracle and Heavy On Mind are now development data. Full chronological native-rate '
        'mono audio; no resets, backdating or offsets. No app/real-time throughput claim.')
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(report, indent=2)+'\n')
    print(json.dumps(totals, indent=2))
    return report


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--audio-root', type=Path, required=True)
    parser.add_argument('--feature-cache', type=Path, required=True)
    parser.add_argument('--heldout-run', type=Path, required=True)
    parser.add_argument('--out', type=Path, required=True)
    args = parser.parse_args()
    run(args.audio_root, args.feature_cache, args.heldout_run, args.out)


if __name__ == '__main__':
    main()
