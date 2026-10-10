#!/usr/bin/env python3
"""Builds the per-song caches the feature sets need for the campaign 2 training songs.

Usage: KICK_GOAL_MORE=1 KICK_GOAL_TRIGGER=1 KICK_GOAL_TRUTH=v3 run_kick_goal_more_prep.py

Fusion features come from Goal (features_{song}.npz); this adds tail_{song}.npy
and tmpl_{song}.npy. Templates are the dev-song kick templates, as for the new
songs, so no song's own kick shapes its template features.
"""
from __future__ import annotations

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[3]))

import numpy as np  # noqa: E402

from tools.audio_analysis.eval.kick_goal_eval import GOAL, MORE_SONGS, RECALL_SONGS, TRIGGER_SONGS, TRUTH, WIP2_SONGS, WIP_SONGS, Goal  # noqa: E402
from tools.audio_analysis.eval.kick_goal_templates import band_spec, patches, template_features  # noqa: E402
from tools.audio_analysis.eval.run_kick_goal_tail_component import mix_audio, tail_cache  # noqa: E402


def main():
    if not MORE_SONGS + TRIGGER_SONGS + WIP_SONGS + RECALL_SONGS + WIP2_SONGS:
        sys.exit('set KICK_GOAL_MORE=1, KICK_GOAL_TRIGGER=1, KICK_GOAL_WIP=1, KICK_GOAL_WIP2=1 or KICK_GOAL_RECALL=1')
    g = Goal(mode=TRUTH)
    t_dev = np.load(GOAL / 'templates_from_dev.npy')
    for t in MORE_SONGS + TRIGGER_SONGS + WIP_SONGS + RECALL_SONGS + WIP2_SONGS:
        r = g.records[t]
        tail_cache(g, t)
        path = GOAL / f'tmpl_{t}.npy'
        if not path.exists():
            spec = band_spec(mix_audio(g, t), r['sample_rate'], r['hop'])
            np.save(path, template_features(patches(spec, r['candidates'], r['available']), t_dev))
        print(t, 'candidates', len(r['candidates']), 'truth', len(r['source']['truth']), 'stem notes', len(r['stem_kicks'] if r['stem_kicks'] is not None else []), flush=True)


if __name__ == '__main__':
    main()
