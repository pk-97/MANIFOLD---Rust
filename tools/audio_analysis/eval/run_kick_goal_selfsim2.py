#!/usr/bin/env python3
"""H-R2: widen the song-relative stage. Strict v2, same stacked nested protocol as H-R.

Usage: run_kick_goal_selfsim2.py BASE [CONFIG ...]   (default Q1 Q2 Q3)

Writes nested_{BASE}_{config}.npz in the nested_{BASE}.npz layout, so a
config can be stacked again by passing BASE_config.

Why: R2 (five self features, 20 s) lifted pooled F1 .585 -> .749 and Cold
165 -> 397/642, but short clips lost kicks (Apricots 14 -> 12, Tears 11 -> 9):
no history early in a clip. Predeclared configs (columns of one matrix):
- Q1: R2 + all 15 base features relative to the song's recent likely kicks (20 s).
- Q2: Q1 + the 4 s window's sim, lp_rel, lvl_rel, evidence (faster warm-up).
- Q3: Q2 + the 15 raw base features (fallback without history).
Second batch, declared after Q1 (all .713/.718) and R3 (self 8 s, all .784/.738):
- R3_self8: R3 rerun to save its predictions for a second stacked pass.
- Q4: the 8 s self features + the 20 s sim, lp_rel, lvl_rel, evidence. Pass vs R3: F1 >= .780.
Expected failure: more inputs overfit the 12 training songs; relative levels
repeat what lp_rel already says.
Pass vs R2 (all R .771 P .728, F1 .749): pooled F1 >= .779, and no song below
R1's recall by more than .10.
"""
from __future__ import annotations

import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[3]))

import numpy as np  # noqa: E402

from tools.audio_analysis.eval.kick_goal_eval import (  # noqa: E402
    GOAL, NEW_SONGS, TRACKS, TRUTH, Goal, add_whole_song_truth, choose, score, summarise)
from tools.audio_analysis.eval.kick_goal_featsets import lowbank_cache, profile_cache  # noqa: E402
from tools.audio_analysis.eval.kick_goal_selfsim import relative_levels, self_features  # noqa: E402
from tools.audio_analysis.eval.run_kick_goal_data import ALL, line  # noqa: E402
from tools.audio_analysis.eval.run_kick_goal_selfsim import fit2  # noqa: E402

SELF20, REL20, SELF4, RAW, SELF8 = (list(range(0, 5)), list(range(5, 20)), list(range(20, 24)), list(range(24, 39)),
                                   list(range(39, 44)))
CONFIGS = (('Q1_rel15', SELF20 + REL20), ('Q2_fast4', SELF20 + REL20 + SELF4), ('Q3_raw', SELF20 + REL20 + SELF4 + RAW),
           ('R3_self8', SELF8), ('Q4_8and20', SELF8 + SELF20[1:]))


def main():
    base = sys.argv[1]
    configs = [c for c in CONFIGS if c[0] in sys.argv[2:]] or CONFIGS[:3]
    g = Goal(mode=TRUTH)
    add_whole_song_truth(g)
    nested = np.load(GOAL / f'nested_{base}.npz')
    shape = {t: np.hstack([profile_cache(g, t), lowbank_cache(g, t)]) for t in g.records}
    cache = {}

    def matrix(t, key):
        if key not in cache:
            r = g.records[t]
            p, f, em = nested[key], r['features'], r['emit_s']
            cache[key] = np.hstack([self_features(p, shape[t], f[:, 0], em, 20.0), relative_levels(p, f, em, 20.0),
                                    self_features(p, shape[t], f[:, 0], em, 4.0)[:, 1:], f,
                                    self_features(p, shape[t], f[:, 0], em, 8.0)])
        return cache[key]
    out, saved = {}, {}
    for name, cols in configs:
        outcome, cuts = {}, {}
        for o in ALL:
            train = {u: matrix(u, f'{o}|{u}') for u in ALL if u != o}
            inner = {}
            for v in train:
                m = fit2(g, train, [u for u in train if u != v], cols)
                inner[v] = m.predict_proba(train[v][:, cols])[:, 1]
                saved[f'{name}|{o}|{v}'] = inner[v]
            th, _ = choose(g, inner)
            m = fit2(g, train, list(train), cols)
            saved[f'{name}|{o}'] = m.predict_proba(matrix(o, o)[:, cols])[:, 1]
            outcome[o] = score(g, o, saved[f'{name}|{o}'], th)
            cuts[o] = th
            print(name, 'outer', o, flush=True)
        s = summarise(g, outcome)
        s.update(name=name, cutoffs=cuts)
        out[name] = dict(all=s, dev=summarise(g, {t: outcome[t] for t in TRACKS}),
                         new=summarise(g, {t: outcome[t] for t in NEW_SONGS}))
        for k in ('all', 'dev', 'new'):
            print(line(f'{name} {k}', out[name][k]), flush=True)
        print('   per track', [f"{k[:8]} {v['matched']}/{v['labels']}+{v['extra']}" for k, v in s['per_track'].items()], flush=True)
    tag = '_'.join(c[0] for c in configs)
    (GOAL / f'results_selfsim2_{base}_{tag}.json').write_text(json.dumps(out, indent=1, default=float))
    for name, _ in configs:
        np.savez(GOAL / f'nested_{base}_{name}.npz', **{k[len(name) + 1:]: v for k, v in saved.items() if k.startswith(name + '|')})


if __name__ == '__main__':
    main()
