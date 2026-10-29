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
# KICK_GOAL_NN_FLAT=1: per-band noisiness (detector_channels.flatness) as an extra channel, after perc and side.
FLAT_ON = os.environ.get('KICK_GOAL_NN_FLAT') == '1'
# The net's slice ends this much after the emission; the fire keeps the emission's time stamp, so the wait is
# accepted output latency (Peter, Oct 2026: 20-40 ms is fine), not timing error eaten from the 70 ms tolerance.
AHEAD_MS = int(os.environ.get('KICK_GOAL_NN_AHEAD_MS', '0'))
CENTRES = np.geomspace(30.0, 16000.0, BANDS)
HALF_BW = .1 * 64 / BANDS  # about one band spacing either side
FRAME_S = .002
SLICE = PAST_MS // 2  # frames ending at emission
TBINS = 1 if SLICE <= 75 else 4
CHANNELS = 2 + PERC_ON + STEREO_ON + FLAT_ON
DEFAULT_INPUT = BANDS == 64 and PAST_MS == 150 and not PERC_ON and not STEREO_ON and not FLAT_ON and not AHEAD_MS
INPUT_TAG = '' if DEFAULT_INPUT else (f'_b{BANDS}' + (f'_h{PAST_MS}' if PAST_MS != 150 else '') + ('_perc' if PERC_ON else '')
                                     + ('_st' if STEREO_ON else '') + ('_flat' if FLAT_ON else '') + (f'_a{AHEAD_MS}' if AHEAD_MS else ''))
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
    return (10 * np.log10(per ** 2 / (per ** 2 + sus ** 2 + 1e-18) + 1e-6)).astype(np.float32)


def extra_caches(g, t, spec):
    """The extra channels' full-song arrays (n_frames, BANDS), each cached per song."""
    out = []
    if PERC_ON:
        path = GOAL / f'nnperc{_band_tag()}_{t}.npy'
        if not path.exists():
            np.save(path, percussive(spec).astype(np.float16))
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
    if FLAT_ON:
        from tools.audio_analysis.eval.detector_channels import flat_cache
        flat = flat_cache(g, t)
        full = np.zeros_like(spec)
        n = min(len(flat), len(spec))
        full[:n] = flat[:n]
        out.append(full)
    return out


class Song:
    """A spectrum, its candidates' slice anchors and training rows (mask, labels)."""

    def __init__(self, spec, onset_s, emit_s, mask, y, name='', hard=None, extra=(), neg_w=None, pos_w=None):
        self.spec, self.mask, self.y, self.name = torch.from_numpy(spec), mask, y, name
        # Per-candidate draw weights beyond the vouched-negative one (lab/models.py): None draws them evenly.
        self.neg_w, self.pos_w = neg_w, pos_w
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
        self.end_t, self.back = torch.from_numpy(self.end), torch.arange(SLICE - 1, -1, -1)
        self.dev = torch.device('cpu')

    def to(self, dev):
        """Keeps the spectrum, extra channels, baselines and slice anchors on dev, so slices are cut there."""
        self.spec, self.base, self.dev = self.spec.to(dev), self.base.to(dev), dev
        self.end_t, self.back = self.end_t.to(dev), self.back.to(dev)
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
        """Candidates idx (array or tensor), each slice moved shift frames; cut on the song's device."""
        idx = torch.as_tensor(idx, device=self.dev)
        end = self.end_t[idx]
        if shift is not None:
            end = end + torch.as_tensor(shift, device=self.dev)
        frames = end[:, None] - self.back[None, :]
        x = self.spec[frames]  # (B, SLICE, BANDS)
        shape = x - x.amax(dim=(1, 2), keepdim=True)
        new = x - self.base[idx][:, None, :]
        chans = [shape, new] + [e[frames] for e in self.extra]
        return torch.stack(chans, 1).transpose(2, 3) / 20.0  # (B, CHANNELS, BANDS, SLICE frames)


class Net(nn.Module):
    """Three 3x3 conv layers; time is max-pooled into TBINS bins at the end, frequency position is kept."""

    def __init__(self, channels=CHANNELS):
        super().__init__()
        self.conv = nn.Sequential(nn.Conv2d(channels, 16, 3, padding=1), nn.ReLU(), nn.MaxPool2d(2),
                                  nn.Conv2d(16, 32, 3, padding=1), nn.ReLU(), nn.MaxPool2d(2),
                                  nn.Conv2d(32, 32, 3, padding=1), nn.ReLU())
        self.pool = nn.AdaptiveMaxPool2d((BANDS // 4, TBINS))
        self.head = nn.Sequential(nn.Linear(32 * (BANDS // 4) * TBINS, 32), nn.ReLU(), nn.Linear(32, 1))

    def forward(self, x):
        h = self.pool(self.conv(x))  # (B, 32, BANDS / 4, TBINS)
        return self.head(h.flatten(1)).squeeze(1)


def device():
    """KICK_GOAL_NN_DEVICE=cpu|mps|cuda; default the GPU when there is one (an NVIDIA card, else the Mac's)."""
    want = os.environ.get('KICK_GOAL_NN_DEVICE')
    if want:
        return torch.device(want)
    if torch.cuda.is_available():
        return torch.device('cuda')
    return torch.device('mps' if torch.backends.mps.is_available() else 'cpu')


def draw_weights(song, idx, y):
    """The relative draw weights of candidates idx of one song for class y: project-vouched non-kicks
    (kick_goal_melodic) are drawn HARD_W times as often; the song's neg_w / pos_w, when set, scale the rest."""
    if y == 0.0:
        w = np.where(song.hard[idx], HARD_W, 1.0)
        return w if song.neg_w is None else w * song.neg_w[idx]
    return np.ones(len(idx)) if song.pos_w is None else song.pos_w[idx]


def train(songs, seed, synth=(), epochs=EPOCHS, synth_share=0.5, swa_epochs=0, batch=BATCH, lr=LR, precision='float32'):
    """A net fitted on the songs' training rows: song-equal, class-balanced sampling.
    A song may be a list of versions (the song first, then its copies: gain or mix variants): the versions share one
    song's draws, each draw from a version picked by the training rng; the draw count follows the first version's.
    A bare Song (or a one-version list) draws exactly as it always has.
    Clips (synth) together get synth_share of all draws (0.5: as many as the real songs), spread evenly.
    swa_epochs > 0 returns the equal-weight average of the weights at the end of each of the last swa_epochs epochs.
    precision 'mixed' runs forward and loss in float16 (autocast) with float32 weights and a loss scaler."""
    rng = np.random.default_rng(seed)
    torch.manual_seed(seed)
    dev = device()
    songs = [list(s) if isinstance(s, (list, tuple)) else [s] for s in songs]
    net = Net(2 + len(songs[0][0].extra)).to(dev)
    # Averaged weights are only valid as they stand without batch statistics to recompute.
    assert not any(isinstance(m, nn.modules.batchnorm._BatchNorm) for m in net.modules())
    assert precision in ('float32', 'mixed') and 0 <= swa_epochs <= epochs
    opt = torch.optim.AdamW(net.parameters(), lr=lr, weight_decay=1e-4)
    mixed = precision == 'mixed'
    scaler = torch.amp.GradScaler(dev.type) if mixed else None
    avg = None
    rows = [[(s, np.flatnonzero(s.mask & (s.y == 1)), np.flatnonzero(s.mask & (s.y == 0))) for s in g]
            for g in songs + [[c] for c in synth]]
    per_song = max(1, int(np.median([len(g[0][1]) for g in rows[:len(songs)]])))
    per_clip = max(1, int(round(per_song * len(songs) * synth_share / (1 - synth_share))) // max(1, len(synth)))
    for e in range(epochs):
        drawn, ys = [], []
        for k, versions in enumerate(rows):
            draws = per_song if k < len(songs) else per_clip
            if len(versions) > 1:
                _draw_versions(rng, versions, draws, drawn, ys)
                continue
            song, pos, neg = versions[0]
            picks = []
            for idx, y in ((pos, 1.0), (neg, 0.0)):
                if not len(idx):
                    continue
                w = draw_weights(song, idx, y)
                pick = rng.choice(idx, draws, p=w / w.sum())
                picks.append(np.stack([pick, rng.integers(-JITTER, JITTER + 1, len(pick))]))
                ys.append(np.full(len(pick), y, np.float32))
            if picks:
                drawn.append((song, np.concatenate(picks, axis=1)))
        # Songs stay on the CPU: each epoch's slices are cut there bit-identically to the GPU, and only the per-batch
        # tensors move to the net's device (a GPU index tensor cannot index a CPU tensor) inside the loop.
        at = torch.from_numpy(np.concatenate([d for _, d in drawn], axis=1))
        parts = at.split([d.shape[1] for _, d in drawn], dim=1)
        x = torch.cat([song.slices(p[0], p[1]) for (song, _), p in zip(drawn, parts)])
        y = torch.from_numpy(np.concatenate(ys))
        order = torch.from_numpy(rng.permutation(len(y)))
        net.train()
        for s in range(0, len(y), batch):
            b = order[s:s + batch]
            xb, yb = x[b].to(dev), y[b].to(dev)
            opt.zero_grad()
            if mixed:
                with torch.autocast(dev.type, dtype=torch.float16):
                    loss = nn.functional.binary_cross_entropy_with_logits(net(xb), yb * (1 - SMOOTH) + SMOOTH / 2)
                scaler.scale(loss).backward()
                scaler.step(opt)
                scaler.update()
            else:
                loss = nn.functional.binary_cross_entropy_with_logits(net(xb), yb * (1 - SMOOTH) + SMOOTH / 2)
                loss.backward()
                opt.step()
        if e >= epochs - swa_epochs:
            now = {k: v.detach().clone() for k, v in net.state_dict().items()}
            avg = now if avg is None else {k: avg[k] + now[k] for k in avg}
    if avg is not None:
        net.load_state_dict({k: v / swa_epochs for k, v in avg.items()})
    return net.eval()


def _draw_versions(rng, versions, draws, drawn, ys):
    """One song's draws spread over its versions [(Song, pos, neg)]: per class, each of the draws picks a version
    that has rows of that class (rng.integers), then each version draws its share as a lone song would."""
    picks = [[] for _ in versions]
    labels = [[] for _ in versions]
    for c, y in ((1, 1.0), (2, 0.0)):
        have = [j for j, v in enumerate(versions) if len(v[c])]
        if not have:
            continue
        who = rng.integers(0, len(have), draws)
        for n, j in zip(np.bincount(who, minlength=len(have)), have):
            if not n:
                continue
            song, idx = versions[j][0], versions[j][c]
            w = draw_weights(song, idx, y)
            pick = rng.choice(idx, n, p=w / w.sum())
            picks[j].append(np.stack([pick, rng.integers(-JITTER, JITTER + 1, len(pick))]))
            labels[j].append(np.full(len(pick), y, np.float32))
    for (song, _, _), p, lab in zip(versions, picks, labels):
        if p:
            drawn.append((song, np.concatenate(p, axis=1)))
            ys.extend(lab)


@torch.no_grad()
def predict(net, song):
    dev = device()
    out = []
    n = len(song.end)
    for s in range(0, n, 2048):
        out.append(torch.sigmoid(net(song.slices(torch.arange(s, min(n, s + 2048), device=song.dev)).to(dev))).cpu())
    return torch.cat(out).numpy().astype(np.float64)
