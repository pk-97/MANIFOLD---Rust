"""Hybrid kick network: a small CNN that judges each candidate from a causal full-spectrum slice.

Slice: 64 log-spaced causal band-pass envelopes from 30 Hz to 16 kHz (constant Q,
so the low end resolves a kick's pitch drop and the top its beater click), on a
2 ms grid, the SLICE_MS ending at the candidate's emission time (the detector's
own decision point, so the net sees nothing the live detector would not have).
Two channels: the slice less its own maximum (shape without gain) and the slice
less each band's median over the 100 ms before the onset (what is new).

The net's job is kick or not at a candidate; its output joins the trees as a
feature. Every prediction for a song comes from a net that never trained on it.
"""
from __future__ import annotations

import numpy as np
import torch
from scipy.signal import butter, sosfilt
from torch import nn

from tools.audio_analysis.eval.kick_goal_eval import GOAL
from tools.audio_analysis.eval.run_kick_goal_tail_component import mix_audio

CENTRES = np.geomspace(30.0, 16000.0, 64)
HALF_BW = .1
FRAME_S = .002
SLICE = 75  # frames: 150 ms ending at emission
PRE_S = .1
EPOCHS = 12
BATCH = 256
LR = 2e-3
SMOOTH = .05
JITTER = 1  # frames


def spectrum(x, sr):
    """(n_frames, 64) causal band envelopes in dB on the 2 ms grid."""
    grid = np.round(np.arange(int(len(x) / sr / FRAME_S)) * FRAME_S * sr).astype(int)
    out = np.zeros((len(grid), len(CENTRES)), np.float32)
    for b, fc in enumerate(CENTRES):
        sos = butter(2, [fc * (1 - HALF_BW), fc * (1 + HALF_BW)], btype='bandpass', fs=sr, output='sos')
        y = np.abs(sosfilt(sos, x))
        k = max(int(.002 * sr), int(2 * sr / fc))  # at least two periods, so the rectified ripple averages out
        cs = np.concatenate([np.zeros(k), np.cumsum(y)])
        out[:, b] = 20 * np.log10((cs[grid + k] - cs[grid]) / k + 1e-9)
    return out


def spectrum_cache(g, t):
    path = GOAL / f'nnspec_{t}.npy'
    if not path.exists():
        np.save(path, spectrum(mix_audio(g, t), g.records[t]['sample_rate']).astype(np.float16))
    return np.load(path).astype(np.float32)


class Song:
    """A song's spectrum and its candidates' slice anchors."""

    def __init__(self, g, t):
        r = g.records[t]
        self.spec = torch.from_numpy(spectrum_cache(g, t))
        n = len(self.spec)
        self.end = np.clip((r['emit_s'] / FRAME_S).astype(int), SLICE + JITTER, n - 1 - JITTER)
        self.pre = np.clip(((r['onset_s'] - PRE_S) / FRAME_S).astype(int), 0, n - 2)
        self.on = np.clip((r['onset_s'] / FRAME_S).astype(int), self.pre + 1, n - 1)

    def slices(self, idx, shift=None):
        end = self.end[idx] + (shift if shift is not None else 0)
        frames = torch.from_numpy(end[:, None] - np.arange(SLICE)[::-1][None, :].copy())
        x = self.spec[frames]  # (B, SLICE, 64)
        span = int(PRE_S / FRAME_S)
        pre_frames = torch.from_numpy(np.clip(self.on[idx][:, None] - np.arange(span)[None, :], 0, None))
        base = self.spec[pre_frames].median(dim=1).values  # (B, 64)
        shape = x - x.amax(dim=(1, 2), keepdim=True)
        new = x - base[:, None, :]
        return torch.stack([shape, new], 1).transpose(2, 3) / 20.0  # (B, 2, 64 bands, SLICE frames)


class Net(nn.Module):
    """Three 3x3 conv layers; time is pooled away at the end, frequency position is kept."""

    def __init__(self):
        super().__init__()
        self.conv = nn.Sequential(nn.Conv2d(2, 16, 3, padding=1), nn.ReLU(), nn.MaxPool2d(2),
                                  nn.Conv2d(16, 32, 3, padding=1), nn.ReLU(), nn.MaxPool2d(2),
                                  nn.Conv2d(32, 32, 3, padding=1), nn.ReLU())
        self.head = nn.Sequential(nn.Linear(32 * 16, 32), nn.ReLU(), nn.Linear(32, 1))

    def forward(self, x):
        h = self.conv(x).amax(dim=3)  # (B, 32, 16 bands)
        return self.head(h.flatten(1)).squeeze(1)


def device():
    return torch.device('mps' if torch.backends.mps.is_available() else 'cpu')


def train(g, songs, data, seed):
    """A net fitted on the songs' training rows: song-equal, class-balanced sampling."""
    rng = np.random.default_rng(seed)
    torch.manual_seed(seed)
    dev = device()
    net = Net().to(dev)
    opt = torch.optim.AdamW(net.parameters(), lr=LR, weight_decay=1e-4)
    rows = {}
    for t in songs:
        r = g.records[t]
        m = r['train_mask']
        rows[t] = (np.flatnonzero(m & (r['train_y'] == 1)), np.flatnonzero(m & (r['train_y'] == 0)))
    per_song = max(1, int(np.median([len(p) for p, _ in rows.values()])))
    for _ in range(EPOCHS):
        batch_x, batch_y = [], []
        for t in songs:
            pos, neg = rows[t]
            for idx, y in ((pos, 1.0), (neg, 0.0)):
                if not len(idx):
                    continue
                pick = rng.choice(idx, per_song)
                batch_x.append(data[t].slices(pick, rng.integers(-JITTER, JITTER + 1, len(pick))))
                batch_y.append(torch.full((len(pick),), y))
        x, y = torch.cat(batch_x), torch.cat(batch_y)
        order = torch.from_numpy(rng.permutation(len(y)))
        net.train()
        for s in range(0, len(y), BATCH):
            b = order[s:s + BATCH]
            xb, yb = x[b].to(dev), y[b].to(dev)
            loss = nn.functional.binary_cross_entropy_with_logits(net(xb), yb * (1 - SMOOTH) + SMOOTH / 2)
            opt.zero_grad()
            loss.backward()
            opt.step()
    return net.eval()


@torch.no_grad()
def predict(net, song):
    dev = device()
    out = []
    n = len(song.end)
    for s in range(0, n, 2048):
        out.append(torch.sigmoid(net(song.slices(np.arange(s, min(n, s + 2048))).to(dev))).cpu())
    return torch.cat(out).numpy().astype(np.float64)
