#!/usr/bin/env python3
"""H-S: does a 16-band early/late rise profile separate kicks from toms, claps, bass notes and impacts? Strict v2.

Why: rendered false fires on the new songs (views/extras) are tom-like pitch
drops (Burn), clap or snare hits with low end (Back to You), bass notes and
section impacts (Pattern). The 15 features see 30-140 Hz as one band; the
per-song oracle cutoff on C1 scores tops out near 82/82, so ranking must improve.
Predeclared, three configs, 13-song nested, whole-song kick-stem truth (as C2):
- S1 gbt53: C2's 21 features + 32 profile features.
- S2 gbt47: the 15 features + 32 profile features.
- S3 gbt68: S1 + the 15 song-normalised features.
Expected failure: rises over sustained bass and pads are noisy, and 13 songs are
too few for 32 more inputs, so dev precision does not move.
Pass vs C2 (dev R .897 P .666; new R .61 P .617, F1 .613): new F1 >= .663 and
dev precision >= .696 with dev recall >= .88.
"""
from __future__ import annotations

import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[3]))

import numpy as np  # noqa: E402

from tools.audio_analysis.eval.kick_goal_eval import (  # noqa: E402
    GOAL, NEW_SONGS, TRACKS, Goal, add_whole_song_truth, nested, summarise)
from tools.audio_analysis.eval.kick_goal_profile import rise_profile  # noqa: E402
from tools.audio_analysis.eval.kick_goal_songnorm import add_normalised  # noqa: E402
from tools.audio_analysis.eval.kick_goal_templates import band_spec  # noqa: E402
from tools.audio_analysis.eval.run_kick_goal_data import ALL, gbt, line  # noqa: E402
from tools.audio_analysis.eval.run_kick_goal_tail_component import mix_audio  # noqa: E402


def profile_cache(g, t):
    path = GOAL / f'prof_{t}.npy'
    if not path.exists():
        r = g.records[t]
        spec = band_spec(mix_audio(g, t), r['sample_rate'], r['hop'])
        np.save(path, rise_profile(spec, r['candidates'], r['available']))
        print('profile', t, flush=True)
    return np.load(path)


def main():
    g = Goal(mode='strict')
    add_whole_song_truth(g)
    add_normalised(g, TRACKS, NEW_SONGS, GOAL)
    for t, r in g.records.items():
        f21 = np.hstack([r['features'], np.load(GOAL / f'tail_{t}.npy'), np.load(GOAL / f'tmpl_{t}.npy')])
        prof = profile_cache(g, t)
        r['f53'] = np.hstack([f21, prof])
        r['f47'] = np.hstack([r['features'], prof])
        r['f68'] = np.hstack([f21, prof, r['f15n']])
    out = {}
    for name, feats in (('S1_gbt53', 'f53'), ('S2_gbt47', 'f47'), ('S3_gbt68', 'f68')):
        fit, pred = gbt(g, True, feats)
        s, outcome, _ = nested(g, fit, pred, tracks=ALL, name=name)
        dev = summarise(g, {t: outcome[t] for t in TRACKS})
        new = summarise(g, {t: outcome[t] for t in NEW_SONGS})
        out[name] = dict(all=s, dev=dev, new=new)
        print(line(name + ' dev', dev), flush=True)
        print(line(name + ' new', new), flush=True)
        print('   per track', [f"{k[:8]} {v['matched']}/{v['labels']}+{v['extra']} c{v['core']}" for k, v in s['per_track'].items()], flush=True)
    (GOAL / 'results_shape.json').write_text(json.dumps(out, indent=1, default=float))


if __name__ == '__main__':
    main()
