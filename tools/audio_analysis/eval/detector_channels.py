#!/usr/bin/env python3
"""Noisiness channel for the snare net: per net band (kick_goal_nn.CENTRES, +-10%) and 2 ms frame, the spectral
flatness of the band's FFT bins over the last 2048 samples (causal), in dB clipped to [-20, 0]. Noise (a snare's
body) sits near 0 dB; a pitched sound (a synth stab, a bass note) has peaks between quiet bins and sits well below.
Bands narrower than 3 bins use their 3 nearest bins."""
import sys
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parents[3]))
import numpy as np  # noqa: E402

N_FFT, CHUNK = 2048, 4096


def flatness(x, sr):
    from tools.audio_analysis.eval.kick_goal_nn import CENTRES, FRAME_S, HALF_BW
    grid = np.round(np.arange(int(len(x) / sr / FRAME_S)) * FRAME_S * sr).astype(int)
    freqs = np.fft.rfftfreq(N_FFT, 1 / sr)
    M = np.zeros((len(CENTRES), len(freqs)))
    for b, fc in enumerate(CENTRES):
        idx = np.flatnonzero((freqs >= fc * (1 - HALF_BW)) & (freqs <= fc * (1 + HALF_BW)))
        if len(idx) < 3:
            idx = np.argsort(np.abs(freqs - fc))[:3]
        M[b, idx] = 1 / len(idx)
    xp = np.concatenate([np.zeros(N_FFT), x])
    win = np.hanning(N_FFT)
    out = np.zeros((len(grid), len(CENTRES)), np.float32)
    for s in range(0, len(grid), CHUNK):
        g = grid[s:s + CHUNK] + N_FFT  # frame ends at sample grid (exclusive) in the padded signal
        frames = xp[g[:, None] - N_FFT + np.arange(N_FFT)[None, :]] * win
        p = np.abs(np.fft.rfft(frames, axis=1)) ** 2 + 1e-12
        out[s:s + CHUNK] = np.clip(10 * np.log10(np.exp(np.log(p) @ M.T) / (p @ M.T)), -20, 0)
    return out


def flat_cache(g, t):
    from tools.audio_analysis.eval.detector_songs import audio, rate
    from tools.audio_analysis.eval.kick_goal_eval import GOAL
    path = GOAL / f'snare_flat_{t}.npy'
    if not path.exists():
        np.save(path, flatness(audio(g, t), rate(g, t)).astype(np.float16))
    return np.load(path).astype(np.float32)


PITCH_RANGES = ((300, 1000), (1000, 3000), (3000, 8000))
PITCH_GAP = 10  # frames (20 ms)


def pitch_stability(x, sr):
    """Per 2 ms frame and range (PITCH_RANGES): the correlation between this frame's fine spectral structure (log power
    minus its 9-bin moving average) and the structure 20 ms earlier. A held pitch keeps its peaks in the same bins
    (near 1); noise reshuffles them (near 0). Causal: both frames end at or before the frame time."""
    from tools.audio_analysis.eval.kick_goal_nn import FRAME_S
    grid = np.round(np.arange(int(len(x) / sr / FRAME_S)) * FRAME_S * sr).astype(int)
    freqs = np.fft.rfftfreq(N_FFT, 1 / sr)
    sel = [np.flatnonzero((freqs >= lo) & (freqs < hi)) for lo, hi in PITCH_RANGES]
    xp = np.concatenate([np.zeros(N_FFT), x])
    win = np.hanning(N_FFT)
    kern = np.ones(9) / 9
    out = np.zeros((len(grid), len(PITCH_RANGES)), np.float32)
    for s in range(0, len(grid), CHUNK):
        lo = max(0, s - PITCH_GAP)
        g = grid[lo:s + CHUNK] + N_FFT
        frames = xp[g[:, None] - N_FFT + np.arange(N_FFT)[None, :]] * win
        lp = np.log(np.abs(np.fft.rfft(frames, axis=1)) ** 2 + 1e-12)
        smooth = np.apply_along_axis(lambda r: np.convolve(r, kern, mode='same'), 1, lp)
        fine = lp - smooth
        for k, idx in enumerate(sel):
            a = fine[:, idx] - fine[:, idx].mean(axis=1, keepdims=True)
            cur, prev = a[PITCH_GAP:], a[:-PITCH_GAP]
            r = (cur * prev).sum(1) / (np.linalg.norm(cur, axis=1) * np.linalg.norm(prev, axis=1) + 1e-9)
            n0 = s - lo  # frames of this chunk start at row n0 of a
            rows = np.arange(n0, len(a))
            vals = np.zeros(len(rows), np.float32)
            have = rows >= PITCH_GAP
            vals[have] = r[rows[have] - PITCH_GAP]
            out[s:s + len(rows), k] = vals
    return out


def pitch_cache(g, t):
    from tools.audio_analysis.eval.detector_songs import audio, rate
    from tools.audio_analysis.eval.kick_goal_eval import GOAL
    path = GOAL / f'snare_pitch_{t}.npy'
    if not path.exists():
        np.save(path, pitch_stability(audio(g, t), rate(g, t)).astype(np.float16))
    return np.load(path).astype(np.float32)


def pitch_measures(series, cand, avail, sr, hop=256):
    """Per candidate and range: mean and max pitch stability over the frames from the candidate to its availability."""
    from tools.audio_analysis.eval.kick_goal_nn import FRAME_S
    a = np.clip(np.round((cand + 1) * hop / sr / FRAME_S).astype(int), 0, len(series) - 1)
    b = np.clip(np.round((avail + 1) * hop / sr / FRAME_S).astype(int), 0, len(series) - 1)
    out = np.zeros((len(cand), 2 * series.shape[1]), np.float32)
    for n, (i, j) in enumerate(zip(a, b)):
        w = series[i:j + 1]
        out[n] = np.concatenate([w.mean(0), w.max(0)])
    return out
