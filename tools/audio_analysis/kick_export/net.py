#!/usr/bin/env python3
"""Kick model export hook for the CNN (Rust: crates/manifold-audio/src/kick/net/).

Every number the Rust net needs: the 64 band-pass SOS sections exactly as scipy's
butter returns them, each band's moving-average length, the slice/baseline
constants and every conv/linear weight as f32 in PyTorch layout.
Reference: tools/audio_analysis/eval/kick_goal_nn.py (spectrum, Song, slices, Net).
"""
from __future__ import annotations

import numpy as np
from scipy.signal import butter

from tools.audio_analysis.eval import kick_goal_nn as ref

SR = 48000
HOP = 256
WEIGHTS = ('conv.0.weight', 'conv.0.bias', 'conv.3.weight', 'conv.3.bias', 'conv.6.weight', 'conv.6.bias',
           'head.0.weight', 'head.0.bias', 'head.2.weight', 'head.2.bias')


def model_entries(final: dict) -> dict[str, np.ndarray]:
    net = final['net']
    assert net['bands'] == ref.BANDS == 64 and net['past_ms'] == ref.PAST_MS == 150, 'kick_goal_nn input config differs from the model'
    assert not ref.PERC_ON and not ref.STEREO_ON, 'recipe 1 has two input channels'
    centres = np.geomspace(30.0, 16000.0, net['bands'])
    assert np.array_equal(centres, ref.CENTRES)
    # spectrum()'s grid is round(i * FRAME_S * sr); the Rust stream steps a whole number of samples per frame.
    frame_hop = int(round(ref.FRAME_S * SR))
    i = np.arange(2_000_000)
    assert np.array_equal(np.round(i * ref.FRAME_S * SR).astype(np.int64), i * frame_hop)
    sos = np.stack([butter(2, [fc * (1 - ref.HALF_BW), fc * (1 + ref.HALF_BW)], btype='bandpass', fs=SR, output='sos')
                    for fc in ref.CENTRES]).astype(np.float64)
    k = np.array([max(int(.002 * SR), int(2 * SR / fc)) for fc in ref.CENTRES], np.int64)
    out = {
        'net.sos': sos,
        'net.k': k,
        'net.sample_rate': np.array(SR, np.int64),
        'net.hop': np.array(HOP, np.int64),
        'net.frame_s': np.array(ref.FRAME_S, np.float64),
        'net.frame_hop': np.array(frame_hop, np.int64),
        'net.slice': np.array(ref.SLICE, np.int64),
        'net.pre_s': np.array(ref.PRE_S, np.float64),
        'net.span': np.array(int(ref.PRE_S / ref.FRAME_S), np.int64),
        'net.ahead_s': np.array(net['ahead_ms'] / 1000, np.float64),
    }
    sd = net['state_dict']
    assert set(sd) == set(WEIGHTS), sorted(sd)
    for name in WEIGHTS:
        out[f'net.{name}'] = np.ascontiguousarray(np.asarray(sd[name], np.float32))
    return out
