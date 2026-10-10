#!/usr/bin/env python3
"""Learning curve: does the per-song oracle ceiling still climb with more training songs?

Usage: run_kick_goal_learning.py FEATS   (KICK_GOAL_TRUTH=v3 for truth v3)

For every held-out song, fit on k random other songs (k = 4, 8: two seeded
draws each; k = 12: all others) and sweep the held-out song's cutoff (a coarser
grid than run_kick_goal_frontier). Reports the pooled balanced oracle point per
k. Pass bar, fixed before the run: ceiling(12) - ceiling(8) >= .015 means more
songs are worth building; < .005 means more training songs are not the lever.
"""
from __future__ import annotations

import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[3]))

import numpy as np  # noqa: E402

from tools.audio_analysis.eval.kick_goal_eval import GOAL, SUFFIX, TRUTH, Goal, add_whole_song_truth, fast_counts  # noqa: E402
from tools.audio_analysis.eval.kick_goal_featsets import build  # noqa: E402
from tools.audio_analysis.eval.run_kick_goal_data import ALL, gbt  # noqa: E402
from tools.audio_analysis.eval.run_kick_goal_frontier import frontier, pooled  # noqa: E402

DRAWS = {4: 2, 8: 2, 12: 1}


def coarse_sweep(g, t, p):
    ths = np.unique(np.quantile(p, np.concatenate([np.linspace(0, .9, 31), np.linspace(.9, 1, 121)])))
    rows = []
    for th in ths:
        m, e, n = fast_counts(g, t, p, th)
        rows.append((float(th), m, e))
    return rows, n


def main():
    feats = sys.argv[1]
    g = Goal(mode=TRUTH)
    add_whole_song_truth(g)
    build(g, feats)
    fit, pred = gbt(g, True, feats)
    rng = np.random.default_rng(0)
    out = {}
    for k, draws in DRAWS.items():
        for d in range(draws):
            fr, labels = {}, {}
            for o in ALL:
                others = [u for u in ALL if u != o]
                train = others if k >= len(others) else list(rng.choice(others, k, replace=False))
                rows, n = coarse_sweep(g, o, pred(fit(train), o))
                fr[o], labels[o] = frontier(rows), n
            res = pooled(fr, labels, 0)['best_balanced']
            out[f'k{k}_d{d}'] = res
            print(f'k {k} draw {d}', res, flush=True)
    (GOAL / f'learning_{feats}{SUFFIX}.json').write_text(json.dumps(out, indent=1, default=float))


if __name__ == '__main__':
    main()
