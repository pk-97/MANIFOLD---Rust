"""Model file part `stage.*`: the song-history stage.

Reference: lab/stage.py (inputs, gated) over eval/kick_goal_selfsim.py self_features. Windows in file order, window 0
feeds the gate; the forest reads 1 + W x (3 + has_level) inputs. The cuts are written only when the gate is on.
"""
from __future__ import annotations

import numpy as np

from tools.audio_analysis import kick_container
from tools.audio_analysis.eval.kick_goal_selfsim import POWER
from tools.audio_analysis.lab.export.trees import FOREST, forest

PREFIX = 'stage.'
# kick_goal_selfsim.self_features clips lp at 1e-6; lab/stage.py gated clips q at 1e-6.
LP_CLIP = 1e-6
Q_CLIP = 1e-6


def names(gate):
    base = ('windows_s', 'shape', 'level', 'gate', 'power', 'lp_clip', 'q_clip', *FOREST)
    return frozenset(PREFIX + n for n in base + (('cut_detector', 'cut_stage') if gate > 0 else ()))


def n_inputs(windows, has_level):
    return 1 + windows * (3 + has_level)


def entries(record: dict) -> dict[str, np.ndarray]:
    st = record['stage']
    assert st['power'] == POWER, 'the record was fitted under another history weight power'
    lv = st['level']
    out = {
        **forest(st['model'], PREFIX, n_inputs(len(st['windows_s']), lv is not None)),
        'stage.windows_s': np.asarray(st['windows_s'], np.float64),
        'stage.shape': kick_container.text(','.join(st['shape'])),
        'stage.level': kick_container.text(f'{lv[0]}:{lv[1]}' if lv else ''),
        'stage.gate': np.asarray(float(st['gate'])),
        'stage.power': np.asarray(float(st['power'])),
        'stage.lp_clip': np.asarray(LP_CLIP),
        'stage.q_clip': np.asarray(Q_CLIP),
    }
    if st['gate'] > 0:
        out['stage.cut_detector'] = np.asarray(float(st['cuts'][0]))
        out['stage.cut_stage'] = np.asarray(float(st['cuts'][1]))
    return out
