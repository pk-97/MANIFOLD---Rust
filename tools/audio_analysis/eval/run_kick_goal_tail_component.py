#!/usr/bin/env python3
"""Component test for the tail-aware features, alone, before any combining.

Declared: per song, AUC of each feature for scored kick candidates (positives)
against (a) all scored non-kick candidates and (b) candidates at strict-removed
roll labels (dev only). Expected failure: rolls over a loud tail still clear
the extrapolated tail by several dB, and sustained bass hides the kick tail.
Pass to proceed: tail_excess_db AUC >= 0.75 against rolls in Midnight and Late
Night, and >= 0.70 against all negatives pooled within songs.
"""
from __future__ import annotations

import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[3]))

import numpy as np  # noqa: E402

from tools.audio_analysis.eval.kick_attack_rejection import read_audio  # noqa: E402
from tools.audio_analysis.eval.kick_goal_eval import GOAL, Goal  # noqa: E402
from tools.audio_analysis.eval.kick_goal_tail_features import TAIL_NAMES, tail_features  # noqa: E402
from tools.audio_analysis.eval.kick_night_diagnose import auc  # noqa: E402


def mix_audio(g, t):
    r = g.records[t]
    if t in g.labels['new']:
        info = g.labels['new'][t]
        if info['mix_path'] is None:
            return np.load(GOAL / f'{t}_mix.npy').astype(np.float64)
        return read_audio(info['mix_path'], r['sample_rate'])[1]
    return read_audio(r['source']['audio_path'], r['sample_rate'])[1]


def tail_cache(g, t):
    path = GOAL / f'tail_{t}.npy'
    if path.exists():
        return np.load(path)
    r = g.records[t]
    f = tail_features(mix_audio(g, t), r['sample_rate'], r['onset_s'], r['emit_s'])
    np.save(path, f)
    return f


def main():
    g = Goal(mode='strict')
    report = {}
    for t, r in g.records.items():
        f = tail_cache(g, t)
        m, y = r['training_mask'], r['labels']
        pos, neg = f[m & (y == 1)], f[m & (y == 0)]
        row = {n: round(auc(pos[:, k], neg[:, k]), 3) for k, n in enumerate(TAIL_NAMES)}
        if r['removed']:
            rolls = np.array(r['removed'])
            near = np.array([np.any(np.abs(rolls - o) <= .035) for o in r['onset_s']]) & m
            if near.sum() >= 3:
                row.update({f'{n}_vs_rolls': round(auc(pos[:, k], f[near, k]), 3) for k, n in enumerate(TAIL_NAMES)})
                row['roll_candidates'] = int(near.sum())
        row['medians_pos_neg'] = {n: [round(float(np.median(pos[:, k])), 2), round(float(np.median(neg[:, k])), 2)]
                                  for k, n in enumerate(TAIL_NAMES)}
        report[t] = row
        print(t, {k: v for k, v in row.items() if k != 'medians_pos_neg'}, flush=True)
    (GOAL / 'tail_component.json').write_text(json.dumps(report, indent=1))


if __name__ == '__main__':
    main()
