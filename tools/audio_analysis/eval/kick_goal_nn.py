"""Hybrid kick network: a small CNN that judges each candidate from a causal full-spectrum slice.

Slice: BANDS log-spaced causal band-pass envelopes from 30 Hz to 16 kHz (constant Q,
so the low end resolves a kick's pitch drop and the top its beater click), on a
2 ms grid, the PAST_MS ending at the candidate's emission time (the detector's
own decision point, so the net sees nothing the live detector would not have).
Channels: the slice less its own maximum (shape without gain); the slice less each
band's median over the 100 ms before the onset (what is new); with PERC_ON, how
percussive each cell is (a causal harmonic/percussive split: sustained = each
band's median over the past 50 ms, percussive = the median across 9 neighbouring
bands now); with STEREO_ON, each band's side level less its mono level (kicks sit
dead centre).

The input is set by env: KICK_GOAL_NN_BANDS (64), KICK_GOAL_NN_PAST_MS (150),
KICK_GOAL_NN_PERC=1, KICK_GOAL_NN_STEREO=1. A slice longer than 150 ms keeps
coarse time (TBINS max-pooled bins) so the net can tell the past from the hit.
Every prediction for a song comes from a net that never trained on it.
"""
from __future__ import annotations

import os
import numpy as np
import torch
from scipy.ndimage import median_filter
from scipy.signal import butter, sosfilt
from torch import nn

from tools.audio_analysis.eval.kick_goal_eval import GOAL
from tools.audio_analysis.eval.kick_goal_melodic import HARD_W
from tools.audio_analysis.eval.kick_goal_stereo import side_audio
from tools.audio_analysis.eval.run_kick_goal_tail_component import mix_audio

BANDS = int(os.environ.get('KICK_GOAL_NN_BANDS', '64'))
PAST_MS = int(os.environ.get('KICK_GOAL_NN_PAST_MS', '150'))
PERC_ON = os.environ.get('KICK_GOAL_NN_PERC') == '1'
STEREO_ON = os.environ.get('KICK_GOAL_NN_STEREO') == '1'
# The net's slice ends this much after the emission; the fire keeps the emission's time stamp, so the wait is
# accepted output latency (Peter, Oct 2026: 20-40 ms is fine), not timing error eaten from the 70 ms tolerance.
AHEAD_MS = int(os.environ.get('KICK_GOAL_NN_AHEAD_MS', '0'))
CENTRES = np.geomspace(30.0, 16000.0, BANDS)
HALF_BW = .1 * 64 / BANDS  # about one band spacing either side
FRAME_S = .002
SLICE = PAST_MS // 2  # frames ending at emission
TBINS = 1 if SLICE <= 75 else 4
CHANNELS = 2 + PERC_ON + STEREO_ON
DEFAULT_INPUT = BANDS == 64 and PAST_MS == 150 and not PERC_ON and not STEREO_ON and not AHEAD_MS
INPUT_TAG = '' if DEFAULT_INPUT else (f'_b{BANDS}' + (f'_h{PAST_MS}' if PAST_MS != 150 else '') + ('_perc' if PERC_ON else '')
                                     + ('_st' if STEREO_ON else '') + (f'_a{AHEAD_MS}' if AHEAD_MS else ''))
SUSTAIN = 25  # frames: 50 ms
NEIGHBOURS = 9
PRE_S = .1
EPOCHS = 12
BATCH = 256
LR = 2e-3
SMOOTH = .05
JITTER = 1  # frames


def spectrum(x, sr):
    """(n_frames, BANDS) causal band envelopes in dB on the 2 ms grid."""
    grid = np.round(np.arange(int(len(x) / sr / FRAME_S)) * FRAME_S * sr).astype(int)
    out = np.zeros((len(grid), len(CENTRES)), np.float32)
    for b, fc in enumerate(CENTRES):
        sos = butter(2, [fc * (1 - HALF_BW), fc * (1 + HALF_BW)], btype='bandpass', fs=sr, output='sos')
        y = np.abs(sosfilt(sos, x))
        k = max(int(.002 * sr), int(2 * sr / fc))  # at least two periods, so the rectified ripple averages out
        cs = np.concatenate([np.zeros(k), np.cumsum(y)])
        out[:, b] = 20 * np.log10((cs[grid + k] - cs[grid]) / k + 1e-9)
    return out


def _band_tag():
    return '' if BANDS == 64 else str(BANDS)


def spectrum_cache(g, t):
    path = GOAL / f'nnspec{_band_tag()}_{t}.npy'
    if not path.exists():
        np.save(path, spectrum(mix_audio(g, t), g.records[t]['sample_rate']).astype(np.float16))
    return np.load(path).astype(np.float32)


def percussive(spec):
    """Causal percussive share in dB (0 = all percussive): sustained = each band's median over the past
    SUSTAIN frames, percussive = the median across NEIGHBOURS bands in the frame; share = P^2 / (P^2 + S^2)."""
    lin = 10 ** (spec.astype(np.float32) / 20)
    sus = median_filter(lin, size=(SUSTAIN, 1), origin=((SUSTAIN - 1) // 2, 0), mode='nearest')
    per = median_filter(lin, size=(1, NEIGHBOURS), mode='nearest')
    return (10 * np.log10(per ** 2 / (per ** 2 + sus ** 2 + 1e-18) + 1e-6)).astype(np.float16)


def extra_caches(g, t, spec):
    """The extra channels' full-song arrays (n_frames, BANDS), each cached per song."""
    out = []
    if PERC_ON:
        path = GOAL / f'nnperc{_band_tag()}_{t}.npy'
        if not path.exists():
            np.save(path, percussive(spec))
        out.append(np.load(path).astype(np.float32))
    if STEREO_ON:
        path = GOAL / f'nnside{_band_tag()}_{t}.npy'
        if not path.exists():
            np.save(path, spectrum(side_audio(g, t), g.records[t]['sample_rate']).astype(np.float16))
        side = np.load(path).astype(np.float32)
        n = min(len(side), len(spec))
        rel = np.full_like(spec, -40.0)
        rel[:n] = np.clip(side[:n] - spec[:n], -40.0, 10.0)
        out.append(rel)
    return out


class Song:
    """A spectrum, its candidates' slice anchors and training rows (mask, labels)."""

    def __init__(self, spec, onset_s, emit_s, mask, y, name='', hard=None, extra=()):
        self.spec, self.mask, self.y, self.name = torch.from_numpy(spec), mask, y, name
        self.extra = [torch.from_numpy(e) for e in extra]
        self.hard = np.zeros(len(onset_s), bool) if hard is None else np.asarray(hard, bool)
        n = len(self.spec)
        self.end = np.clip((emit_s / FRAME_S).astype(int), SLICE + JITTER, n - 1 - JITTER)
        self.pre = np.clip(((onset_s - PRE_S) / FRAME_S).astype(int), 0, n - 2)
        self.on = np.clip((onset_s / FRAME_S).astype(int), self.pre + 1, n - 1)
        span = int(PRE_S / FRAME_S)
        # Each candidate's pre-onset baseline does not depend on the slice jitter: computed once.
        self.base = torch.cat([self.spec[torch.from_numpy(np.clip(self.on[a:a + 4096][:, None] - np.arange(span)[None, :], 0, None))]
                               .median(dim=1).values for a in range(0, len(self.on), 4096)]) if len(self.on) else torch.zeros(0, BANDS)
        self.dev = torch.device('cpu')

    def to(self, dev):
        """Keeps the spectrum, extra channels and baselines on dev, so slices are cut there."""
        self.spec, self.base, self.dev = self.spec.to(dev), self.base.to(dev), dev
        self.extra = [e.to(dev) for e in self.extra]
        return self

    @classmethod
    def real(cls, g, t):
        r = g.records[t]
        mask = r['train_mask'] & (r['train_y'] == 1) if r.get('positives_only') else r['train_mask']
        spec = spectrum_cache(g, t)
        return cls(spec, r['onset_s'], r['emit_s'] + AHEAD_MS / 1000, mask, r['train_y'], t, r.get('hard_neg'), extra_caches(g, t, spec))

    @classmethod
    def synth(cls, path):
        assert DEFAULT_INPUT, 'kick-swap clips hold 64-band spectra only'
        with np.load(path) as z:
            song = cls(z['spec'].astype(np.float32), z['onset_s'], z['emit_s'], z['mask'], z['y'], path.stem)
            song.sources = (str(z['target']), str(z['donor']))
        return song

    def slices(self, idx, shift=None):
        end = self.end[idx] + (shift if shift is not None else 0)
        frames = torch.from_numpy(end[:, None] - np.arange(SLICE)[::-1][None, :].copy()).to(self.dev)
        x = self.spec[frames]  # (B, SLICE, BANDS)
        shape = x - x.amax(dim=(1, 2), keepdim=True)
        new = x - self.base[torch.from_numpy(np.asarray(idx)).to(self.dev)][:, None, :]
        chans = [shape, new] + [e[frames] for e in self.extra]
        return torch.stack(chans, 1).transpose(2, 3) / 20.0  # (B, CHANNELS, BANDS, SLICE frames)


class Net(nn.Module):
    """Three 3x3 conv layers; time is max-pooled into TBINS bins at the end, frequency position is kept."""

    def __init__(self):
        super().__init__()
        self.conv = nn.Sequential(nn.Conv2d(CHANNELS, 16, 3, padding=1), nn.ReLU(), nn.MaxPool2d(2),
                                  nn.Conv2d(16, 32, 3, padding=1), nn.ReLU(), nn.MaxPool2d(2),
                                  nn.Conv2d(32, 32, 3, padding=1), nn.ReLU())
        self.pool = nn.AdaptiveMaxPool2d((BANDS // 4, TBINS))
        self.head = nn.Sequential(nn.Linear(32 * (BANDS // 4) * TBINS, 32), nn.ReLU(), nn.Linear(32, 1))

    def forward(self, x):
        h = self.pool(self.conv(x))  # (B, 32, BANDS / 4, TBINS)
        return self.head(h.flatten(1)).squeeze(1)


def device():
    """KICK_GOAL_NN_DEVICE=cpu|mps; default the GPU when there is one."""
    want = os.environ.get('KICK_GOAL_NN_DEVICE')
    return torch.device(want or ('mps' if torch.backends.mps.is_available() else 'cpu'))


def train(songs, seed, synth=()):
    """A net fitted on the songs' training rows: song-equal, class-balanced sampling.
    Kick-swap clips (synth) together get as many draws as the real songs, spread evenly."""
    rng = np.random.default_rng(seed)
    torch.manual_seed(seed)
    dev = device()
    net = Net().to(dev)
    opt = torch.optim.AdamW(net.parameters(), lr=LR, weight_decay=1e-4)
    rows = [(s, np.flatnonzero(s.mask & (s.y == 1)), np.flatnonzero(s.mask & (s.y == 0))) for s in list(songs) + list(synth)]
    per_song = max(1, int(np.median([len(p) for _, p, _ in rows[:len(songs)]])))
    per_clip = max(1, per_song * len(songs) // max(1, len(synth)))
    for _ in range(EPOCHS):
        batch_x, batch_y = [], []
        for k, (song, pos, neg) in enumerate(rows):
            draws = per_song if k < len(songs) else per_clip
            for idx, y in ((pos, 1.0), (neg, 0.0)):
                if not len(idx):
                    continue
                # Project-vouched non-kicks (kick_goal_melodic) are drawn HARD_W times as often.
                w = np.where(song.hard[idx], HARD_W, 1.0) if y == 0.0 else np.ones(len(idx))
                pick = rng.choice(idx, draws, p=w / w.sum())
                batch_x.append(song.slices(pick, rng.integers(-JITTER, JITTER + 1, len(pick))))
                batch_y.append(torch.full((len(pick),), y, device=song.dev))
        x, y = torch.cat([b.to(dev) for b in batch_x]), torch.cat([b.to(dev) for b in batch_y])
        order = torch.from_numpy(rng.permutation(len(y))).to(dev)
        net.train()
        for s in range(0, len(y), BATCH):
            b = order[s:s + BATCH]
            xb, yb = x[b], y[b]
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
