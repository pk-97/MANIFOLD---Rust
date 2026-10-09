#!/usr/bin/env python3
"""Does a 50 ms evidence window (from 40) raise the ranking ceiling? Oracle frontier A/B, strict v2.

Why (Fable consult): kick-vs-tom pitch settling and Cold's soft sub swell may
arrive after the 40 ms deadline. Emission moves 2 hops later (+10.7 ms, median
delay about 55 ms, p90 about 67 ms), still inside the 70 ms match window.
Same candidates, same learner (gbt, whole-song truth), features = the 15 base +
the 16 low filter-bank rises, computed at each deadline. Outer models only, then
per-song oracle cutoffs (run_kick_goal_frontier).
Pass: the 50 ms ceiling (all songs, no floor) is >= 0.02 above 40 ms; then every
feature family is rebuilt at 50 ms.
Check first: the 40 ms recomputation reproduces the cached features of one dev
song and one new song.
"""
from __future__ import annotations

import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[3]))

import numpy as np  # noqa: E402

from tools.audio_analysis.eval.kick_fusion_bandwise import fusion_features  # noqa: E402
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


def at_deadline(g, t, deadline_s):
    path = GOAL / f'dl{int(deadline_s * 1000)}_{t}.npz'
    if not path.exists():
        r = g.records[t]
        x = mix_audio(g, t)
        c, a, f, hop = fusion_features(x, r['sample_rate'], deadline_s)
        assert hop == r['hop']
        np.savez(path, candidates=c, available=a, features=f[:, :15], env=band_envelopes(x, r['sample_rate']))
    z = np.load(path)
    return z['candidates'], z['available'], z['features'], z['env']


def main():
    g = Goal(mode='strict')
    add_whole_song_truth(g)
    for t in ('miracle', 'pattern'):
        c, a, f, _ = at_deadline(g, t, .040)
        r = g.records[t]
        keep = np.isin(r['candidates'], c)
        print('check 40 ms', t, 'candidates kept', int(keep.sum()), '/', len(keep),
              'max feature diff', float(np.abs(r['features'][keep] - f[np.isin(c, r['candidates'])]).max()), flush=True)
    for t, r in g.records.items():
        r['f31b'] = np.hstack([r['features'], lowbank_cache(g, t)])
    res = {'40': ceiling(g, 'f31b')}
    print('40 ms', res['40'], flush=True)
    for t, r in g.records.items():
        c, a, f, env = at_deadline(g, t, .050)
        keep = np.isin(r['candidates'], c)
        sel = np.isin(c, r['candidates'])
        r['candidates'], r['available'], r['features'] = r['candidates'][keep], a[sel], f[sel]
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
