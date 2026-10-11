#!/usr/bin/env python3
"""Snare training songs from Peter's generated label sets (generated_sets), one song per section.
Positives: snare and clap pads (keys 38, 39), layers within layer_s one event. Every other note (other pads, bass,
leads) more than far_s from a snare is a vouched negative. Rolls are unscored and untrained, as in the project songs:
snare fast runs (detector_labels.dense_spans, the project songs' rule; their snares kept as dense_pos) and kick rolls
(detector_labels.roll_spans on pad 36). Echoes and tails are deliberately unlabelled, so they train as non-snares.
Writes GOAL/snare_wip/{song}.npy (48 kHz full mix) and GOAL/snare_labels_generated.json; the lab reads them
(lab/registry.py, lab/run.py).
Usage: snare_generated.py"""
import json
import sys
sys.path.insert(0, str(__import__('pathlib').Path(__file__).resolve().parents[3]))
import numpy as np  # noqa: E402

SNARE_KEYS = {38, 39}


def main():
    from tools.audio_analysis.eval import generated_sets as gs
    from tools.audio_analysis.eval.detector_eval import inside
    from tools.audio_analysis.eval.detector_labels import builder_id, dense_spans, roll_spans
    from tools.audio_analysis.eval.kick_goal_eval import GOAL
    from tools.audio_analysis.eval.snare_labels import FAMILY
    out = {}
    (GOAL / 'snare_wip').mkdir(exist_ok=True)
    for als, export, how in gs.SETS:
        res, start, x = gs.load(als, export)
        notes = [(n['s'] - start, n['key'] if 'LinnDrum' in tr['name'] else None) for tr in res['tracks']
                 for c in tr['midi_clips'] for n in c['notes']]
        for name, a, b in gs.sections(res, how, start):
            pos = np.sort([t - a for t, key in notes if key in SNARE_KEYS and a <= t < b])
            pos = pos[np.concatenate([[True], np.diff(pos) > FAMILY.layer_s])] if len(pos) else pos
            neg = np.array([t - a for t, key in notes if key not in SNARE_KEYS and a <= t < b])
            neg = neg[np.min(np.abs(neg[:, None] - pos[None, :]), axis=1) > FAMILY.far_s] if len(pos) and len(neg) else neg
            kicks = np.sort([t - a for t, key in notes if key in gs.KICK_KEYS and a <= t < b])
            unscored = [[float(u), float(v)] for u, v in dense_spans(pos, FAMILY) + roll_spans(kicks)]
            rolls = pos[inside(pos, unscored)]
            pos, neg = pos[~inside(pos, unscored)], neg[~inside(neg, unscored)]
            dur = b - a
            np.save(GOAL / 'snare_wip' / f'{name}.npy', x[int(a * gs.SR):int(b * gs.SR)].astype(np.float32))
            keep = (pos > .1) & (pos < dur - .1)
            out[name] = dict(kind='generated', duration_s=float(dur), positives=sorted(map(float, pos[keep])), unscored=unscored, loop_spans=[],
                             hard_neg=sorted(map(float, np.unique(np.round(neg, 4)))), positives_only=False,
                             dense_pos=sorted(map(float, rolls)), dropped=[], dense=unscored, timing_shift_ms=0.0,
                             offset_s=float(a), section=[float(a), float(b)],
                             export=gs.export_path(export),
                             sources=dict(generated=int(len(pos))), builder=builder_id(__file__))
            print(f'{name:14s} {dur:6.1f} s: {int(keep.sum()):4d} snares and claps, {len(rolls):4d} in rolls (unscored), '
                  f'{sum(v - u for u, v in unscored):5.1f} s of rolls, {len(neg):5d} vouched negatives')
    (GOAL / 'snare_labels_generated.json').write_text(json.dumps(out))
    print(f"total {sum(len(s['positives']) for s in out.values())} snares in {len(out)} songs, {sum(s['duration_s'] for s in out.values()) / 60:.1f} min")


if __name__ == '__main__':
    main()
