#!/usr/bin/env python3
"""Snare detector: one net that sees the spectrum slice AND the 47 snare measurements (snare_stack.measures).

The spectrum trunk is the kick net's (three conv layers, pooled); the measurements (standardised on the training
songs, clipped to +-5) pass one 32-unit layer; the two 32-unit summaries are joined before the last two layers.
Training, sampling, rows and scoring are snare_net's (leave one song out, cutoff chosen on the other songs).
Saves GOAL/snare_combo_s{seed}{TAG}{"_pitch" if PITCH else ""}.npz. Inputs follow snare_net (SNARE_FLAT, SNARE_PAST_MS, SNARE_AHEAD_MS); SNARE_PITCH=1 adds the pitch-stability measures.
Usage: snare_combo.py [SEED]"""
import json
import os
import sys
from pathlib import Path
import time
sys.path.insert(0, str(Path(__file__).resolve().parents[3]))
import numpy as np  # noqa: E402

HOP = 256


def main():
    os.environ.setdefault('KICK_GOAL_NN_AHEAD_MS', '0')
    import torch
    from torch import nn
    from tools.audio_analysis.eval.snare_cands import candidates
    from tools.audio_analysis.eval.snare_stack import measures
    from tools.audio_analysis.eval.snare_net import AHEAD, choose, rows, score
    from tools.audio_analysis.eval import kick_goal_nn as K
    from tools.audio_analysis.eval.kick_goal_eval import GOAL, Goal
    from tools.audio_analysis.eval.kick_goal_melodic import HARD_W
    from tools.audio_analysis.eval.detector_channels import flat_cache, pitch_cache, pitch_measures
    from tools.audio_analysis.eval.snare_net import FLAT, TAG
    from tools.audio_analysis.eval.detector_songs import audio, rate, spec

    class Combo(nn.Module):
        def __init__(self, n_meas):
            super().__init__()
            base = K.Net()
            self.conv, self.pool = base.conv, base.pool
            self.spec = nn.Sequential(nn.Linear(32 * (K.BANDS // 4) * K.TBINS, 32), nn.ReLU())
            self.meas = nn.Sequential(nn.Linear(n_meas, 32), nn.ReLU())
            self.head = nn.Sequential(nn.Linear(64, 32), nn.ReLU(), nn.Linear(32, 1))

        def forward(self, x, m):
            h = self.spec(self.pool(self.conv(x)).flatten(1))
            return self.head(torch.cat([h, self.meas(m)], 1)).squeeze(1)

    seed = int(sys.argv[1]) if len(sys.argv) > 1 else 0
    PITCH = os.environ.get('SNARE_PITCH') == '1'  # adds pitch-stability measures (detector_channels) to the 47
    g = Goal(mode='v3')
    lab = json.loads((GOAL / 'snare_labels.json').read_text())
    dev = K.device()
    data, meta, X = {}, {}, {}
    for t, s in lab.items():
        sr = rate(g, t)
        x = audio(g, t)
        cand, avail = candidates(x, sr, 4.5)
        onset, emit = (cand + 1) * HOP / sr, (avail + 1) * HOP / sr
        mask, y, hard = rows(s, onset)
        data[t] = K.Song(spec(g, t), onset, emit + AHEAD, mask, y, t, hard, (flat_cache(g, t),) if FLAT else ()).to(dev)
        meta[t] = (s, emit, avail, sr)
        cache = GOAL / f'snare_measures_{t}.npy'
        if not cache.exists():
            np.save(cache, measures(x, sr, cand, avail))
        X[t] = np.load(cache)
        if PITCH:
            X[t] = np.hstack([X[t], pitch_measures(pitch_cache(g, t), cand, avail, sr)])
    songs = list(lab)
    print('songs ready', flush=True)

    def train(us, seed):
        rng = np.random.default_rng(seed)
        torch.manual_seed(seed)
        allx = np.concatenate([X[u][data[u].mask] for u in us])
        mu, sd = allx.mean(0), allx.std(0) + 1e-6
        Z = {u: torch.from_numpy(np.clip((X[u] - mu) / sd, -5, 5).astype(np.float32)).to(dev) for u in songs}
        net = Combo(allx.shape[1]).to(dev)
        opt = torch.optim.AdamW(net.parameters(), lr=K.LR, weight_decay=1e-4)
        rws = [(data[u], np.flatnonzero(data[u].mask & (data[u].y == 1)), np.flatnonzero(data[u].mask & (data[u].y == 0))) for u in us]
        per = max(1, int(np.median([len(p) for _, p, _ in rws])))
        for _ in range(K.EPOCHS):
            bx, bm, by = [], [], []
            for song, pos, neg in rws:
                for idx, yv in ((pos, 1.0), (neg, 0.0)):
                    if not len(idx):
                        continue
                    w = np.where(song.hard[idx], HARD_W, 1.0) if yv == 0.0 else np.ones(len(idx))
                    pick = rng.choice(idx, per, p=w / w.sum())
                    bx.append(song.slices(pick, rng.integers(-K.JITTER, K.JITTER + 1, len(pick))))
                    bm.append(Z[song.name][torch.from_numpy(pick).to(dev)])
                    by.append(torch.full((len(pick),), yv, device=dev))
            xa, ma, ya = torch.cat(bx), torch.cat(bm), torch.cat(by)
            order = torch.from_numpy(rng.permutation(len(ya))).to(dev)
            net.train()
            for s in range(0, len(ya), K.BATCH):
                b = order[s:s + K.BATCH]
                loss = nn.functional.binary_cross_entropy_with_logits(net(xa[b], ma[b]), ya[b] * (1 - K.SMOOTH) + K.SMOOTH / 2)
                opt.zero_grad()
                loss.backward()
                opt.step()
        return net.eval(), Z

    held = {}
    for t in songs:
        t0 = time.time()
        net, Z = train([u for u in songs if u != t], 1000 * songs.index(t) + seed)
        out = []
        with torch.no_grad():
            n = len(data[t].end)
            for s in range(0, n, 2048):
                idx = np.arange(s, min(n, s + 2048))
                out.append(torch.sigmoid(net(data[t].slices(idx), Z[t][torch.from_numpy(idx).to(dev)])).cpu())
        held[t] = torch.cat(out).numpy().astype(np.float64)
        print(f'combo without {t}: {time.time() - t0:.0f} s', flush=True)
    tm = tn = tf = 0
    for t in songs:
        th = choose([(meta[u][0], meta[u][1], held[u], meta[u][2], meta[u][3]) for u in songs if u != t and not lab[u]['positives_only']])
        m, n, f = score(meta[t][0], meta[t][1], held[t], meta[t][2], th, meta[t][3])
        tm, tn, tf = tm + m, tn + n, tf + f
        print(f'  {t:14s} cutoff {th:.2f}: {m}/{n} caught, {f} false fires', flush=True)
    print(f'combo net, held out: R {tm / tn:.3f} P {tm / max(1, tm + tf):.3f} ({tm}/{tn}, {tf} false)')
    np.savez(GOAL / f'snare_combo_s{seed}.npz', **held)


if __name__ == '__main__':
    main()
