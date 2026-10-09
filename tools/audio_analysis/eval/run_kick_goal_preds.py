#!/usr/bin/env python3
"""Saves every nested prediction for a feature set, so cutoff rules and second stages run exactly nested without refits.

Usage: run_kick_goal_preds.py FEATS

nested_{FEATS}.npz keys: 'o' = song o scored by the model fitted without o;
'o|u' = song u scored by the model fitted without o and u (the inner models).
Learner and truth as C1/C2 (gbt, whole-song kick-stem truth).
"""
from __future__ import annotations

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[3]))

import numpy as np  # noqa: E402

from tools.audio_analysis.eval.kick_goal_eval import GOAL, Goal, add_whole_song_truth  # noqa: E402
from tools.audio_analysis.eval.kick_goal_featsets import build  # noqa: E402
from tools.audio_analysis.eval.run_kick_goal_data import ALL, gbt  # noqa: E402


def main():
    feats = sys.argv[1]
    g = Goal(mode='strict')
    add_whole_song_truth(g)
    build(g, feats)
    fit, pred = gbt(g, True, feats)
    out = {}
    for o in ALL:
        out[o] = pred(fit([u for u in ALL if u != o]), o)
        for u in ALL:
            if u != o and f'{u}|{o}' not in out:
                m = fit([v for v in ALL if v not in (o, u)])
                out[f'{o}|{u}'], out[f'{u}|{o}'] = pred(m, u), pred(m, o)
        print('outer', o, flush=True)
    np.savez(GOAL / f'nested_{feats}.npz', **out)


if __name__ == '__main__':
    main()
