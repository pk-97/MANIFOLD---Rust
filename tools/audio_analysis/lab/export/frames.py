"""Model file part `flat.*`: the noisiness frame channel (docs/DETECTOR_RUNTIME_DESIGN.md section 4).

Reference: eval/detector_channels.py flatness, per net band (kick_goal_nn.CENTRES, +-HALF_BW) over the last N_FFT
samples on the net's 2 ms grid, in dB clipped to [-20, 0].
"""
from __future__ import annotations

import numpy as np

from tools.audio_analysis.eval import kick_goal_nn as nnm
from tools.audio_analysis.eval.detector_channels import N_FFT

PREFIX = 'flat.'
SR = 48000
CLIP_DB = (-20.0, 0.0)  # detector_channels.flatness clips here
NAMES = frozenset(PREFIX + n for n in ('n_fft', 'window', 'band_lo', 'band_n', 'clip_db'))


def entries(record: dict) -> dict[str, np.ndarray]:
    del record  # no learned numbers
    freqs = np.fft.rfftfreq(N_FFT, 1 / SR)
    lo, cnt = [], []
    for fc in nnm.CENTRES:
        idx = np.flatnonzero((freqs >= fc * (1 - nnm.HALF_BW)) & (freqs <= fc * (1 + nnm.HALF_BW)))
        if len(idx) < 3:
            idx = np.sort(np.argsort(np.abs(freqs - fc))[:3])
        assert np.array_equal(idx, np.arange(idx[0], idx[0] + len(idx))), 'flatness bands must be contiguous bins'
        lo.append(idx[0])
        cnt.append(len(idx))
    return {
        'flat.n_fft': np.array(N_FFT, np.int64),
        'flat.window': np.hanning(N_FFT).astype(np.float64),
        'flat.band_lo': np.array(lo, np.int64),
        'flat.band_n': np.array(cnt, np.int64),
        'flat.clip_db': np.array(CLIP_DB, np.float64),
    }
