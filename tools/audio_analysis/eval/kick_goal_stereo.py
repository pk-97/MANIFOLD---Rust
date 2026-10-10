"""The side signal ((L - R) / 2) of each song's mix, on the mono mix's timeline.

Built from the same sources as run_kick_goal_tail_component.mix_audio: the fixture
or master file, else the sum of the mix parts (stems, premasters, a WIP). A mono
part adds nothing. Kicks sit dead centre; plucks, leads and many loops are wide.
"""
from __future__ import annotations

import math

import numpy as np
import soundfile as sf
from scipy.signal import resample_poly

from tools.audio_analysis.eval.kick_goal_labels import GOAL


def _side(path, sr):
    x, rate = sf.read(str(path), dtype='float64', always_2d=True)
    s = (x[:, 0] - x[:, 1]) / 2 if x.shape[1] > 1 else np.zeros(len(x))
    if rate != sr:
        d = math.gcd(rate, sr)
        s = resample_poly(s, sr // d, rate // d)
    return s


def side_audio(g, t):
    path = GOAL / f'side_{t}.npy'
    if not path.exists():
        r = g.records[t]
        sr = r['sample_rate']
        info = g.labels['new'].get(t)
        if info is None:
            srcs = [r['source']['audio_path']]
        elif info.get('mix_path'):
            srcs = [info['mix_path']]
        else:
            srcs = info['mix_parts']
        parts = [_side(p, sr) for p in srcs]
        out = np.zeros(max(len(p) for p in parts))
        for p in parts:
            out[:len(p)] += p
        np.save(path, out.astype(np.float32))
    return np.load(path).astype(np.float64)
