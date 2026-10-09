"""Kick-shape templates from isolated kick stems, matched causally against mix onsets.

A candidate's onset patch: 32 log-spaced bands (30 Hz-8 kHz) of causal STFT
power (1024-sample Hann windows ending at each hop), frames from the candidate
hop to its evidence deadline, minus the mean of the three frames ending two
hops before the candidate, clipped at zero: the spectral increase the new event
brings, shape only. Templates: k-means (K = 8) of L2-normalised patches taken
at fresh attacks of isolated kick audio. Features: the best cosine similarity
over templates for the whole patch, its bands under 300 Hz, and its bands over
1 kHz. Templates never come from the song being scored: dev songs use the four
new songs' kick stems, new songs use the dev songs' kick and drum stems.
"""
from __future__ import annotations

import numpy as np
from sklearn.cluster import KMeans

TEMPLATE_NAMES = ('tmpl_sim', 'tmpl_sim_low', 'tmpl_sim_high')
N_FFT, BANDS, K = 1024, 32, 8
EDGES = np.geomspace(30, 8000, BANDS + 1)


def band_spec(x, sr, hop):
    """Causal log band power: frame k uses the 1024 samples ending at (k + 1) * hop."""
    n_frames = len(x) // hop
    pad = np.concatenate([np.zeros(N_FFT), x])
    win = np.hanning(N_FFT)
    freqs = np.fft.rfftfreq(N_FFT, 1 / sr)
    which = np.digitize(freqs, EDGES) - 1
    out = np.zeros((n_frames, BANDS))
    step = 2048
    for s in range(0, n_frames, step):
        ks = np.arange(s, min(n_frames, s + step))
        ends = (ks + 1) * hop + N_FFT
        frames = np.stack([pad[e - N_FFT:e] for e in ends]) * win
        p = np.abs(np.fft.rfft(frames, axis=1)) ** 2
        for b in range(BANDS):
            m = which == b
            out[ks, b] = p[:, m].sum(axis=1) if m.any() else 0.0
    return np.log10(out + 1e-10)


def patches(spec, cand, avail):
    span = int(np.median(avail - cand)) + 1
    out = np.zeros((len(cand), span, BANDS))
    for n, (c, a) in enumerate(zip(cand, avail)):
        if c < 6 or c + span > len(spec):
            continue
        base = spec[c - 5:c - 2].mean(axis=0)
        out[n] = np.clip(spec[c:c + span] - base, 0, None)
    return out


def unit(p):
    f = p.reshape(len(p), -1)
    return f / (np.linalg.norm(f, axis=1, keepdims=True) + 1e-9)


def fit_templates(patch_list, seed=0):
    p = np.concatenate(patch_list)
    p = p[p.reshape(len(p), -1).sum(axis=1) > 0]
    return KMeans(n_clusters=K, n_init=4, random_state=seed).fit(unit(p)).cluster_centers_.reshape(K, p.shape[1], BANDS)


def template_features(patch, templates):
    low = EDGES[1:] <= 300
    high = EDGES[:-1] >= 1000
    out = np.zeros((len(patch), 3))
    for k, sel in enumerate((slice(None), low, high)):
        p = unit(patch[:, :, sel])
        t = unit(templates[:, :, sel])
        out[:, k] = (p @ t.T).max(axis=1)
    return out
