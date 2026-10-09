#!/usr/bin/env python3
"""Per-song oracle frontier on saved held-out scores (a ranking measure free of cutoff noise).

Usage: run_kick_goal_frontier_saved.py NPZ [PREFIX ...]

NPZ holds one array per song keyed '{PREFIX}|{song}' (selfsim2_outer_*.npz) or
'{song}' (outer_*.npz, nested_*.npz); each PREFIX is reported separately
(none = bare song keys).
"""
from __future__ import annotations

import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[3]))

import numpy as np  # noqa: E402

from tools.audio_analysis.eval.kick_goal_eval import GOAL, MORE_SONGS, NEW_SONGS, TRACKS, TRUTH, Goal  # noqa: E402
from tools.audio_analysis.eval.run_kick_goal_data import ALL  # noqa: E402
from tools.audio_analysis.eval.run_kick_goal_frontier import frontier, pooled, sweep  # noqa: E402


def main():
    z = np.load(GOAL / sys.argv[1])
    prefixes = sys.argv[2:] or ['']
    g = Goal(mode=TRUTH)
    res = {}
    for pre in prefixes:
        fr, labels = {}, {}
        for t in ALL:
            rows, n = sweep(g, t, z[f'{pre}|{t}' if pre else t])
            fr[t], labels[t] = frontier(rows), n
        res[pre or 'bare'] = dict(all=pooled(fr, labels, 0), all_floor=pooled(fr, labels, .8),
                                  dev=pooled({t: fr[t] for t in TRACKS}, labels, 0),
                                  new=pooled({t: fr[t] for t in NEW_SONGS}, labels, 0))
        if MORE_SONGS:
            res[pre or 'bare']['orig13'] = pooled({t: fr[t] for t in TRACKS + NEW_SONGS}, labels, 0)
        print(pre or 'bare', {k: v.get('best_balanced', v) for k, v in res[pre or 'bare'].items()}, flush=True)
    (GOAL / f'frontier_saved_{Path(sys.argv[1]).stem}.json').write_text(json.dumps(res, indent=1, default=float))


if __name__ == '__main__':
    main()
