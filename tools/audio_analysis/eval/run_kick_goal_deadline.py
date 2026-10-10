#!/usr/bin/env python3
"""Does 10 ms more low-band evidence (a 50 ms window, from 40) raise the ranking ceiling? Oracle A/B, strict v2.

Why (Fable consult): kick-vs-tom pitch settling and Cold's soft sub swell may
arrive after the 40 ms deadline. Emission moves 2 hops later (+10.7 ms, median
delay about 55 ms, p90 about 67 ms), still inside the 70 ms match window.
The 15 frozen features stay at 40 ms (their source files key the night feature
cache, so they are never edited); only the 16 low filter-bank rises are taken
over the longer window. Same candidates, same learner (gbt, whole-song truth),
outer models only, then per-song oracle cutoffs (run_kick_goal_frontier).
Pass: the 50 ms ceiling (all songs, no floor) is >= 0.02 above 40 ms; then the
frozen features get a parameterised 50 ms copy and every family is rebuilt.
"""
from __future__ import annotations

import json
import math
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[3]))

import numpy as np  # noqa: E402

from tools.audio_analysis.eval.kick_goal_eval import (  # noqa: E402
    GOAL, NEW_SONGS, TRACKS, Goal, add_whole_song_truth)
from tools.audio_analysis.eval.kick_goal_featsets import lowbank_cache  # noqa: E402
from tools.audio_analysis.eval.kick_goal_labels import DEV_STEMS, NEW  # noqa: E402
from tools.audio_analysis.eval.kick_goal_lowbank import band_envelopes, lowbank_features  # noqa: E402
from tools.audio_analysis.eval.run_kick_goal_data import ALL, gbt  # noqa: E402
from tools.audio_analysis.eval.run_kick_goal_frontier import frontier, pooled, sweep  # noqa: E402
from tools.audio_analysis.eval.run_kick_goal_tail_component import mix_audio  # noqa: E402


def ceiling(g, feats):
    fit, pred = gbt(g, True, feats)
    fr, labels = {}, {}
    for o in ALL:
        rows, n = sweep(g, o, pred(fit([u for u in ALL if u != o]), o))
        fr[o], labels[o] = frontier(rows), n
    return dict(all=pooled(fr, labels, 0), all_floor=pooled(fr, labels, .8),
                dev=pooled({t: fr[t] for t in TRACKS}, labels, 0), new=pooled({t: fr[t] for t in NEW_SONGS}, labels, 0))


def low_env(g, t):
    path = GOAL / f'env_{t}.npy'
    if not path.exists():
        np.save(path, band_envelopes(mix_audio(g, t), g.records[t]['sample_rate']).astype(np.float32))
    return np.load(path)


def main():
    g = Goal(mode='strict')
    add_whole_song_truth(g)
    for t, r in g.records.items():
        r['f31b'] = np.hstack([r['features'], lowbank_cache(g, t)])
    res = {'40': ceiling(g, 'f31b')}
    print('40 ms', res['40'], flush=True)
    for t, r in g.records.items():
        sr, hop = r['sample_rate'], r['hop']
        extra = math.ceil(.050 * sr / hop) - math.ceil(.040 * sr / hop)
        env = low_env(g, t)
        keep = (r['available'] + extra + 1) * hop / sr < len(env) * .001
        r['candidates'], r['available'], r['features'] = r['candidates'][keep], r['available'][keep] + extra, r['features'][keep]
        for k in ('train_mask', 'train_y'):
            r.pop(k, None)
        kick = DEV_STEMS[t]['kick'] if t in DEV_STEMS else NEW[t]['kick'] if t in NEW else None
        g._finish(r, kick)
        r['f31b'] = np.hstack([r['features'], lowbank_features(env, r['onset_s'], r['emit_s'])])
    add_whole_song_truth(g)
    res['50'] = ceiling(g, 'f31b')
    print('50 ms', res['50'], flush=True)
    (GOAL / 'results_deadline.json').write_text(json.dumps(res, indent=1, default=float))


if __name__ == '__main__':
    main()
