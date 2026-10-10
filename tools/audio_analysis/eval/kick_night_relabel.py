#!/usr/bin/env python3
"""Rescore baseline, H16 and H18 on the frozen stem-snapped label sets.

Identical machinery for every scorer: nested cutoffs are re-selected on the
new labels inside training folds (the inner v5 extra budgets stay the frozen
numbers). Unsnapped labels keep their reviewed midpoints; regions are unchanged.
"""
from __future__ import annotations

import copy
import json
import pickle
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[3]))

from tools.audio_analysis.eval.kick_night_common import (  # noqa: E402
    NIGHT, Data, baseline_prob, evaluate_scorer, h16_prob, h18_prob)


def relabel(d, key):
    snap = json.loads((NIGHT / 'snapped_labels.json').read_text())
    moved = 0
    for track, info in snap['tracks'].items():
        r = d.records[track]
        src = copy.deepcopy(r['source'])
        mapping = {}
        for x in info['labels']:
            new = x[key]
            if new is not None and abs(new - x['midpoint_s']) <= .03:
                mapping[(x['passage'], round(x['midpoint_s'], 4))] = new
        for p in src['passages']:
            times = []
            for t in p['kick_times_s']:
                nt = mapping.get((p['id'], round(t, 4)), t)
                moved += nt != t
                times.append(min(max(nt, p['review_start_s']), p['review_end_s']))
            p['kick_times_s'] = sorted(set(round(t, 4) for t in times))
        d.records[track] = dict(r, source=src)
    return moved


def main():
    results = {}
    for key in ('midpoint', 'physical_s', 'attack_s'):
        d = Data()
        moved = relabel(d, key) if key != 'midpoint' else 0
        for name, fn in (('baseline', baseline_prob(d)), ('h16', h16_prob(d)), ('h18', h18_prob(d))):
            s, by_track, fires = evaluate_scorer(d, fn, name=name)
            results[(key, name)] = s
            print(key, f'moved {moved}', name, 'labels', s['labels'], {k: (v['matched'], v['extra']) for k, v in s['tol'].items()},
                  'cores', s['kick_free_extras'], 'delay p50/p90', round(s['delay_p50'], 1), round(s['delay_p90'], 1), flush=True)
    (NIGHT / 'relabel_summary.json').write_text(json.dumps({f'{k}|{n}': v for (k, n), v in results.items()}, indent=1, default=str))


if __name__ == '__main__':
    main()
