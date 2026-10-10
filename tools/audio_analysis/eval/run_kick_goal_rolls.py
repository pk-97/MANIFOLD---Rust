#!/usr/bin/env python3
"""Tune the pitch-restart kick-note labeller (kick_goal_rolls) against the reviewed loose labels of the four own-stem dev songs.

Truth: reviewed passage labels in loose mode (kept kick-stem labels plus the
kick_tail removals Peter ruled are kicks), stem time = mix time - lag. Inside the
reviewed passages: recall of all stem labels and of the restored rolls (+-20 ms),
extras = notes with no reviewed label (any class) within 20 ms, and notes closer
than 40 ms to the previous one. Pick the config with the most restored rolls at
no more extras than the current fresh-onset rule plus 10%.
"""
from __future__ import annotations

import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[3]))

import numpy as np  # noqa: E402

from tools.audio_analysis.eval.kick_goal_eval import GOAL, Goal  # noqa: E402
from tools.audio_analysis.eval.kick_goal_labels import DEV_STEMS, fresh_onsets, kick_env_db, load  # noqa: E402
from tools.audio_analysis.eval.kick_goal_rolls import kick_notes  # noqa: E402

GRID = [(j, h) for j in (1.8, 2.2, 3.0) for h in (90.0, 110.0, 150.0)]
TOL = .020


def main():
    g = Goal(mode='loose', with_new=False)
    v2 = json.loads((GOAL / 'labels_v2.json').read_text())['dev']
    res = {}
    for t in DEV_STEMS:
        r = g.records[t]
        x = load(DEV_STEMS[t]["kick"], r["sample_rate"])
        e = kick_env_db(x, r["sample_rate"])
        lag = r['lag']
        spans = [(p['start_s'], p['end_s']) for p in r['source']['passages']]
        rows = v2[t]
        stem_lab = np.array([x['t'] for x in rows['kept'] if x['cls'] == 'kick_stem'] +
                            [x['t'] for x in rows['removed'] if x['cls'] == 'kick_tail'])
        rolls = np.array([x['t'] for x in rows['removed'] if x['cls'] == 'kick_tail'])
        any_lab = np.array([x['t'] for x in rows['kept'] + rows['removed']])

        def judge(notes):
            notes = notes + lag
            inside = notes[[any(a <= x < b for a, b in spans) for x in notes]]
            hit = lambda labs: int(sum(np.any(np.abs(inside - x) <= TOL) for x in labs))
            extra = int(sum(not np.any(np.abs(any_lab - x) <= TOL) for x in inside))
            close = int(np.sum(np.diff(np.sort(notes)) < .040))
            return dict(stem_hit=hit(stem_lab), stem=len(stem_lab), roll_hit=hit(rolls), rolls=len(rolls), extra=extra, close=close)
        res[t] = {'fresh60': judge(fresh_onsets(e))}
        for j, h in GRID:
            res[t][f"j{j:g}_h{h:g}"] = judge(kick_notes(x, r["sample_rate"], e, jump=j, min_hz=h))
        print(t, json.dumps(res[t]), flush=True)
    tot = {}
    for k in ["fresh60"] + [f"j{j:g}_h{h:g}" for j, h in GRID]:
        tot[k] = {f: sum(res[t][k][f] for t in res) for f in ('stem_hit', 'stem', 'roll_hit', 'rolls', 'extra', 'close')}
        print('TOTAL', k, tot[k], flush=True)
    (GOAL / 'results_rolls_tuning.json').write_text(json.dumps(dict(per_song=res, total=tot), indent=1))


if __name__ == '__main__':
    main()
