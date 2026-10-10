#!/usr/bin/env python3
"""H-D: does more, more varied training data fix transfer? Strict truth v2.

Predeclared, three configs, 13-song whole-song nested evaluation (each song
scored by models that never saw it; thresholds by inner pooled F1):
- C1 gbt15_whole: frozen 15 features, trees, whole-song kick-stem training truth
  on the eight own-stem songs plus the original five.
- C2 gbt21_whole: C1 plus the three tail features and three template features.
- C3 gbt15_passages: C1 learner on the passage-only training truth (control).
Dev songs are scored on reviewed v2 passages, new songs on their whole length.
Expected failure: whole-song auto truth adds label noise and the extra songs
pull the cutoff toward their own scale.
Pass: C1 beats C3 by >= 0.03 in both dev precision and recall, and new-song F1
by >= 0.05. Reference: dev-only gbt15 strict R .842 P .657; new R .652 P .521.
"""
from __future__ import annotations

import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[3]))

import numpy as np  # noqa: E402
from sklearn.ensemble import HistGradientBoostingClassifier  # noqa: E402

from tools.audio_analysis.eval.kick_goal_eval import (  # noqa: E402
    GOAL, MORE_SONGS, NEW_SONGS, RECALL_SONGS, TRACKS, TRIGGER_SONGS, WIP2_SONGS, WIP_SONGS, Goal, add_whole_song_truth, nested, summarise,
    training_set)

ALL = tuple(TRACKS) + NEW_SONGS + MORE_SONGS + TRIGGER_SONGS + WIP_SONGS + RECALL_SONGS + WIP2_SONGS


def gbt(g, whole, feats):
    def fit(tracks):
        x, y, w = training_set(g, tracks, whole=whole, feats=feats)
        return HistGradientBoostingClassifier(max_iter=200, learning_rate=.05, max_leaf_nodes=15, min_samples_leaf=40,
                                              l2_regularization=1.0, random_state=0).fit(x, y, sample_weight=w * len(y))
    return fit, (lambda model, t: model.predict_proba(g.records[t][feats])[:, 1])


def line(name, s):
    t = s['tol']
    return (f"{name:18s} {t['70']['matched']}/{s['labels']}+{t['70']['extra']} R {s['recall']} P {s['precision']} "
            f"minR {s['min_track_recall']} tail {s['tail_fires']} core {s['core_fires']} 50ms {t['50']['matched']} delay {s['delay_ms']}")


def main():
    g = Goal(mode='strict')
    add_whole_song_truth(g)
    for t, r in g.records.items():
        r['f21'] = np.hstack([r['features'], np.load(GOAL / f'tail_{t}.npy'), np.load(GOAL / f'tmpl_{t}.npy')])
    out = {}
    for name, whole, feats in (('C1_gbt15_whole', True, 'features'), ('C2_gbt21_whole', True, 'f21'),
                               ('C3_gbt15_passages', False, 'features')):
        fit, pred = gbt(g, whole, feats)
        s, outcome, _ = nested(g, fit, pred, tracks=ALL, name=name)
        dev = summarise(g, {t: outcome[t] for t in TRACKS})
        new = summarise(g, {t: outcome[t] for t in NEW_SONGS})
        out[name] = dict(all=s, dev=dev, new=new)
        print(line(name + ' dev', dev), flush=True)
        print(line(name + ' new', new), flush=True)
        print('   per track', {k[:8]: (v['matched'], v['labels'], v['extra']) for k, v in s['per_track'].items()}, flush=True)
    (GOAL / 'results_data.json').write_text(json.dumps(out, indent=1, default=float))


if __name__ == '__main__':
    main()
