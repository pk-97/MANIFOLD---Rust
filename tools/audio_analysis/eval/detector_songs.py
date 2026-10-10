#!/usr/bin/env python3
"""Audio, sample rate and net spectrum for every snare song: the kick goal set's songs, plus the proven 2024/2025 WIP
mixdowns from detector_wip.py (48 kHz, cached in GOAL/snare_wip/)."""
import json
import numpy as np
from tools.audio_analysis.eval.kick_goal_eval import GOAL

SKIP = ('one',)  # its newest mixdown is a Dusk WIP: the same song as dusk


def wip():
    p = GOAL / 'snare_wip.json'
    return {k: v for k, v in json.loads(p.read_text()).items() if k not in SKIP} if p.exists() else {}


def audio(g, t):
    from tools.audio_analysis.eval.run_kick_goal_tail_component import mix_audio
    return np.load(GOAL / 'snare_wip' / f'{t}.npy').astype(np.float64) if t not in g.records else mix_audio(g, t)


def rate(g, t):
    return g.records[t]['sample_rate'] if t in g.records else 48000


def spec(g, t):
    from tools.audio_analysis.eval.kick_goal_nn import spectrum, spectrum_cache
    if t in g.records:
        return spectrum_cache(g, t)
    path = GOAL / f'nnspec_wip_{t}.npy'
    if not path.exists():
        np.save(path, spectrum(audio(g, t), 48000).astype(np.float16))
    return np.load(path).astype(np.float32)
