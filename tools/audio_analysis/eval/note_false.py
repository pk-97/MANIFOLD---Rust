#!/usr/bin/env python3
"""What the false fires of a family's held-out run sit on. Each false fire (results JSON from detector_net, field
false_s) is put on the nearest project event within NEAR: a kick, or a note of a known part (part_classes); fires on
nothing are split by the time since the last kick (60-300 ms after a kick: a sidechain return). Prints the split over
all songs and per song, worst first.
Usage: note_false.py FAMILY RESULTS.json [RESULTS.json...]"""
import importlib
import json
import sys
from collections import Counter
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parents[3]))
import numpy as np  # noqa: E402

NEAR = .050


def song_events(g, t, lab):
    """{name: sorted mix times}: kicks and every part's note starts, shifted like the labels."""
    from tools.audio_analysis.eval.als_extract import extract
    from tools.audio_analysis.eval.detector_labels import vouched
    from tools.audio_analysis.eval.detector_songs import wip
    from tools.audio_analysis.eval.kick_goal_melodic import _source
    from tools.audio_analysis.eval.note_labels import project_notes
    W = wip()
    src = (W[t]['als'], W[t]['offset_s']) if t in W else _source(t)
    res = extract(src[0])
    shift = lab['offset_s'] + lab['timing_shift_ms'] / 1000
    out = {}
    for s, _, _, part, _ in project_notes(res):
        out.setdefault(part, []).append(s + shift)
    out['kick'] = list(vouched(g, t, W, src, res if t in W else None)[0])
    return {k: np.sort(np.array(v)) for k, v in out.items()}


def main():
    from tools.audio_analysis.eval.detector_eval import nearest
    from tools.audio_analysis.eval.kick_goal_eval import GOAL, Goal
    fam = importlib.import_module(f'tools.audio_analysis.eval.{sys.argv[1]}_labels').FAMILY
    labels = json.loads((GOAL / f'{fam.name}_labels.json').read_text())
    runs = [json.loads(Path(p).read_text())['results']['net']['per_song'] for p in sys.argv[2:]]
    g = Goal(mode='v3')
    total, per_song = Counter(), {}
    for t in labels:
        fires = [np.array(r[t]['false_s']) for r in runs if t in r and 'false_s' in r[t]]
        if not fires:
            continue
        ev = song_events(g, t, labels[t])
        c = Counter()
        for f in np.concatenate(fires):
            d = {k: nearest(np.array([f]), v)[0] for k, v in ev.items() if len(v)}
            k = min(d, key=d.get) if d and min(d.values()) <= NEAR else None
            if k is None:
                kicks = ev.get('kick', np.zeros(0))
                before = kicks[kicks <= f]
                k = 'nothing, 60-300 ms after a kick' if len(before) and .06 <= f - before[-1] <= .3 else 'nothing'
            c[k] += 1
        per_song[t] = c
        total += c
    n = sum(total.values())
    print(f'{fam.name}: {n} false fires over {len(runs)} runs: ' + ', '.join(f'{k} {v / n:.0%}' for k, v in total.most_common()))
    for t, c in sorted(per_song.items(), key=lambda kv: -sum(kv[1].values()))[:12]:
        m = sum(c.values())
        print(f'  {t:18s} {m:5d}: ' + ', '.join(f'{k} {v}' for k, v in c.most_common(4)))


if __name__ == '__main__':
    main()
