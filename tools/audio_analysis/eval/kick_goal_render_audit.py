#!/usr/bin/env python3
"""Render the v2 label audit set: windows chosen from the labels, not from any detector.

Per own-stem dev song: up to three 4 s windows holding the most removed labels.
Per original-five song: one 6.5 s window holding the most flagged onsets (else
the first 6.5 s of scored audio). Per new song: 4 s windows at 25/50/75% of the
song, plus the densest drum-bus-only window when there are any.
"""
from __future__ import annotations

import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[3]))

import numpy as np  # noqa: E402

from tools.audio_analysis.eval.kick_goal_labels import GOAL, NEW, fresh_onsets, kick_env_db, load  # noqa: E402
from tools.audio_analysis.eval.kick_goal_view import render  # noqa: E402


def densest(times, width, k, taken=()):
    times = np.sort(np.asarray(times))
    out = []
    for _ in range(k):
        best = None
        for t in times:
            n = int(np.sum((times >= t - .5) & (times < t - .5 + width)))
            s = t - .5
            if any(abs(s - u) < width for u in out + list(taken)):
                continue
            if best is None or n > best[1]:
                best = (s, n)
        if best is None or best[1] == 0:
            break
        out.append(best[0])
    return out


def main():
    labels = json.loads((GOAL / 'labels_v2.json').read_text())
    out_dir = GOAL / 'views/audit'
    out_dir.mkdir(parents=True, exist_ok=True)
    index = []
    for song, row in labels['dev'].items():
        if row['group'] == 'original_five':
            starts = densest(row['flagged_missing'], 6.5, 1) or [max(0.0, min(r['t'] for r in row['kept']) - .5)]
            wins = [(s, 6.5, 'flagged onsets' if row['flagged_missing'] else 'overview') for s in starts]
        else:
            wins = [(s, 4.0, 'removed labels') for s in densest([r['t'] for r in row['removed']], 4.0, 3)]
        for s, w, why in wins:
            p = out_dir / f'{song}_{s:.1f}.png'
            render(p, song, s, w)
            index.append(dict(song=song, start=round(s, 2), dur=w, why=why, png=str(p)))
            print(p, why, flush=True)
    for song, info in labels['new'].items():
        dur = info['duration_s']
        wins = [(f * dur, 4.0, f'{int(f * 100)}% of song') for f in (.25, .5, .75)]
        if info['drum_bus_only_kicks']:
            cfg, sr = NEW[song], info['sample_rate']
            stem = (fresh_onsets(kick_env_db(load(cfg['kick'], sr), sr)) + info['kick_lag_ms'] / 1000
                    if cfg['kick'] is not None else np.array([]))
            only = [t for t in info['labels'] if not len(stem) or np.min(np.abs(stem - t)) > .07]
            wins += [(s, 4.0, 'drum-bus-only kicks') for s in densest(only, 4.0, 1, [w[0] for w in wins])]
        for s, w, why in wins:
            p = out_dir / f'{song}_{s:.1f}.png'
            render(p, song, s, w)
            index.append(dict(song=song, start=round(s, 2), dur=w, why=why, png=str(p)))
            print(p, why, flush=True)
    (out_dir / 'index.json').write_text(json.dumps(index, indent=1))


if __name__ == '__main__':
    main()
