#!/usr/bin/env python3
"""Does the song-relative stage (R3, 8 s) miss kicks when its history is empty or comes from another song?

Kicks per song: whole kick-stem truth where a stem exists, else the scored truth.
A kick counts as caught when a fire's emit time lies within 70 ms of it. Cutoffs
are the nested ones from the R3 and plain (T0) runs; stage two is refitted per
song on the other twelve exactly as in R3.
- Song start: kicks in the first 8 s after the song's first kick.
- After a break: kicks in the first 8 s after any kick-free stretch of 8 s or more.
- After another song: for every other song A, A's densest 8 s of candidates is
  spliced in to end 1 s before B's first kick (B's earlier candidates dropped),
  then B's first 8 s of kicks are scored. Worst case: a hard cut from A's groove
  into B's first kick.
Plain = the base score at its own nested cutoff (no history used).
"""
from __future__ import annotations

import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[3]))

import numpy as np  # noqa: E402

from tools.audio_analysis.eval.kick_goal_eval import GOAL, Goal, add_whole_song_truth, fires  # noqa: E402
from tools.audio_analysis.eval.kick_goal_featsets import lowbank_cache, profile_cache  # noqa: E402
from tools.audio_analysis.eval.kick_goal_selfsim import self_features  # noqa: E402
from tools.audio_analysis.eval.run_kick_goal_data import ALL  # noqa: E402
from tools.audio_analysis.eval.run_kick_goal_selfsim import fit2  # noqa: E402

WINDOW, EARLY, GAP, TOL = 8.0, 8.0, 8.0, .070
COLS = [0, 1, 2, 3, 4]


def kicks_of(r):
    if r['stem_kicks'] is not None:
        return np.asarray(r['stem_kicks'])
    return np.asarray(r['source']['truth'])


def caught(kicks, emit):
    return np.array([len(emit) > 0 and np.min(np.abs(emit - k)) <= TOL for k in kicks], dtype=bool)


def main():
    g = Goal(mode='strict')
    add_whole_song_truth(g)
    nested = np.load(GOAL / 'nested_f69.npz')
    r3_cut = json.loads((GOAL / 'results_selfsim2_f69_R3_self8_Q4_8and20.json').read_text())['R3_self8']['all']['cutoffs']
    t0_cut = {k: v[0] for k, v in json.loads((GOAL / 'results_rules_f69.json').read_text())['T0_refractory']['all']['chosen'].items()}
    shape = {t: np.hstack([profile_cache(g, t), lowbank_cache(g, t)]) for t in ALL}

    def sf(t, p, sh, lv, em):
        return self_features(p, sh, lv, em, WINDOW)[:, COLS]
    tot = {k: [0, 0] for k in ('start_plain', 'start_r3', 'break_plain', 'break_r3', 'after_plain', 'after_r3')}
    worst, per_song = [], {}
    for b in ALL:
        rb = g.records[b]
        train = {u: self_features(nested[f'{b}|{u}'], shape[u], g.records[u]['features'][:, 0], g.records[u]['emit_s'], WINDOW)
                 for u in ALL if u != b}
        m2 = fit2(g, train, list(train), COLS)
        sr, hop, em_b = rb['sample_rate'], rb['hop'], rb['emit_s']
        kicks = kicks_of(rb)
        if len(kicks) == 0:
            continue
        plain_emit = em_b[fires(nested[b], rb['available'], sr, hop, t0_cut[b])]
        s_alone = m2.predict_proba(sf(b, nested[b], shape[b], rb['features'][:, 0], em_b))[:, 1]
        r3_emit = em_b[fires(s_alone, rb['available'], sr, hop, r3_cut[b])]
        first = kicks[0]
        start = kicks[(kicks >= first) & (kicks < first + EARLY)]
        gaps = np.flatnonzero(np.diff(kicks) >= GAP)
        brk = np.concatenate([kicks[(kicks >= kicks[i + 1]) & (kicks < kicks[i + 1] + EARLY)] for i in gaps]) if len(gaps) else np.zeros(0)
        for name, ks in (('start', start), ('break', brk)):
            tot[f'{name}_plain'][0] += caught(ks, plain_emit).sum(); tot[f'{name}_plain'][1] += len(ks)
            tot[f'{name}_r3'][0] += caught(ks, r3_emit).sum(); tot[f'{name}_r3'][1] += len(ks)
        keep = em_b >= first - 1.0
        song_worst = (len(start) + 1, None)
        for a in ALL:
            if a == b:
                continue
            ra = g.records[a]
            ka = kicks_of(ra)
            if len(ka) == 0:
                continue
            counts = [np.sum((ka >= k) & (ka < k + WINDOW)) for k in ka]
            a0 = ka[int(np.argmax(counts))]
            seg = (ra['emit_s'] >= a0) & (ra['emit_s'] < a0 + WINDOW)
            shift = (first - 1.0) - (a0 + WINDOW)
            p = np.concatenate([nested[f'{b}|{a}'][seg], nested[b][keep]])
            sh = np.vstack([shape[a][seg], shape[b][keep]])
            lv = np.concatenate([ra['features'][seg, 0], rb['features'][keep, 0]])
            em = np.concatenate([ra['emit_s'][seg] + shift, em_b[keep]])
            s = m2.predict_proba(sf(b, p, sh, lv, em))[:, 1][seg.sum():]
            e = em_b[keep][fires(s, rb['available'][keep], sr, hop, r3_cut[b])]
            c = int(caught(start, e).sum())
            tot['after_r3'][0] += c; tot['after_r3'][1] += len(start)
            tot['after_plain'][0] += int(caught(start, plain_emit).sum()); tot['after_plain'][1] += len(start)
            if c < song_worst[0]:
                song_worst = (c, a)
        per_song[b] = dict(start_kicks=len(start), plain=int(caught(start, plain_emit).sum()),
                           r3_alone=int(caught(start, r3_emit).sum()), r3_worst_after=song_worst[0], worst_from=song_worst[1])
        print(b, per_song[b], flush=True)
    out = {k: dict(caught=int(v[0]), kicks=int(v[1]), recall=round(v[0] / max(1, v[1]), 3)) for k, v in tot.items()}
    print(json.dumps(out), flush=True)
    (GOAL / 'results_coldstart.json').write_text(json.dumps(dict(totals=out, per_song=per_song), indent=1, default=int))


if __name__ == '__main__':
    main()
