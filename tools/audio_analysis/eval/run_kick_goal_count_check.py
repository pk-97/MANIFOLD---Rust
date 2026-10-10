#!/usr/bin/env python3
"""Proves fast_fires/fast_counts equal fires()/counts(score()) on saved nested predictions.

Usage: run_kick_goal_count_check.py NPZ ...   (relative to GOAL; KICK_GOAL_JOBS spreads the work)

For every song: each threshold of run_kick_goal_frontier.sweep's grid on the song's
outer predictions. For every outer song: each threshold choose() visits on its inner
predictions, plus choose's result against the reference choose. Fire indices and
matched/extra/labels at 35, 50 and 70 ms must be identical. Exits 1 on any mismatch.
"""
from __future__ import annotations

import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[3]))

import numpy as np  # noqa: E402
from scipy.special import expit, logit  # noqa: E402

from tools.audio_analysis.eval.kick_goal_eval import (  # noqa: E402
    GOAL, TRUTH, Goal, choose, counts, fast_counts, fast_fires, run_tasks, score)
from tools.audio_analysis.eval.run_kick_goal_data import ALL  # noqa: E402

MS = ('35', '50', '70')
STATE = {}


def setup():
    STATE['g'] = Goal(mode=TRUTH)


def compare(t, p, th):
    """Mismatch descriptions for one (song, predictions, threshold)."""
    g = STATE['g']
    idx, passages = score(g, t, p, th)
    bad = [] if np.array_equal(idx, fast_fires(g, t, p, th)) else ['fires']
    for ms in MS:
        if counts(passages, ms) != fast_counts(g, t, p, th, ms):
            bad.append(f'{ms} ms {counts(passages, ms)} != {fast_counts(g, t, p, th, ms)}')
    return [f'{t} th={th!r}: {b}' for b in bad]


def reference_choose(preds, grid=None, seen=None):
    """choose() as it was before fast_counts; records every threshold it visits."""
    g = STATE['g']
    allp = np.concatenate(list(preds.values()))
    if grid is None:
        grid = np.unique(expit(np.quantile(logit(np.clip(allp, 1e-9, 1 - 1e-9)), np.linspace(.5, .9995, 60))))
    best = None
    for th in grid:
        m = e = n = 0
        for t, p in preds.items():
            a, b, c = counts(score(g, t, p, th)[1])
            m, e, n = m + a, e + b, n + c
            seen.append((t, th))
        f1 = 2 * m / max(1, 2 * m + e + (n - m))
        if best is None or f1 > best[1] + 1e-12:
            best = (float(th), f1)
    if grid is not None and len(grid) > 30:
        i = int(np.searchsorted(grid, best[0]))
        lo, hi = grid[max(0, i - 1)], grid[min(len(grid) - 1, i + 1)]
        return reference_choose(preds, np.linspace(lo, hi, 12), seen) if hi > lo else best
    return best


def check(task):
    name, kind, s = task
    z = np.load(GOAL / name)
    bad, n = [], 0
    if kind == 'sweep':
        p = z[s]
        for th in np.unique(np.quantile(p, np.concatenate([np.linspace(0, .9, 91), np.linspace(.9, 1, 401)]))):
            bad += compare(s, p, th)
            n += 1
    else:
        inner = {v: z[f'{s}|{v}'] for v in ALL if v != s}
        seen = []
        ref = reference_choose(inner, seen=seen)
        if ref != choose(STATE['g'], inner):
            bad.append(f'choose outer {s}: {ref} != {choose(STATE["g"], inner)}')
        for t, th in seen:
            bad += compare(t, inner[t], th)
            n += 1
    return name, kind, s, n, bad


def main():
    start = time.time()
    setup()
    tasks = [(name, kind, s) for name in sys.argv[1:] for kind in ('sweep', 'choose') for s in ALL]
    total, failed = 0, []
    for name, kind, s, n, bad in run_tasks(check, tasks, setup):
        total += n
        failed += bad
        print(f'{name} {kind} {s}: {n} thresholds, {len(bad)} mismatches', flush=True)
    for b in failed[:50]:
        print('MISMATCH', b)
    print(f'{total} (song, threshold) checks, {len(failed)} mismatches, {time.time() - start:.0f} s', flush=True)
    sys.exit(1 if failed else 0)


if __name__ == '__main__':
    main()
