"""Model file part `net.*`: the nets' shared input and each net's weights (docs/DETECTOR_RUNTIME_DESIGN.md section 4,
D17).

The 64 band-pass SOS sections exactly as scipy's butter returns them, each band's moving-average length, the slice
and baseline constants, the frame channels in order, then net k's conv and linear weights as f32 in PyTorch layout
under net.<k>.. Reference: eval/kick_goal_nn.py (spectrum, Song, slices, Net), imported under the record's own
net env (lab/release.py applies it), so SLICE and TBINS are the record's.
"""
from __future__ import annotations

import numpy as np
from scipy.signal import butter

from tools.audio_analysis import kick_container
from tools.audio_analysis.eval import kick_goal_nn as ref

PREFIX = 'net.'
SR = 48000
HOP = 256
WEIGHTS = ('conv.0.weight', 'conv.0.bias', 'conv.3.weight', 'conv.3.bias', 'conv.6.weight', 'conv.6.bias',
           'head.0.weight', 'head.0.bias', 'head.2.weight', 'head.2.bias')
INPUT_NAMES = frozenset(PREFIX + n for n in (
    'sos', 'k', 'sample_rate', 'hop', 'frame_s', 'frame_hop', 'slice', 'tbins', 'channels', 'pre_s', 'span', 'ahead_s',
    'extra', 'count'))


def names(count):
    return INPUT_NAMES | {f'{PREFIX}{k}.{w}' for k in range(count) for w in WEIGHTS}


def entries(record: dict) -> dict[str, np.ndarray]:
    net = record['net']
    assert net['past_ms'] == ref.PAST_MS and ref.BANDS == 64, 'kick_goal_nn read another net env than the record'
    assert not ref.PERC_ON and not ref.STEREO_ON and not ref.FLAT_ON, 'channels come from the record, not env'
    channels = 2 + len(net['channels'])
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
        'net.tbins': np.array(ref.TBINS, np.int64),
        'net.channels': np.array(channels, np.int64),
        'net.pre_s': np.array(ref.PRE_S, np.float64),
        'net.span': np.array(int(ref.PRE_S / ref.FRAME_S), np.int64),
        'net.ahead_s': np.array(net['ahead_ms'] / 1000, np.float64),
        'net.extra': kick_container.text(','.join(net['channels'])),
        'net.count': np.array(len(net['state_dicts']), np.int64),
    }
    shapes = None
    for n, sd in enumerate(net['state_dicts']):
        assert set(sd) == set(WEIGHTS), sorted(sd)
        got = {w: np.shape(sd[w]) for w in WEIGHTS}
        assert shapes in (None, got), f'net {n} has other weight shapes than net 0'
        shapes = got
        for w in WEIGHTS:
            out[f'net.{n}.{w}'] = np.ascontiguousarray(np.asarray(sd[w], np.float32))
    return out
