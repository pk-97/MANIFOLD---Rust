#!/usr/bin/env python3
"""H21: gate the kernel by its own training support, falling back to linear.

Declared before scoring (rule written to the night cache first):
- Diagnosis (kick_night_kernel_support.py): 4 of 5 labels the kernel loses
  against the baseline have kernel activity sum|a_i|K < 5 (median 0.7) with
  confident linear logits (median 4.3); kernel recoveries sit in dense support
  (median 13.8, 1.5% below 5). Far from training data an RBF margin collapses
  to its intercept, so rare kick shapes inherit the bias.
- Score: g * S + (1 - g) * L, g = act / (act + 5), act from the same fold's
  kernel model; L = frozen H10 covered-linear logit of that fold.
- Configurations: S = H16 kernel margin / S = H18 .25 blend logit /
  S = H16 kernel with H18 as the fallback instead of L.
- kappa = 5 comes from the all-song diagnostic split: development, disclosed.
- Unchanged: candidates, 60 ms refractory, nested coarse+refined cutoffs.
- Expected failure: 45% of H18 extras are also low-support with high linear
  logits, so the fallback can re-admit them and add kick-free core fires.
- Acceptance: intermediate target.
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
from tools.audio_analysis.eval.kick_night_dump import EVENING  # noqa: E402
from tools.audio_analysis.eval.kick_night_kernel_support import support  # noqa: E402
from tools.audio_analysis.eval.kick_threeway_blend import FrozenFitter  # noqa: E402

RULE = dict(hypothesis='H21', kappa=5.0, gate='act/(act+kappa)',
            configurations=['h16_gated_linear', 'h18_gated_linear', 'h16_gated_h18'],
            acceptance='intermediate target', expected_failure='low-support extras re-admitted by linear fallback')


def activities(d):
    path = NIGHT / 'kernel_activity.npz'
    if path.exists():
        with np.load(path) as z:
            return {k: z[k] for k in z.files}
    h10, h16 = d.inputs['h10'], d.inputs['h16_compact']
    covered = next(v for v in h10['variants'] if v['variant'] == 'coverage_linear15')
    inter = next(v for v in h10['variants'] if v['variant'] == 'coverage_interaction15')
    fitter = FrozenFitter(covered, h16['variants'][-1], inter, EVENING / 'h16/models')
    records = [d.records[t] for t in TRACKS]
    models, out = {}, {}
    for o in TRACKS:
        for t in TRACKS:
            pool = records if o == t else [r for r in records if r['track'] != o]
            ref = fitter.fit(pool, t, .25)['base']['kernel_reference']
            if ref['path'] not in models:
                models[ref['path']] = json.loads(Path(ref['path']).read_text())['model']
            out[f'{o}|{t}'] = support(models[ref['path']], d.features(t))[0]
        models.clear()
        print('activity', o, flush=True)
    np.savez(path, **out)
    return out


def main():
    out = NIGHT / 'h21'
    out.mkdir(exist_ok=True)
    rule_path = out / 'rule.json'
    if not rule_path.exists():
        rule_path.write_text(json.dumps(RULE, indent=1))
    if json.loads(rule_path.read_text()) != RULE:
        raise ValueError('predeclared rule differs')
    d = Data()
    act = activities(d)
    with open(NIGHT / 'replay.pkl', 'rb') as f:
        replay = pickle.load(f)

    def gate(o, t):
        a = act[f'{o}|{t}']
        return a / (a + RULE['kappa'])

    scorers = {
        'h16_gated_linear': lambda o, t: gate(o, t) * d.logits(o, t, 'kernel') + (1 - gate(o, t)) * d.logits(o, t, 'linear'),
        'h18_gated_linear': lambda o, t: gate(o, t) * h18_logit(d, o, t) + (1 - gate(o, t)) * d.logits(o, t, 'linear'),
        'h16_gated_h18': lambda o, t: gate(o, t) * d.logits(o, t, 'kernel') + (1 - gate(o, t)) * h18_logit(d, o, t),
    }
    report = dict(rule=RULE, rule_sha256=hashlib.sha256(rule_path.read_bytes()).hexdigest(), variants=[])
    for name in RULE['configurations']:
        fn = scorers[name]
        s, by_track, fires = evaluate_scorer(d, lambda o, t, fn=fn: expit(fn(o, t)), name=name)
        ret = retention(replay['baseline'][1], by_track)
        acc = acceptance(s, ret)
        report['variants'].append(dict(name=name, summary=s, acceptance=acc,
                                       lost_vs_baseline={t: v['lost'] for t, v in ret.items() if v['lost']},
                                       recovered_vs_baseline=sum(len(v['recovered']) for v in ret.values())))
        with open(out / f'{name}.pkl', 'wb') as f:
            pickle.dump((s, by_track, fires), f)
        print(name, {k: (v['matched'], v['extra']) for k, v in s['tol'].items()}, 'cores', s['kick_free_extras'],
              'per', {t[:6]: v[:2] for t, v in s['per_track'].items()}, flush=True)
        print('   acceptance', acc, 'lost', report['variants'][-1]['lost_vs_baseline'],
              'recovered', report['variants'][-1]['recovered_vs_baseline'], flush=True)
    (out / 'trial.json').write_text(json.dumps(report, indent=1, default=str))


if __name__ == '__main__':
    main()
