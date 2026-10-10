#!/usr/bin/env python3
"""Compare kick fast-loop runs (run_kick_goal_fast.py results JSONs) on one song set: pooled F1 of each run's shipped
detector (blend+stage), the mean over seeds per side, and per-song recall change (mean over seeds), worst first.
Recall-only songs count toward recall only. Songs scored on only one side are left out, so a run with more songs
is compared on the songs both sides share.
Usage: kick_fast_compare.py A1.json[,A2.json...] B1.json[,B2.json...] [--key blend+stage]"""
import json
import sys
from pathlib import Path

RECALL_ONLY = ('default_haze', 'lowkey', 'overwhelming_force')


def per_song(path, key):
    return json.loads(Path(path).read_text())['results'][key]['per_song']


def pooled(ps, songs):
    m = sum(ps[t]['matched'] for t in songs)
    n = sum(ps[t]['labels'] for t in songs)
    f = sum(ps[t]['extra'] for t in songs if t not in RECALL_ONLY)
    r, p = m / max(1, n), m / max(1, m + f)
    return r, p, 2 * r * p / max(1e-9, r + p)


def main():
    args = [a for a in sys.argv[1:] if not a.startswith('--')]
    key = sys.argv[sys.argv.index('--key') + 1] if '--key' in sys.argv else 'blend+stage'
    args = [a for a in args if a != key]
    sides = [[per_song(p, key) for p in a.split(',')] for a in args[:2]]
    songs = sorted(set.intersection(*[set(ps) for side in sides for ps in side]))
    means = []
    for name, side in zip('AB', sides):
        f1 = [pooled(ps, songs)[2] for ps in side]
        means.append(sum(f1) / len(f1))
        print(f'{name}: F1 ' + ' '.join(f'{v:.4f}' for v in f1) + f' | mean {means[-1]:.4f} on {len(songs)} songs')
    print(f'B - A: {100 * (means[1] - means[0]):+.2f} F1 points')

    def rec(side, t):
        vals = [ps[t]['matched'] / ps[t]['labels'] for ps in side if ps[t]['labels']]
        return sum(vals) / len(vals) if vals else None
    rows = [(rec(sides[1], t) - rec(sides[0], t), t, sides[0][0][t]['labels']) for t in songs if rec(sides[0], t) is not None]
    rows.sort()
    print('recall change per song (B - A), worst first: ' + ', '.join(f'{t} {100 * d:+.1f} ({n})' for d, t, n in rows[:8]))
    print(f'songs losing more than 5 recall points: {[t for d, t, _ in rows if d < -.05]}')


if __name__ == '__main__':
    main()
