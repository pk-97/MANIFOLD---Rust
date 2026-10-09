#!/usr/bin/env python3
"""Per-song oracle frontier on v2 truth: could any cutoff rule reach 90/90?

Usage: run_kick_goal_frontier.py FEATS  (features, f30, f15n, f31, f21; f53, f47, f68, f69 once prof caches exist)

Diagnostic only. Fits the 13 outer whole-song models (gbt, as C1), saves each
song's held-out probabilities to outer_{FEATS}.npz for cheap cutoff-rule
experiments, then sweeps every song's cutoff with the labels. Reports each song's
recall ceiling, the fewest extras for 80% and 90% recall, and an exact search
over per-song cutoffs for the goal (pooled R and P >= .90, every song >= .80),
dev and new songs together. If even per-song oracle cutoffs miss, no cutoff rule
(song-relative, adaptive) can reach the goal on these scores.
"""
from __future__ import annotations

import json
import math
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[3]))

import numpy as np  # noqa: E402

from tools.audio_analysis.eval.kick_goal_eval import (  # noqa: E402
    GOAL, NEW_SONGS, TRACKS, Goal, add_whole_song_truth, counts, score)
from tools.audio_analysis.eval.kick_goal_featsets import build  # noqa: E402
from tools.audio_analysis.eval.run_kick_goal_data import ALL, gbt  # noqa: E402


def sweep(g, t, p):
    ths = np.unique(np.quantile(p, np.concatenate([np.linspace(0, .9, 91), np.linspace(.9, 1, 401)])))
    rows = []
    for th in ths:
        m, e, n = counts(score(g, t, p, th)[1])
        rows.append((float(th), m, e))
    return rows, n


def frontier(rows):
    best = {}
    for _, m, e in rows:
        best[e] = max(best.get(e, -1), m)
    pts, top = [], -1
    for e in sorted(best):
        if best[e] > top:
            pts.append((best[e], e))
            top = best[e]
    return pts


def pooled(fr, labels, floor):
    cap = 4000
    dp = {0: 0}
    for t in fr:
        pts = [(m, e) for m, e in fr[t] if m >= math.ceil(floor * labels[t])]
        if not pts:
            return dict(feasible=False, reason=f'{t} cannot reach {floor:.0%} recall')
        nxt = {}
        for e0, m0 in dp.items():
            for m, e in pts:
                if e0 + e <= cap and nxt.get(e0 + e, -1) < m0 + m:
                    nxt[e0 + e] = m0 + m
        dp = nxt
    n = sum(labels[t] for t in fr)
    ok = [(m, e) for e, m in dp.items() if m >= .9 * n and m / max(1, m + e) >= .9]
    bal = max(dp.items(), key=lambda kv: min(kv[1] / n, kv[1] / max(1, kv[1] + kv[0])))
    return dict(feasible=bool(ok), labels=n, best_balanced=dict(matched=bal[1], extra=bal[0],
                recall=round(bal[1] / n, 3), precision=round(bal[1] / max(1, bal[1] + bal[0]), 3)))


def main():
    feats = sys.argv[1]
    g = Goal(mode='strict')
    add_whole_song_truth(g)
    build(g, feats)
    fit, pred = gbt(g, True, feats)
    probs = {o: pred(fit([u for u in ALL if u != o]), o) for o in ALL}
    np.savez(GOAL / f'outer_{feats}.npz', **probs)
    fr, labels, out = {}, {}, {}
    for t in ALL:
        rows, n = sweep(g, t, probs[t])
        fr[t], labels[t] = frontier(rows), n
        ceiling = max(m for m, _ in fr[t])

        def fewest(r):
            ok = [e for m, e in fr[t] if m >= math.ceil(r * n)]
            return min(ok) if ok else None
        at90p = [m for m, e in fr[t] if m / max(1, m + e) >= .9]
        out[t] = dict(labels=n, ceiling=ceiling, extras_for_80=fewest(.8), extras_for_90=fewest(.9),
                      best_recall_at_90_precision=round(max(at90p, default=0) / max(1, n), 3))
        print(t[:12], out[t], flush=True)
    res = dict(per_song=out, all=pooled(fr, labels, .8), all_no_floor=pooled(fr, labels, 0), dev=pooled({t: fr[t] for t in TRACKS}, labels, .8),
               new=pooled({t: fr[t] for t in NEW_SONGS}, labels, .8))
    for k in ('all', 'all_no_floor', 'dev', 'new'):
        print(k, res[k], flush=True)
    (GOAL / f'frontier_{feats}.json').write_text(json.dumps(res, indent=1, default=float))


if __name__ == '__main__':
    main()
