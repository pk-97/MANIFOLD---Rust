"""Model file part `perc.*`: the percussive-share frame channel.

Reference: eval/kick_goal_nn.py percussive, on the net's own 64-band spectrum: sustained = each band's median over
the past SUSTAIN frames, percussive = the median across NEIGHBOURS bands of the frame.
"""
from __future__ import annotations

import numpy as np

from tools.audio_analysis.eval import kick_goal_nn as nnm

PREFIX = 'perc.'
NAMES = frozenset(PREFIX + n for n in ('sustain', 'neighbours'))


def entries(record: dict) -> dict[str, np.ndarray]:
    del record  # no learned numbers
    return {'perc.sustain': np.array(nnm.SUSTAIN, np.int64), 'perc.neighbours': np.array(nnm.NEIGHBOURS, np.int64)}
