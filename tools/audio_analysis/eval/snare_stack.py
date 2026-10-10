#!/usr/bin/env python3
"""Snare detector, step 3: trees on snare measurements, the blend with the held-out net (snare_net), and the kick's
8 s song-relative stage. Leave-one-song-out throughout; cutoffs chosen on the other songs. Reports net / trees / blend /
blend+stage. The stage trains on other songs' held-out blends, whose nets saw the scored song (the kick fast loop's
declared mild optimism). With --mix it blends the net trained with the snare-mix clips. Usage: snare_stack.py [NET_SEED] [--mix]

Measurements per candidate (47): the 32-band rise profile (kick_goal_profile on kick_goal_templates.band_spec) and, for
7 bands from 40 Hz to 16 kHz on the 5 ms hop grid, the rise (max over candidate..deadline minus the mean of the three
hops ending two before the candidate) and the decay (deadline minus that max), plus the spread of the four upper rises
(a snare's noise rises everywhere above 1 kHz at once)."""
import json
import os
import sys
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parents[3]))
import numpy as np  # noqa: E402
from scipy.special import expit, logit  # noqa: E402
from sklearn.ensemble import HistGradientBoostingClassifier  # noqa: E402

BANDS = ((40, 120), (120, 300), (300, 800), (800, 2000), (2000, 5000), (5000, 10000), (10000, 16000))
HOP = 256


def measures(x, sr, cand, avail):
    from tools.audio_analysis.eval.snare_cands import band_hops
    from tools.audio_analysis.eval.kick_goal_profile import rise_profile
    from tools.audio_analysis.eval.kick_goal_templates import band_spec
    prof = rise_profile(band_spec(x, sr, HOP), cand, avail)
    env = np.stack([band_hops(x, sr, lo, hi) for lo, hi in BANDS], 1)
    out = np.zeros((len(cand), 2 * len(BANDS) + 1))
    for n, (c, a) in enumerate(zip(cand, avail)):
        if c < 6 or a >= len(env):
            continue
        base = env[c - 5:c - 2].mean(axis=0)
        peak = env[c:a + 1].max(axis=0)
        out[n, :7] = peak - base
        out[n, 7:14] = env[a] - peak
        out[n, 14] = np.std(out[n, 3:7])
    return np.hstack([prof, out])


def lg(p):
    return logit(np.clip(p, 1e-6, 1 - 1e-6))


def fit(x, y, w, leaf=15, iters=200, min_leaf=40):
    return HistGradientBoostingClassifier(max_iter=iters, learning_rate=.05, max_leaf_nodes=leaf, min_samples_leaf=min_leaf,
                                          l2_regularization=1.0, random_state=0).fit(x, y, sample_weight=w * len(y))


def balanced(y, hard):
    from tools.audio_analysis.eval.kick_goal_melodic import HARD_W
    neg = np.where(hard, HARD_W, 1.0) * (y == 0)
    return np.where(y == 1, .5 / max(1, y.sum()), .5 * neg / max(1e-9, neg.sum()))


def main():
    from tools.audio_analysis.eval.snare_cands import candidates
    from tools.audio_analysis.eval.detector_eval import choose, rows, score
    from tools.audio_analysis.eval.kick_goal_eval import GOAL, Goal
    from tools.audio_analysis.eval.kick_goal_selfsim import self_features
    from tools.audio_analysis.eval.detector_songs import audio, rate, spec
    args = [a for a in sys.argv[1:] if a != '--mix']
    seed = int(args[0]) if args else 0
    tag = '_mix' if '--mix' in sys.argv else ''
    g = Goal(mode='v3')
    lab = json.loads((GOAL / 'snare_labels.json').read_text())
    name = os.environ.get('SNARE_NET', f'snare_net_s{seed}{tag}')  # e.g. snare_combo_s0_..., any saved held-out net
    net = dict(np.load(GOAL / f'{name}.npz'))
    tag = name.removeprefix(f'snare_net_s{seed}') if name.startswith('snare_net') else '_' + name
    songs = list(lab)
    D = {}
    for t in songs:
        sr = rate(g, t)
        x = audio(g, t)
        cand, avail = candidates(x, sr, 4.5)
        onset, emit = (cand + 1) * HOP / sr, (avail + 1) * HOP / sr
        mask, y, hard = rows(lab[t], onset)
        D[t] = dict(X=measures(x, sr, cand, avail), mask=mask, y=y, hard=hard, emit=emit, avail=avail, sr=sr)
    print('measures built', flush=True)

    def stack(us, key):
        return np.concatenate([D[u][key][D[u]['mask']] for u in us])

    trees = {}
    for t in songs:
        us = [u for u in songs if u != t]
        y, hard = stack(us, 'y'), stack(us, 'hard')
        trees[t] = fit(stack(us, 'X'), y, balanced(y, hard)).predict_proba(D[t]['X'])[:, 1]
    blend = {t: expit((lg(trees[t]) + lg(net[t])) / 2) for t in songs}

    def shape_level(t):
        return D[t]['X'][:, :32], D[t]['X'][:, 32 + 4]  # rise profile; 2-5 kHz rise

    staged = {}
    for t in songs:
        us = [u for u in songs if u != t]
        feats = {u: self_features(blend[u], *shape_level(u), D[u]['emit'], 8.0) for u in us + [t]}
        y = np.concatenate([D[u]['y'][D[u]['mask']] for u in us])
        x = np.concatenate([feats[u][D[u]['mask']] for u in us])
        hard = np.concatenate([D[u]['hard'][D[u]['mask']] for u in us])
        staged[t] = fit(x, y, balanced(y, hard), leaf=7, iters=150, min_leaf=80).predict_proba(feats[t])[:, 1]

    for name, P in (('net', net), ('trees', trees), ('blend', blend), ('blend+stage', staged)):
        tm = tn = tf = 0
        per = []
        for t in songs:
            items = [(lab[u], D[u]['emit'], P[u], D[u]['avail'], D[u]['sr']) for u in songs if u != t and not lab[u]['positives_only']]
            th = choose(items)
            m, n, f = score(lab[t], D[t]['emit'], P[t], D[t]['avail'], th, D[t]['sr'])
            tm, tn, tf = tm + m, tn + n, tf + f
            per.append(f'{t} {m}/{n}+{f}')
        print(f'{name:12s} R {tm / tn:.3f} P {tm / max(1, tm + tf):.3f} | ' + ', '.join(per), flush=True)
    np.savez(GOAL / f'snare_full_s{seed}{tag}.npz', **{f'{k}|{t}': v for k, P in (('trees', trees), ('staged', staged)) for t, v in P.items()})


if __name__ == '__main__':
    main()
