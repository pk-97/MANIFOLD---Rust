#!/usr/bin/env python3
"""What the project plays at a song's scored false snare fires (blend+stage, held-out cutoff): per project track, how
many false fires have one of its notes (or an audio clip start) within 30 ms. Per drum-rack track the count is split by pad name. Usage: snare_what_fires.py SONG [NPZ in GOAL]"""
import json
import os
import sys
from pathlib import Path
from collections import Counter
sys.path.insert(0, str(Path(__file__).resolve().parents[3]))
import numpy as np  # noqa: E402
from tools.audio_analysis.eval.snare_cands import candidates  # noqa: E402
from tools.audio_analysis.eval.snare_net import choose, fires, inside, nearest, rows  # noqa: E402
from tools.audio_analysis.eval.als_extract import extract  # noqa: E402
from tools.audio_analysis.eval.kick_goal_eval import GOAL, Goal  # noqa: E402
from tools.audio_analysis.eval.kick_goal_melodic import _source  # noqa: E402
from tools.audio_analysis.eval.detector_songs import audio, rate, wip  # noqa: E402



def main():
    t = sys.argv[1]
    g = Goal(mode='v3')
    lab = json.loads((GOAL / 'snare_labels.json').read_text())
    z = np.load(GOAL / (sys.argv[2] if len(sys.argv) > 2 else 'snare_full_s0.npz'))
    key = 'staged|{}' if 'full' in (sys.argv[2] if len(sys.argv) > 2 else 'full') else '{}'
    D = {}
    for u in lab:
        sr = rate(g, u)
        cand, avail = candidates(audio(g, u), sr, 4.5)
        D[u] = ((avail + 1) * 256 / sr, z[key.format(u)], avail, sr)
    th = choose([(lab[u], *D[u]) for u in lab if u != t and not lab[u]['positives_only']])
    emit, p, avail, sr = D[t]
    s = lab[t]
    f = emit[fires(p, avail, th, sr)]
    f = f[nearest(f, s['positives']) > .07]
    f = f[~inside(f, s['unscored'])]
    loop = inside(f, s['loop_spans'])
    f = f[~loop | (nearest(f, s['hard_neg']) <= .035)]
    print(f'{t}: {len(f)} false fires at cutoff {th:.2f}')
    W = wip()
    als, off = (W[t]['als'], W[t]['offset_s']) if t in W else _source(t)
    res = extract(als)
    c = Counter()
    for tr in res['tracks']:
        pads = {}

        def walk(devs):
            for d in devs or []:
                for b in d.get('branches') or []:
                    if b.get('key') is not None:
                        pads[b['key']] = b.get('name') or ''
                    walk(b.get('devices'))
        walk(tr.get('devices'))
        by = {}
        for cl in tr['midi_clips']:
            for n in cl['notes']:
                by.setdefault(pads.get(n['key'], ''), []).append(n['s'] + off)
        for pad, notes in by.items():
            c[tr['name'] + (f' [{pad}]' if pad else '')] = int(np.sum(nearest(f, notes) <= .03))
    starts = {}
    for cl in res['audio_clips']:
        starts.setdefault(cl['track'], []).append(cl['start_s'] + off)
    for k, v in starts.items():
        c['audio: ' + k] = int(np.sum(nearest(f, v) <= .03))
    print('   ', ', '.join(f'{k} {v}' for k, v in c.most_common(10) if v))
    print('    fire times:', np.round(f[:25], 2).tolist())


if __name__ == '__main__':
    main()
