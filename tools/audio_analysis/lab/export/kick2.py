"""Model file part `kick2.*`: the kick verdict a model reads (lab/kick_verdict.py), and the kick model it was
trained against. The runtime feeds it the kick's decisions and refuses any other kick.
"""
from __future__ import annotations

import numpy as np

from tools.audio_analysis import kick_container
from tools.audio_analysis.lab import kick_verdict

PREFIX = 'kick2.'
NAMES = frozenset(PREFIX + n for n in ('window_s', 'none_dt_ms', 'kick_train_id'))


def entries(record: dict) -> dict[str, np.ndarray]:
    return {
        'kick2.window_s': np.array([kick_verdict.BEFORE_S, kick_verdict.AFTER_S], np.float64),
        'kick2.none_dt_ms': np.array(kick_verdict.NONE_DT_MS, np.float64),
        'kick2.kick_train_id': kick_container.text(record['kick_train_id']),
    }
