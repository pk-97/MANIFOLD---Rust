#!/usr/bin/env python3
"""Saves every nested prediction for a feature set, so cutoff rules and second stages run exactly nested without refits.

Usage: run_kick_goal_preds.py FEATS   (KICK_GOAL_TRUTH=v3 for truth v3; output gets a _v3 suffix;
KICK_GOAL_JOBS=n fits on n worker processes; KICK_GOAL_OUT=dir writes there instead of GOAL)

nested_{FEATS}.npz keys: 'o' = song o scored by the model fitted without o;
'o|u' = song u scored by the model fitted without o and u (the inner models).
Learner and truth as C1/C2 (gbt, whole-song kick-stem truth).
"""
from __future__ import annotations

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[3]))

import numpy as np  # noqa: E402

from tools.audio_analysis.eval.kick_goal_eval import OUT, SUFFIX, TRUTH, Goal, add_whole_song_truth, run_tasks  # noqa: E402
from tools.audio_analysis.eval.kick_goal_featsets import build  # noqa: E402
from tools.audio_analysis.eval.run_kick_goal_data import ALL, gbt  # noqa: E402

STATE = {}


def setup(feats):
    g = Goal(mode=TRUTH)
    add_whole_song_truth(g)
    build(g, feats)
    STATE['fit'], STATE['pred'] = gbt(g, True, feats)


def fit_pred(task):
    """(songs left out, songs to score) -> their predictions."""
    out, score = task
    m = STATE['fit']([u for u in ALL if u not in out])
    return [STATE['pred'](m, u) for u in score]


def main():
    feats = sys.argv[1]
    setup(feats)
    # Task order and key order are the serial loop's, so the npz is the same for any KICK_GOAL_JOBS.
    tasks, keys, planned, done = [], [], set(), {}
    for o in ALL:
        tasks.append(((o,), (o,)))
        keys.append([o])
        for u in ALL:
            if u != o and f'{u}|{o}' not in planned:
                tasks.append(((o, u), (u, o)))
                keys.append([f'{o}|{u}', f'{u}|{o}'])
                planned.update(keys[-1])
        done[len(tasks) - 1] = o
    out = {}
    for i, preds in enumerate(run_tasks(fit_pred, tasks, setup, (feats,))):
        out.update(zip(keys[i], preds))
        if i in done:
            print('outer', done[i], flush=True)
    np.savez(OUT / f'nested_{feats}{SUFFIX}.npz', **out)


if __name__ == '__main__':
    main()
