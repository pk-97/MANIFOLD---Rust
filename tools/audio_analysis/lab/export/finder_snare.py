"""Model file part `cand.*`: the snare finder.

Reference: eval/snare_cands.py over detector_cands.band_hops and rise_candidates; every value comes from their own
constants and defaults. The rise in dB is the finder rule's, 'snare:<dB>'.
"""
from __future__ import annotations

import inspect

import numpy as np
from scipy.signal import butter

from tools.audio_analysis.eval import detector_cands, snare_cands

PREFIX = 'cand.'
SR = 48000
NAMES = frozenset(PREFIX + n for n in ('sos', 'window', 'hop', 'rise_db', 'lookback', 'refractory', 'deadline'))


def rise_db(finder):
    kind, db = finder.split(':')
    assert kind == 'snare', finder
    return float(db)


def entries(record: dict) -> dict[str, np.ndarray]:
    band = inspect.signature(detector_cands.band_hops).parameters
    win_s, order = band['win_s'].default, band['order'].default
    i64, f64 = np.int64, np.float64
    return {
        'cand.sos': butter(order, snare_cands.BAND, btype='band', fs=SR, output='sos').astype(f64),
        'cand.window': np.array(int(win_s * SR), i64),
        'cand.hop': np.array(detector_cands.HOP, i64),
        'cand.rise_db': np.array(rise_db(record['finder']), f64),
        'cand.lookback': np.array(snare_cands.LOOKBACK, i64),
        'cand.refractory': np.array(snare_cands.REFRACTORY, i64),
        'cand.deadline': np.array(snare_cands.DEADLINE, i64),
    }
