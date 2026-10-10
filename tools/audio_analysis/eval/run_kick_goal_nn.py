#!/usr/bin/env python3
"""Hybrid kick network runner (kick_goal_nn).

Usage: KICK_GOAL_MORE=1 KICK_GOAL_TRIGGER=1 KICK_GOAL_TRUTH=v3 run_kick_goal_nn.py prep|look|outer

prep  builds every song's spectrum cache (KICK_GOAL_JOBS processes).
look  renders labelled kicks and the stacked detector's extra fires on one song
      (default corrosion) to GOAL/nn_look_{song}.png, top row kicks.
outer fits one net per held-out song on the other songs and saves its
      predictions for that song to nn_outer{SUFFIX}.npz, then prints each song's
      best precision at 90% recall for the net alone and for bare f69.
"""
from __future__ import annotations

import json
import sys
import time
from concurrent.futures import ProcessPoolExecutor
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[3]))

import numpy as np  # noqa: E402

from tools.audio_analysis.eval.kick_goal_eval import GOAL, OUT, SUFFIX, TRUTH, Goal, add_whole_song_truth, fast_counts, jobs, score  # noqa: E402
from tools.audio_analysis.eval.kick_goal_nn import Song, predict, spectrum_cache, train  # noqa: E402
from tools.audio_analysis.eval.run_kick_goal_data import ALL  # noqa: E402

STATE = {}


def _init():
    STATE['g'] = Goal(mode=TRUTH)


def _prep(t):
    spectrum_cache(STATE['g'], t)
    return t


def best_p(g, t, p, rmin=.9):
    out = 0.0
    for th in np.quantile(p, np.linspace(.5, .999, 300)):
        m, e, n = fast_counts(g, t, p, th)
        if n and m / n >= rmin:
            out = max(out, m / max(1, m + e))
    return out


def look(g, t):
    from PIL import Image
    r = g.records[t]
    song = Song(g, t)
    z = np.load(GOAL / f'nested_f69{SUFFIX}_R3_self8.npz')
    cut = json.loads((GOAL / f'results_selfsim2_f69{SUFFIX}_R3_self8.json').read_text())['R3_self8']['all']['cutoffs'][t]
    idx, passages = score(g, t, z[t], cut)
    extra = {round(x, 6) for p in passages for x in p['accuracy_by_tolerance_ms']['70']['extra_times_s']}
    fired_extra = [i for i in idx if round(float(r['emit_s'][i]), 6) in extra]
    kicks = np.flatnonzero(r['train_mask'] & (r['train_y'] == 1))
    rng = np.random.default_rng(0)
    rows = [rng.choice(kicks, 8, replace=False), rng.choice(fired_extra, 8, replace=False)]
    tiles = []
    for row in rows:
        x = song.slices(np.asarray(row))[:, 0].numpy()  # shape channel, (8, 64, SLICE)
        img = np.clip((x + 3.0) / 3.0, 0, 1)[:, ::-1, :]  # 60 dB range, low frequencies at the bottom
        tiles.append(np.concatenate([np.pad(im, ((2, 2), (2, 2)), constant_values=1) for im in img], axis=1))
    im = (255 * np.concatenate(tiles, axis=0)).astype(np.uint8)
    path = GOAL / f'nn_look_{t}.png'
    Image.fromarray(im).resize((im.shape[1] * 4, im.shape[0] * 4), Image.NEAREST).save(path)
    print(path)


def outer(g):
    data = {t: Song(g, t) for t in ALL}
    base = np.load(GOAL / f'nested_f69{SUFFIX}.npz')
    out, rows = {}, []
    for k, o in enumerate(ALL):
        t0 = time.time()
        net = train(g, [u for u in ALL if u != o], data, seed=k)
        out[o] = predict(net, data[o])
        a, b = best_p(g, o, base[o]), best_p(g, o, out[o])
        rows.append((a, b))
        print(f'{o:28s} f69 {a:.2f}  net {b:.2f}  ({time.time() - t0:.0f} s)', flush=True)
    np.savez(OUT / f'nn_outer{SUFFIX}.npz', **out)
    a, b = np.mean(rows, axis=0)
    print(f'mean best precision at 90% recall: f69 {a:.3f}  net {b:.3f}')


def main():
    cmd = sys.argv[1]
    if cmd == 'prep':
        _init()
        with ProcessPoolExecutor(max(1, jobs()), initializer=_init) as ex:
            for t in ex.map(_prep, ALL):
                print('spectrum', t, flush=True)
        return
    g = Goal(mode=TRUTH)
    add_whole_song_truth(g)
    if cmd == 'look':
        look(g, sys.argv[2] if len(sys.argv) > 2 else 'corrosion')
    elif cmd == 'outer':
        outer(g)


if __name__ == '__main__':
    main()
