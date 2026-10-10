#!/usr/bin/env python3
"""Why kernel (H16) and conservative (H18) disagree: training support at each decision.

For every outer-fold candidate, the RBF kernel activity sum |a_i| K(x, sv_i)
measures how much training evidence lies near x. Far from training data the
kernel margin collapses to its intercept, so rare shapes inherit the bias.
Compares that support and the three component logits across event classes:
labels both catch, kernel-only recoveries, labels the kernel loses versus the
baseline, kernel extras and H18 extras.
"""
from __future__ import annotations

import json
import pickle
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[3]))

import numpy as np  # noqa: E402
from scipy.spatial.distance import cdist  # noqa: E402

from tools.audio_analysis.eval.kick_kernel_score import standardise  # noqa: E402
from tools.audio_analysis.eval.kick_night_common import NIGHT, TRACKS, Data, missed_ids  # noqa: E402
from tools.audio_analysis.eval.kick_night_diagnose import emission_times, scored_labels  # noqa: E402
from tools.audio_analysis.eval.kick_night_dump import EVENING  # noqa: E402
from tools.audio_analysis.eval.kick_threeway_blend import FrozenFitter  # noqa: E402


def support(model, x):
    z = standardise(model, x)
    sv = np.asarray(model['support_vectors'])
    dual = np.asarray(model['dual_coefficients'])
    act, near = np.empty(len(z)), np.empty(len(z))
    for s in range(0, len(z), 256):
        k = np.exp(-model['gamma'] * cdist(z[s:s + 256], sv, 'sqeuclidean'))
        act[s:s + 256] = k @ np.abs(dual)
        near[s:s + 256] = k.max(axis=1)
    return act, near


def main():
    d = Data()
    h10, h16 = d.inputs['h10'], d.inputs['h16_compact']
    covered = next(v for v in h10['variants'] if v['variant'] == 'coverage_linear15')
    inter = next(v for v in h10['variants'] if v['variant'] == 'coverage_interaction15')
    fitter = FrozenFitter(covered, h16['variants'][-1], inter, EVENING / 'h16/models')
    with open(NIGHT / 'replay.pkl', 'rb') as f:
        replay = pickle.load(f)
    rows = []
    for t in TRACKS:
        r = d.records[t]
        model = fitter.fit(list(d.records.values()), t, .25)
        ref = model['base']['kernel_reference']
        km = json.loads(Path(ref['path']).read_text())['model']
        act, near = support(km, d.features(t))
        em = emission_times(r)
        a, b, c = (d.logits(t, t, k) for k in ('linear', 'kernel', 'interaction'))
        miss = {n: {x[1] for x in missed_ids(replay[n][1][t])} for n in ('baseline', 'h16', 'h18')}
        labels, *_ = scored_labels(r['source'])
        for _, lt in labels:
            m = np.flatnonzero(np.abs(em - lt) <= .07)
            if not len(m):
                continue
            i = m[np.argmax(b[m])]
            key = round(lt, 4)
            caught = {n: key not in miss[n] for n in miss}
            if caught['h16'] and caught['baseline']:
                cls = 'both'
            elif caught['h16']:
                cls = 'kernel_recovered'
            elif caught['baseline']:
                cls = 'kernel_lost'
            else:
                cls = 'both_missed'
            rows.append(dict(track=t, cls=cls, h18=caught['h18'], act=act[i], near=near[i], lin=a[i], ker=b[i], inter=c[i],
                             intercept=km['intercept']))
        for n in ('h16', 'h18'):
            for p in replay[n][1][t]:
                for e in p['accuracy_by_tolerance_ms']['70']['extra_times_s']:
                    i = int(np.argmin(np.abs(em - e)))
                    rows.append(dict(track=t, cls=f'{n}_extra', h18=None, act=act[i], near=near[i], lin=a[i], ker=b[i], inter=c[i],
                                     intercept=km['intercept']))
        print('support', t, flush=True)
    out = {}
    for cls in ('both', 'kernel_recovered', 'kernel_lost', 'both_missed', 'h16_extra', 'h18_extra'):
        sel = [x for x in rows if x['cls'] == cls]
        if not sel:
            continue
        med = lambda k: round(float(np.median([x[k] for x in sel])), 3)  # noqa: E731
        out[cls] = dict(n=len(sel), activity=med('act'), nearest=med('near'), linear=med('lin'), kernel=med('ker'),
                        interaction=med('inter'), low_support_share=round(float(np.mean([x['act'] < 5 for x in sel])), 3))
        print(cls, out[cls])
    rec = [x for x in rows if x['cls'] == 'kernel_recovered']
    print('kernel-recovered also caught by H18:', sum(x['h18'] for x in rec), 'of', len(rec))
    lost18 = [x for x in rows if x['cls'] in ('both', 'kernel_lost') and x['h18'] is False]
    print('baseline-caught labels H18 loses:', [(x['track'][:6], round(x['act'], 2), round(x['lin'], 2), round(x['ker'], 2), round(x['inter'], 2)) for x in lost18])
    (NIGHT / 'kernel_support.json').write_text(json.dumps(dict(summary=out, rows=rows), indent=1, default=float))


if __name__ == '__main__':
    main()
