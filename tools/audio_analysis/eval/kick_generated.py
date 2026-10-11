#!/usr/bin/env python3
"""Kick training songs from Peter's generated label sets (generated_sets), one song per section, by the project
kicks' rules (kick_labels): positives are the kick pad's notes (36), layers within 30 ms one kick; kick rolls
(detector_labels.roll_spans) are unscored and their kicks dropped; every other note (snares, hats, the 808s, bass and
leads) more than 105 ms from every kick is a vouched negative. Echoes and tails are deliberately unlabelled, so they
train as non-kicks. Each set's timing is proven against its Drums stem first (generated_sets.timing_proof).
Writes GOAL/kick_labels_generated.json; the lab reads each section from the set's export.
Usage: kick_generated.py"""
import json
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT))
from tools.audio_analysis.lab import recipe  # noqa: E402

recipe.apply()

import numpy as np  # noqa: E402

LAYER_S = .030
EDGE_S = .1   # kicks this close to a section's ends are cut off by the section; neither labelled nor scored


def main():
    from tools.audio_analysis.eval import generated_sets as gs
    from tools.audio_analysis.eval.detector_eval import inside, nearest
    from tools.audio_analysis.eval.detector_labels import builder_id, merge_layers, roll_spans
    from tools.audio_analysis.eval.kick_goal_eval import GOAL
    from tools.audio_analysis.eval.kick_labels import HARD_FAR_S
    out = {}
    for gset in gs.SETS:
        res, start, _ = gs.load(gset)
        proof = gs.timing_proof(gset, res, start)
        if not proof['proven']:
            sys.exit(f'{gset.als}: timing not proven against the Drums stem: {proof}')
        notes = gs.drum_notes(res, start)
        for name, a, b in gs.sections(res, gset, start):
            dur = b - a
            kicks = merge_layers(np.sort([t - a for t, key in notes if key in gs.KICK_KEYS and a <= t < b]), LAYER_S)
            other = np.unique(np.round([t - a for t, key in notes if key not in gs.KICK_KEYS and a <= t < b], 4))
            rolls = [[float(u), float(v)] for u, v in roll_spans(kicks)]
            unscored = rolls + [[0.0, EDGE_S], [dur - EDGE_S, dur]]
            gone = inside(kicks, unscored)
            neg = other[nearest(other, kicks) > HARD_FAR_S] if len(other) else other
            neg = neg[~inside(neg, unscored)]
            out[name] = dict(kind='generated', duration_s=float(dur), positives=[float(u) for u in kicks[~gone]],
                             unscored=unscored, loop_spans=[], hard_neg=[float(u) for u in neg], positives_only=False,
                             dropped=[float(u) for u in kicks[gone]], offset_s=float(a), section=[float(a), float(b)],
                             export=gset.path, proof='generated', timing=proof,
                             sources=dict(generated=int((~gone).sum())), builder=builder_id(__file__))
            print(f'{name:14s} {dur:6.1f} s: {int((~gone).sum()):4d} kicks, {int(gone.sum()):3d} in rolls or at the edges, '
                  f'{len(neg):5d} vouched negatives')
        print(f"{gset.als}: timing median {proof['median_ms']:+.1f} ms over {proof['hits']} isolated drum hits")
    (GOAL / 'kick_labels_generated.json').write_text(json.dumps(out))
    print(f"total {sum(len(s['positives']) for s in out.values())} kicks in {len(out)} songs, "
          f"{sum(s['duration_s'] for s in out.values()) / 60:.1f} min")


if __name__ == '__main__':
    if sys.argv[1:]:
        sys.exit(__doc__)
    main()
