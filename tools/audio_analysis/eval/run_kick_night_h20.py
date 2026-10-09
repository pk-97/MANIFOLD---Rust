#!/usr/bin/env python3
"""H20: past-only upper-anchor cutoff tracking on the frozen H18 .25 score.

Declared before scoring (rule written to the night cache first):
- Hypothesis: H12 re-centred on the median of all recent candidates, which is
  set by the ~40/s non-kick candidates. Anchoring on the upper tail of recent
  scores may track where this song's kicks sit.
- Score: logit' = logit - g * clip(A_past - A_ref, -2, 2), g = min(n_past/64, 1);
  A_past from candidates emitted in the previous 8 s (current one excluded);
  A_ref = mean of whole-song A over the training songs only.
- Configurations: A = 90th percentile / 99th percentile of past logits /
  median of past 250 ms block maxima.
- Unchanged: candidates, 60 ms refractory, nested coarse+refined cutoffs.
- Expected failure: the anchor follows accompaniment density, not kick level
  (night anchor probe: best whole-song statistic only cuts cutoff spread
  0.74 -> 0.52 logit), and it sags inside kick-free passages, adding core fires.
- Acceptance: intermediate target, as H19.
"""
from __future__ import annotations

import hashlib
import json
import pickle
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[3]))

import numpy as np  # noqa: E402
from scipy.special import expit  # noqa: E402

from tools.audio_analysis.eval.kick_night_common import (  # noqa: E402
    NIGHT, TRACKS, Data, acceptance, evaluate_scorer, h18_logit, retention)

RULE = dict(hypothesis='H20', base='H18 .25', history_s=8.0, warmup=64, cap=2.0,
            configurations=['p90', 'p99', 'block_max_median'],
            acceptance='intermediate target as H19', expected_failure='anchor tracks accompaniment; sags in kick-free cores')


def anchor_stat(values, kind, times=None):
    if len(values) == 0:
        return np.nan
    if kind == 'p90':
        return np.percentile(values, 90)
    if kind == 'p99':
        return np.percentile(values, 99)
    blocks = np.floor(times / .25)
    return float(np.median([values[blocks == b].max() for b in np.unique(blocks)]))


def emission(r):
    return (np.asarray(r['available']) + 1) * r['hop'] / r['sample_rate']


def tracked(d, lg, track, kind, ref):
    t = emission(d.records[track])
    out = np.empty_like(lg)
    lo = 0
    for i in range(len(lg)):
        while t[lo] < t[i] - RULE['history_s']:
            lo += 1
        past = lg[lo:i]
        if len(past) == 0:
            out[i] = lg[i]
            continue
        g = min(len(past) / RULE['warmup'], 1.0)
        a = anchor_stat(past, kind, t[lo:i])
        out[i] = lg[i] - g * np.clip(a - ref, -RULE['cap'], RULE['cap'])
    return out


def main():
    out = NIGHT / 'h20'
    out.mkdir(exist_ok=True)
    rule_path = out / 'rule.json'
    if not rule_path.exists():
        rule_path.write_text(json.dumps(RULE, indent=1))
    if json.loads(rule_path.read_text()) != RULE:
        raise ValueError('predeclared rule differs')
    d = Data()
    with open(NIGHT / 'replay.pkl', 'rb') as f:
        replay = pickle.load(f)
    report = dict(rule=RULE, rule_sha256=hashlib.sha256(rule_path.read_bytes()).hexdigest(), variants=[])
    for kind in RULE['configurations']:
        cache = {}

        def prob(o, t, kind=kind, cache=cache):
            if (o, t) not in cache:
                training = [u for u in TRACKS if u not in (o, t)]
                ref = float(np.mean([anchor_stat(h18_logit(d, o, u), kind, emission(d.records[u]))
                                     for u in training]))
                cache[(o, t)] = expit(tracked(d, h18_logit(d, o, t), t, kind, ref))
            return cache[(o, t)]

        s, by_track, fires = evaluate_scorer(d, prob, name=kind)
        ret = retention(replay['baseline'][1], by_track)
        acc = acceptance(s, ret)
        report['variants'].append(dict(name=kind, summary=s, acceptance=acc,
                                       lost_vs_baseline={t: v['lost'] for t, v in ret.items() if v['lost']},
                                       recovered_vs_baseline=sum(len(v['recovered']) for v in ret.values())))
        with open(out / f'{kind}.pkl', 'wb') as f:
            pickle.dump((s, by_track, fires), f)
        print(kind, s['tol'], 'cores', s['kick_free_extras'], 'per', {t[:6]: v[:2] for t, v in s['per_track'].items()}, flush=True)
        print('   acceptance', acc, 'lost', {t[:6]: len(v) for t, v in report['variants'][-1]['lost_vs_baseline'].items()}, flush=True)
    (out / 'trial.json').write_text(json.dumps(report, indent=1, default=str))


if __name__ == '__main__':
    main()
