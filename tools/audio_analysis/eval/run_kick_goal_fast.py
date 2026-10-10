#!/usr/bin/env python3
"""Fast grouped evaluation: trees, nets, their blend and the 8 s song-relative stage, scored live.

Usage: run_kick_goal_fast.py   (song-set env flags as run_kick_goal_data; KICK_GOAL_FOLDS=k groups, default 9;
KICK_GOAL_NN_ENSEMBLE=n nets per group, default 1; KICK_GOAL_JOBS=n CPU workers, default 10)

Songs split into k fixed groups, round-robin within each kind, so every group mixes
kinds. For each held-out group F:
- trees fitted without F score F; trees fitted without F and G score G, for every
  other group G (the stage's training inputs and the cutoff songs);
- nets fitted without F score F. Nets have no inner fits: G's inputs come from the
  nets fitted without G, which saw F. That is a mild optimism in the stage's training
  rows and in the cutoff, never in F's own scores;
- blend = mean logit of trees and nets;
- the stage (run_kick_goal_selfsim2 R3_self8) is fitted on the other groups. Its
  cutoff maximises pooled F1 over stage fits that each leave one other group out.
Every song is scored once, at a cutoff chosen without it, by models that never saw it.
Trees run on CPU workers while the nets train on the GPU in this process.

Reports pooled and song-mean recall and precision, Peter's songs vs outside songs,
and the 90/90 check. Recall-only songs (KICK_GOAL_RECALL) count toward recall only:
they are left out of every cutoff choice and of precision.
"""
from __future__ import annotations

import json
import os
import sys
import time
from concurrent.futures import ProcessPoolExecutor
from multiprocessing import get_context
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[3]))

import numpy as np  # noqa: E402
from scipy.special import expit, logit  # noqa: E402

from tools.audio_analysis.eval.kick_goal_eval import (  # noqa: E402
    DEV_STEMS, MORE_SONGS, NEW_SONGS, OUT, RECALL_SONGS, SUFFIX, TRACKS, TRIGGER_SONGS, TRUTH, WIP_SONGS, Goal,
    add_whole_song_truth, choose, counts, score)
from tools.audio_analysis.eval.kick_goal_featsets import build, lowbank_cache, profile_cache  # noqa: E402
from tools.audio_analysis.eval.kick_goal_selfsim import self_features  # noqa: E402
from tools.audio_analysis.eval.run_kick_goal_data import ALL, gbt  # noqa: E402
from tools.audio_analysis.eval.run_kick_goal_selfsim import fit2  # noqa: E402

FOLDS = int(os.environ.get('KICK_GOAL_FOLDS', '9'))
NETS = int(os.environ.get('KICK_GOAL_NN_ENSEMBLE', '1'))
WORKERS = int(os.environ.get('KICK_GOAL_JOBS', '10'))
FEATS = 'f69'
STAGE_COLS = list(range(5))
OUTSIDE = {'apricots_128bpm', 'bad_guy_128bpm', 'feel_the_vibration_174bpm', 'inhale_exhale_145bpm', 'tears_140bpm',
           'lh', 'eat_sleep', 'business', 'worship', 'gerrit', 'cold_remix'}
STATE = {}


def groups():
    """Fixed fold assignment: each kind's songs dealt round-robin across the folds in turn."""
    hand = [t for t in TRACKS if t not in DEV_STEMS]
    stems = [t for t in TRACKS if t in DEV_STEMS] + list(NEW_SONGS)
    kinds = [hand, stems, list(MORE_SONGS), list(TRIGGER_SONGS), list(WIP_SONGS), list(RECALL_SONGS)]
    order = [t for k in kinds for t in sorted(k)]
    assert sorted(order) == sorted(ALL)
    out = [[] for _ in range(FOLDS)]
    for i, t in enumerate(order):
        out[i % FOLDS].append(t)
    return [tuple(x) for x in out]


def setup():
    g = Goal(mode=TRUTH)
    add_whole_song_truth(g)
    build(g, FEATS)
    STATE['g'] = g
    STATE['fit'], STATE['pred'] = gbt(g, True, FEATS)


def tree_task(task):
    """(groups left out, songs to score) -> {song: probabilities}."""
    out, scored = task
    m = STATE['fit']([u for u in ALL if u not in out])
    return {u: STATE['pred'](m, u) for u in scored}


def lg(p):
    return logit(np.clip(p, 1e-6, 1 - 1e-6))


def nets(g, folds):
    """{group index: {song: mean net probability}} from NETS nets fitted without that group."""
    from tools.audio_analysis.eval.kick_goal_nn import Song, device, predict, train
    dev = device()
    data = {t: Song.real(g, t).to(dev) for t in ALL}
    out = {}
    for k, f in enumerate(folds):
        t0 = time.time()
        fitted = [train([data[u] for u in ALL if u not in f], 1000 * k + 7 * e) for e in range(NETS)]
        out[k] = {t: np.mean([predict(net, data[t]) for net in fitted], axis=0) for t in f}
        print(f'nets group {k} ({time.time() - t0:.0f} s)', flush=True)
    return out


def stage_matrix(g, shape, t, p):
    r = g.records[t]
    return self_features(p, shape[t], r['features'][:, 0], r['emit_s'], 8.0)


def stage_task(task):
    """Held-out group F: stage inputs for the other groups, inner cutoff, stage scores for F."""
    k, folds, base_in, base_out = task
    g = STATE['g']
    shape = STATE.setdefault('shape', {t: np.hstack([profile_cache(g, t), lowbank_cache(g, t)]) for t in ALL})
    train = {u: stage_matrix(g, shape, u, p) for u, p in base_in.items()}
    inner = {}
    for j, h in enumerate(folds):
        if j == k:
            continue
        m = fit2(g, train, [u for u in train if u not in h], STAGE_COLS)
        inner.update({u: m.predict_proba(train[u][:, STAGE_COLS])[:, 1] for u in h if u not in RECALL_SONGS})
    th, _ = choose(g, inner)
    m = fit2(g, train, list(train), STAGE_COLS)
    return {o: m.predict_proba(stage_matrix(g, shape, o, p)[:, STAGE_COLS])[:, 1] for o, p in base_out.items()}, th


def report(g, name, preds, cuts):
    """preds {song: probabilities}, cuts {song: cutoff} -> one summary line and the per-song rows."""
    per = {}
    for t, p in preds.items():
        m, e, n = counts(score(g, t, p, cuts[t])[1])
        per[t] = (m, e, n)
    def pooled(songs, prec=True):
        m = sum(per[t][0] for t in songs)
        n = sum(per[t][2] for t in songs)
        e = sum(per[t][1] for t in songs if t not in RECALL_SONGS)
        mp = sum(per[t][0] for t in songs if t not in RECALL_SONGS)
        return m / max(1, n), mp / max(1, mp + e)
    songs = list(preds)
    r_all, p_all = pooled(songs)
    rec = {t: per[t][0] / max(1, per[t][2]) for t in songs}
    prc = {t: per[t][0] / max(1, per[t][0] + per[t][1]) for t in songs if t not in RECALL_SONGS}
    mine = [t for t in songs if t not in OUTSIDE]
    out = [t for t in songs if t in OUTSIDE]
    r_m, p_m = pooled(mine)
    r_o, p_o = pooled(out)
    worst = min(rec, key=rec.get)
    meets = r_all >= .9 and p_all >= .9 and rec[worst] >= .8
    print(f'{name:12s} pooled R {r_all:.3f} P {p_all:.3f} | song-mean R {np.mean(list(rec.values())):.3f} '
          f'P {np.mean(list(prc.values())):.3f} | Peter R {r_m:.3f} P {p_m:.3f} | outside R {r_o:.3f} P {p_o:.3f} | '
          f'worst {worst} {rec[worst]:.2f} | 90/90 {"MET" if meets else "no"}', flush=True)
    return dict(pooled=[r_all, p_all], peter=[r_m, p_m], outside=[r_o, p_o], meets=meets,
                per_song={t: dict(matched=per[t][0], extra=per[t][1], labels=per[t][2], cutoff=float(cuts[t])) for t in songs})


def main():
    t_start = time.time()
    folds = groups()
    print('groups:', [list(f) for f in folds], flush=True)
    setup()
    g = STATE['g']
    tasks = [((f,), f) for f in folds]
    keys = [(k, None) for k in range(len(folds))]
    for i in range(len(folds)):
        for j in range(i + 1, len(folds)):
            tasks.append(((folds[i], folds[j]), folds[i] + folds[j]))
            keys.append((i, j))
    tree_out, tree_in = {}, {}  # tree_out[k][song]; tree_in[(k, j)][song]: song in group j, trees without k and j
    os.environ.update(OMP_NUM_THREADS='1', OPENBLAS_NUM_THREADS='1', VECLIB_MAXIMUM_THREADS='1', MKL_NUM_THREADS='1')
    with ProcessPoolExecutor(WORKERS, mp_context=get_context('spawn'), initializer=setup) as ex:
        futures = [ex.submit(tree_task, (tuple(u for f in out for u in f), scored)) for out, scored in tasks]
        net_p = nets(g, folds)
        print(f'nets done at {time.time() - t_start:.0f} s', flush=True)
        for (i, j), fut in zip(keys, futures):
            p = fut.result()
            if j is None:
                tree_out[i] = p
            else:
                tree_in[(i, j)] = {u: p[u] for u in folds[j]}
                tree_in[(j, i)] = {u: p[u] for u in folds[i]}
        print(f'trees done at {time.time() - t_start:.0f} s', flush=True)

        net_of = {t: net_p[k][t] for k, f in enumerate(folds) for t in f}
        bases = {'trees': lambda k, j, t: tree_in[(k, j)][t] if j is not None else tree_out[k][t],
                 'blend': lambda k, j, t: expit((lg(tree_in[(k, j)][t] if j is not None else tree_out[k][t]) + lg(net_of[t])) / 2)}
        results = {}
        for name, base in bases.items():
            raw = {t: base(k, None, t) for k, f in enumerate(folds) for t in f}
            raw_cut = {}
            for k, f in enumerate(folds):
                th, _ = choose(g, {t: base(k, j, t) for j, h in enumerate(folds) if j != k for t in h if t not in RECALL_SONGS})
                raw_cut.update({t: th for t in f})
            results[name] = report(g, name, raw, raw_cut)
            stage_tasks = [(k, folds, {t: base(k, j, t) for j, h in enumerate(folds) if j != k for t in h},
                            {t: base(k, None, t) for t in f}) for k, f in enumerate(folds)]
            staged, cuts = {}, {}
            for k, (p, th) in enumerate(ex.map(stage_task, stage_tasks)):
                staged.update(p)
                cuts.update({t: th for t in folds[k]})
            results[name + '+stage'] = report(g, name + '+stage', staged, cuts)
            np.savez(OUT / f'fast_{name}{SUFFIX}.npz', **staged)
    for name, r in results.items():
        bad = sorted(r['per_song'].items(), key=lambda kv: kv[1]['matched'] / max(1, kv[1]['labels']) - kv[1]['extra'] / max(1, kv[1]['labels']))[:6]
        print(f'{name} hardest:', ', '.join(f"{t} {v['matched']}/{v['labels']}+{v['extra']}" for t, v in bad), flush=True)
    (OUT / f'results_fast{SUFFIX}.json').write_text(json.dumps(dict(groups=folds, results=results), indent=1, default=float))
    print(f'total {time.time() - t_start:.0f} s', flush=True)


if __name__ == '__main__':
    main()
