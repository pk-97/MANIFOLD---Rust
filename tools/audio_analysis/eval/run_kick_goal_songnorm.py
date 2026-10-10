#!/usr/bin/env python3
"""H-N: causal per-song feature normalisation fixes the cutoff transfer? Strict v2.

Why: Cold's kicks rank well inside the song (AUC .935) but sit lower than other
songs' kicks, so a pooled cutoff misses most of them (139/642); within-song
ranking is good everywhere. Predeclared, three configs, 13-song nested,
whole-song kick-stem training truth (as C1):
- N1 gbt30: raw 15 + song-normalised 15.
- N2 gbt15n: song-normalised 15 only.
- N3 gbt31: N1 + pre_rel_db (low end already loud, relative to the past 4 s).
Expected failure: normalising against mostly non-kick candidates erases the
level cues kicks stand out on; cold starts misfire in the first 20 s.
Pass vs C1 (dev R .907 P .681; new R .455 P .485): new-song F1 up >= 0.10 and dev
precision up >= 0.03 with dev recall >= 0.88.
"""
from __future__ import annotations

import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[3]))

from tools.audio_analysis.eval.kick_goal_eval import (  # noqa: E402
    GOAL, NEW_SONGS, TRACKS, Goal, add_whole_song_truth, nested, summarise)
from tools.audio_analysis.eval.kick_goal_songnorm import add_normalised  # noqa: E402
from tools.audio_analysis.eval.run_kick_goal_data import ALL, gbt, line  # noqa: E402

def main():
    g = Goal(mode='strict')
    add_whole_song_truth(g)
    add_normalised(g, TRACKS, NEW_SONGS, GOAL)
    out = {}
    for name, feats in (('N1_gbt30', 'f30'), ('N2_gbt15n', 'f15n'), ('N3_gbt31', 'f31')):
        fit, pred = gbt(g, True, feats)
        s, outcome, _ = nested(g, fit, pred, tracks=ALL, name=name)
        dev = summarise(g, {t: outcome[t] for t in TRACKS})
        new = summarise(g, {t: outcome[t] for t in NEW_SONGS})
        out[name] = dict(all=s, dev=dev, new=new)
        print(line(name + ' dev', dev), flush=True)
        print(line(name + ' new', new), flush=True)
        print('   per track', {k[:8]: (v['matched'], v['labels'], v['extra']) for k, v in s['per_track'].items()}, flush=True)
    (GOAL / 'results_songnorm.json').write_text(json.dumps(out, indent=1, default=float))

if __name__ == '__main__':
    main()
