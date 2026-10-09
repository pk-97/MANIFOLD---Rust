#!/usr/bin/env python3
"""Step 1 of the 90/90 goal: where we stand on truth v2.

Frozen night models (trained on v1 labels) are re-cut and scored on v2 dev
truth. Two retrained learners on the same frozen 15 features (weighted logistic,
small gradient-boosted trees) are scored nested on dev and once on the four new
songs. No new features yet.
"""
from __future__ import annotations

import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[3]))

import numpy as np  # noqa: E402
from scipy.special import expit  # noqa: E402
from sklearn.ensemble import HistGradientBoostingClassifier  # noqa: E402
from sklearn.linear_model import LogisticRegression  # noqa: E402

from tools.audio_analysis.eval.kick_goal_eval import (  # noqa: E402
    GOAL, TRACKS, Goal, choose, heldout, nested, score, summarise, training_set)
from tools.audio_analysis.eval.kick_night_common import baseline_logit, h18_logit  # noqa: E402


def frozen(g, logit_fn, name):
    d = g.night
    outcome, cuts = {}, {}
    for o in TRACKS:
        th, _ = choose(g, {u: expit(logit_fn(d, o, u)) for u in TRACKS if u != o})
        cuts[o] = th
        outcome[o] = score(g, o, expit(logit_fn(d, o, o)), th)
    s = summarise(g, outcome)
    s.update(name=name, cutoffs=cuts)
    return s


def lr_fit(g):
    def fit(tracks):
        x, y, w = training_set(g, tracks)
        mu, sd = x.mean(0), x.std(0) + 1e-9
        m = LogisticRegression(C=1.0, max_iter=2000).fit((x - mu) / sd, y, sample_weight=w * len(y))
        return (m, mu, sd)
    return fit


def lr_predict(g):
    return lambda model, t: model[0].predict_proba((g.records[t]['features'] - model[1]) / model[2])[:, 1]


def gbt_fit(g):
    def fit(tracks):
        x, y, w = training_set(g, tracks)
        return HistGradientBoostingClassifier(max_iter=200, learning_rate=.05, max_leaf_nodes=15, min_samples_leaf=40,
                                              l2_regularization=1.0, random_state=0).fit(x, y, sample_weight=w * len(y))
    return fit


def gbt_predict(g):
    return lambda model, t: model.predict_proba(g.records[t]['features'])[:, 1]


def show(s):
    t = s['tol']
    print(f"{s['name']:22s} 70ms {t['70']['matched']}/{s['labels']}+{t['70']['extra']}  R {s['recall']} P {s['precision']} "
          f"minR {s['min_track_recall']} tail {s['tail_fires']} core {s['core_fires']} | 50ms {t['50']['matched']}+{t['50']['extra']} "
          f"| noBG {s['without_bad_guy']} | delay {s['delay_ms']} | target {s['meets_target']}", flush=True)
    print('   ', {k[:8]: (v['matched'], v['labels'], v['extra'], v['tail']) for k, v in s['per_track'].items()}, flush=True)


def main():
    g = Goal()
    out = {}
    for name, fn in (('frozen_baseline', baseline_logit), ('frozen_h18', lambda d, o, t: h18_logit(d, o, t))):
        out[name] = frozen(g, fn, name)
        show(out[name])
    for name, fit, pred in (('lr15', lr_fit(g), lr_predict(g)), ('gbt15', gbt_fit(g), gbt_predict(g))):
        s, outcome, cache = nested(g, fit, pred, name=name)
        out[name] = s
        show(s)
        h, _ = heldout(g, fit, pred, cache, name=name + '_new')
        out[name + '_new'] = h
        show(h)
    (GOAL / 'results_rescore.json').write_text(json.dumps(out, indent=1, default=float))


if __name__ == '__main__':
    main()
