"""Causal per-song feature normalisation (running mean and spread over the past 20 s).

Each candidate's 15 features are re-expressed against the candidates of the same
song in the previous 20 s (the current one excluded): z = (f - mean) / std. This
is per-song input normalisation, the speech-recognition fix for channel and
production differences; it differs from H12, which re-centred the output score.
Until 50 past candidates exist the statistics blend toward a population prior
computed from songs other than the one being normalised. Bounded memory: one 20 s
window of candidate features.
"""
from __future__ import annotations

import numpy as np

WINDOW_S, MIN_COUNT = 20.0, 50


def song_normalised(features, onset_s, prior_mean, prior_std):
    f = np.asarray(features, dtype=np.float64)
    t = np.asarray(onset_s)
    cs = np.vstack([np.zeros(f.shape[1]), np.cumsum(f, axis=0)])
    cs2 = np.vstack([np.zeros(f.shape[1]), np.cumsum(f * f, axis=0)])
    i = np.arange(len(f))
    j = np.searchsorted(t, t - WINDOW_S, side='left')
    n = (i - j)[:, None].astype(float)
    s, s2 = cs[i] - cs[j], cs2[i] - cs2[j]
    a = np.minimum(n / MIN_COUNT, 1.0)
    mean = a * np.divide(s, np.maximum(n, 1)) + (1 - a) * prior_mean
    var = np.divide(s2, np.maximum(n, 1)) - np.divide(s, np.maximum(n, 1)) ** 2
    std = np.sqrt(a * np.maximum(var, 0) + (1 - a) * prior_std ** 2) + 1e-6
    return (f - mean) / std


def add_normalised(g, tracks, new_songs, goal):
    """Adds f30 (raw + normalised), f15n (normalised) and f31 (f30 + pre_rel_db) to each record.
    The prior for a dev song comes from the new songs and vice versa."""
    dev_f = np.concatenate([g.records[t]['features'] for t in tracks])
    new_f = np.concatenate([g.records[t]['features'] for t in new_songs])
    for t, r in g.records.items():
        prior = new_f if t in tracks else dev_f
        n = song_normalised(r['features'], r['onset_s'], prior.mean(0), prior.std(0))
        pre = np.load(goal / f'tail_{t}.npy')[:, 1:2]
        r['f30'] = np.hstack([r['features'], n])
        r['f15n'] = n
        r['f31'] = np.hstack([r['features'], n, pre])
