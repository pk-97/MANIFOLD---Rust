#!/usr/bin/env python3
"""H-R: do song-relative cues (each hit vs the song's own recent likely kicks) lift within-song ranking? Strict v2.

Usage: run_kick_goal_selfsim.py BASE   (a feature set with nested_{BASE}.npz saved)

Why: the per-song oracle cutoff on the best scores tops out near 83/83, so no
cutoff rule reaches 90/90; within-song ranking must improve. Cold's kicks all
sound alike but sit low on the cross-song scale; Burn's toms differ in shape
from Burn's kicks.
Protocol (stacked, exactly nested for the outer song): for outer o, training
song u gets self features from base 'o|u' (fitted without o and u); o gets them
from base 'o'. Stage two is fitted on u != o; its cutoff maximises pooled F1 over
inner stage-two models fitted without o and u' (their training features came from
base models that saw u': a second-order leak into the cutoff only).
Predeclared configs:
- R1 lp only (control: should match the base nested result).
- R2 all five self features, 20 s window.
- R3 all five, 8 s window.
Expected failure: weights p ** 8 pick the song's loud non-kicks as often as its
kicks, so the template is a blur and sim adds nothing.
Pass: R2 or R3 beats R1 by >= 0.05 pooled F1 over all 13 songs, with no song
losing more than 0.10 recall.
"""
from __future__ import annotations

import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[3]))

import numpy as np  # noqa: E402
from sklearn.ensemble import HistGradientBoostingClassifier  # noqa: E402

from tools.audio_analysis.eval.kick_goal_eval import (  # noqa: E402
    GOAL, NEW_SONGS, TRACKS, Goal, add_whole_song_truth, choose, score, summarise)
from tools.audio_analysis.eval.kick_goal_featsets import lowbank_cache, profile_cache  # noqa: E402
from tools.audio_analysis.eval.kick_goal_selfsim import self_features  # noqa: E402
from tools.audio_analysis.eval.run_kick_goal_data import ALL, line  # noqa: E402

CONFIGS = (('R1_lp', (0,), 20.0), ('R2_self20', (0, 1, 2, 3, 4), 20.0), ('R3_self8', (0, 1, 2, 3, 4), 8.0))


def fit2(g, feats_by_song, songs, cols):
    xs, ys, ws = [], [], []
    for u in songs:
        r = g.records[u]
        m = r['train_mask']
        y = r['train_y'][m]
        xs.append(feats_by_song[u][m][:, cols]); ys.append(y)
        ws.append(np.where(y == 1, .5 / max(1, y.sum()), .5 / max(1, (1 - y).sum())))
    x, y, w = np.concatenate(xs), np.concatenate(ys), np.concatenate(ws)
    return HistGradientBoostingClassifier(max_iter=150, learning_rate=.05, max_leaf_nodes=7, min_samples_leaf=80,
                                          l2_regularization=1.0, random_state=0).fit(x, y, sample_weight=w * len(y))


def main():
    base = sys.argv[1]
    g = Goal(mode='strict')
    add_whole_song_truth(g)
    nested = np.load(GOAL / f'nested_{base}.npz')
    shape = {}
    for t, r in g.records.items():
        parts = [profile_cache(g, t)]
        if (GOAL / f'low_{t}.npy').exists() or base == 'f69':
            parts.append(lowbank_cache(g, t))
        shape[t] = np.hstack(parts)

    def sf(t, key, window):
        r = g.records[t]
        return self_features(nested[key], shape[t], r['features'][:, 0], r['emit_s'], window)
    out = {}
    for name, cols, window in CONFIGS:
        cols = list(cols)
        outcome, cuts = {}, {}
        for o in ALL:
            train = {u: sf(u, f'{o}|{u}', window) for u in ALL if u != o}
            inner = {}
            for v in train:
                m = fit2(g, train, [u for u in train if u != v], cols)
                inner[v] = m.predict_proba(train[v][:, cols])[:, 1]
            th, _ = choose(g, inner)
            m = fit2(g, train, list(train), cols)
            outcome[o] = score(g, o, m.predict_proba(sf(o, o, window)[:, cols])[:, 1], th)
            cuts[o] = th
            print(name, 'outer', o, flush=True)
        s = summarise(g, outcome)
        s.update(name=name, cutoffs=cuts)
        dev = summarise(g, {t: outcome[t] for t in TRACKS})
        new = summarise(g, {t: outcome[t] for t in NEW_SONGS})
        out[name] = dict(all=s, dev=dev, new=new)
        print(line(name + ' all', s), flush=True)
        print(line(name + ' dev', dev), flush=True)
        print(line(name + ' new', new), flush=True)
        print('   per track', [f"{k[:8]} {v['matched']}/{v['labels']}+{v['extra']}" for k, v in s['per_track'].items()], flush=True)
    (GOAL / f'results_selfsim_{base}.json').write_text(json.dumps(out, indent=1, default=float))


if __name__ == '__main__':
    main()
