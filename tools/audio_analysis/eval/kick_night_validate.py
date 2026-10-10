#!/usr/bin/env python3
"""Validate the best balanced night candidate (H22 main) beside baseline and H18.

Reports, with identical machinery for every scorer:
- original 174 versus added 207 label groups at 70 ms;
- stem-snapped physical-onset and perceived-attack labels at 35/50/70 ms;
- per-song label-informed oracle cutoffs (diagnostic only, never a setting),
  the ceiling of each representation;
- extra kinds (near a label = late/duplicate, kick-free core, unrelated).
"""
from __future__ import annotations

import json
import pickle
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[3]))

import numpy as np  # noqa: E402
from scipy.special import expit  # noqa: E402

from tools.audio_analysis.eval.kick_night_common import (  # noqa: E402
    NIGHT, TRACKS, Data, baseline_logit, evaluate_scorer, h18_logit, run_track)
from tools.audio_analysis.eval.kick_night_diagnose import extra_kinds  # noqa: E402
from tools.audio_analysis.eval.kick_night_relabel import relabel  # noqa: E402
from tools.audio_analysis.eval.run_kick_night_h22 import CONFIGS, load_inputs, make_h22  # noqa: E402


def groups(by_track):
    out = {'original_174': [0, 0, 0], 'added_207': [0, 0, 0]}
    for passages in by_track.values():
        for p in passages:
            g = 'added_207' if str(p['id']).startswith('expanded_') else 'original_174'
            a = p['accuracy_by_tolerance_ms']['70']
            out[g][0] += a['matched']; out[g][1] += a['extra']; out[g][2] += p['labels']
    return out


def oracle(d, logit_fn):
    m = e = 0
    for t in TRACKS:
        p = expit(logit_fn(t, t))
        best = None
        for th in np.unique(np.quantile(p, np.linspace(.5, 1, 201))):
            _, ps = run_track(d.records[t], p, th)
            mm = sum(x['accuracy_by_tolerance_ms']['70']['matched'] for x in ps)
            ee = sum(x['accuracy_by_tolerance_ms']['70']['extra'] for x in ps)
            if best is None or mm - ee > best[0] - best[1]:
                best = (mm, ee)
        m += best[0]; e += best[1]
    return m, e


def scorers(d):
    glide, act = load_inputs()
    h22, _ = make_h22(d, glide, act, *CONFIGS['h18_gate_glide_lowfit'])
    return {'baseline': lambda o, t: baseline_logit(d, o, t), 'h18': lambda o, t: h18_logit(d, o, t), 'h22': h22}


def main():
    report = {}
    for key in ('midpoint', 'physical_s', 'attack_s'):
        d = Data()
        if key != 'midpoint':
            relabel(d, key)
        for name, fn in scorers(d).items():
            s, by_track, fires = evaluate_scorer(d, lambda o, t, fn=fn: expit(fn(o, t)), name=name)
            row = dict(tol={k: (v['matched'], v['extra']) for k, v in s['tol'].items()}, cores=s['kick_free_extras'],
                       delay=(round(s['delay_p50'], 1), round(s['delay_p90'], 1), round(s['delay_max'], 1), s['late_over_70']))
            if key == 'midpoint':
                row['groups'] = groups(by_track)
                row['oracle'] = oracle(d, fn)
                kinds = {}
                for t in TRACKS:
                    for k, v in extra_kinds(d, t, by_track[t]).items():
                        kinds[k] = kinds.get(k, 0) + v
                row['extra_kinds'] = kinds
                row['per_track'] = {t: s['per_track'][t][:2] for t in TRACKS}
                with open(NIGHT / f'validate_{name}.pkl', 'wb') as f:
                    pickle.dump((s, by_track, fires), f)
            report[f'{key}|{name}'] = row
            print(key, name, row, flush=True)
    (NIGHT / 'validation.json').write_text(json.dumps(report, indent=1, default=str))


if __name__ == '__main__':
    main()
