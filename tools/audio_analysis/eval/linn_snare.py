#!/usr/bin/env python3
"""Snare labels from Peter's clean labels project (note_clean.PROJECT, read only): its four LinnDrum kits (133 of 416
rack settings differ between them: kit FX, boom, distortion, decay, reverb) become four extra snare songs, one per
section of the full mix. Positives: snare and clap pads (keys 38, 39). Vouched negatives: every other drum pad
(hats, toms, rims, cowbell, congas...) and every bass and lead note more than 70 ms from a snare. Fast runs are
unscored with the snare family's rule (detector_labels.dense_spans), their snares kept for dense training.
Writes GOAL/snare_wip/linn_{1..4}.npy (48 kHz mix audio, the snare WIP audio path) and GOAL/snare_labels_linn.json;
snare_net reads them with SNARE_LINN=train|eval.
Usage: linn_snare.py"""
import json
import sys
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parents[3]))
import numpy as np  # noqa: E402

SNARE_KEYS = {38, 39}
SR = 48000


def main():
    from tools.audio_analysis.eval.als_extract import extract
    from tools.audio_analysis.eval.detector_labels import dense_spans, inside, read_any
    from tools.audio_analysis.eval.kick_goal_eval import GOAL, load
    from tools.audio_analysis.eval.note_clean import PROJECT
    from tools.audio_analysis.eval.snare_labels import FAMILY
    res = extract(PROJECT / 'Labels and Timings.als')
    start = min(c['start_s'] for tr in res['tracks'] for c in tr['midi_clips'])
    x = read_any(str(PROJECT / 'Exports' / 'Labels and Timings - V1 FULL MIX.wav'), SR, load)
    others = np.sort([n['s'] - start for tr in res['tracks'] if 'LinnDrum' not in tr['name']
                      for c in tr['midi_clips'] for n in c['notes']])
    out = {}
    (GOAL / 'snare_wip').mkdir(exist_ok=True)
    for k, tr in enumerate([t for t in res['tracks'] if 'LinnDrum' in t['name']], 1):
        a = min(c['start_s'] for c in tr['midi_clips']) - start
        b = max(c['end_s'] for c in tr['midi_clips']) - start
        notes = [(n['s'] - start - a, n['key']) for c in tr['midi_clips'] for n in c['notes']]
        pos = np.sort([t for t, key in notes if key in SNARE_KEYS])
        pos = pos[np.concatenate([[True], np.diff(pos) > FAMILY.layer_s])] if len(pos) else pos
        neg = np.concatenate([[t for t, key in notes if key not in SNARE_KEYS], others[(others >= a) & (others < b)] - a])
        neg = neg[np.min(np.abs(neg[:, None] - pos[None, :]), axis=1) > FAMILY.far_s] if len(pos) else neg
        dense = dense_spans(pos, FAMILY)
        dur = b - a
        name = f'linn_{k}'
        np.save(GOAL / 'snare_wip' / f'{name}.npy', x[int(a * SR):int(b * SR)].astype(np.float32))
        keep = ~inside(pos, dense) & (pos > .1) & (pos < dur - .1)
        out[name] = dict(kind='project', duration_s=float(dur), positives=sorted(map(float, pos[keep])), unscored=[list(map(float, d)) for d in dense],
                         loop_spans=[], hard_neg=sorted(map(float, np.unique(np.round(neg, 4)))), positives_only=False,
                         dense_pos=sorted(map(float, pos[inside(pos, dense)])), dropped=[], dense=dense, timing_shift_ms=0.0, offset_s=float(a))
        print(f'{name} ({tr["name"]}): {len(pos)} snares and claps ({int(keep.sum())} scored), {len(neg)} vouched negatives, {dur:.1f} s')
    (GOAL / 'snare_labels_linn.json').write_text(json.dumps(out))


if __name__ == '__main__':
    main()
