#!/usr/bin/env python3
"""Compare two detector configurations, each run over several seeds on fixed song groups, paired song by song.

Reads per-song results JSONs: the kick fast loop's (run_kick_goal_fast.py, key 'blend' by default; 'blend+stage' only
to confirm, it doubles the seed noise) or a family's (detector_eval.write_results, key 'net'). Only songs both sides
scored count. Prints:
- pooled F1 per seed, each side's mean, and the difference with its standard error (seeds are independent draws);
- per song, the hits and false fires each side averages over seeds; a sign test over songs on per-song F1;
- songs losing at least LOSS_HITS hits on average among songs with at least LOSS_LABELS labels (the per-song bar;
  points on a 14-label song are 3 kicks of noise);
- when the results carry them, false fires per scored minute and trigger lateness (median and 90th percentile).
Recall-only songs count toward recall only.
Usage: detector_compare.py A1.json[,A2.json...] B1.json[,B2.json...] [--key KEY]"""
import json
import math
import sys
from pathlib import Path

RECALL_ONLY = ('default_haze', 'lowkey', 'overwhelming_force')
LOSS_HITS, LOSS_LABELS = 5, 30


def per_song(path, key):
    res = json.loads(Path(path).read_text())['results']
    return res[key or ('blend' if 'blend' in res else 'net')]['per_song']


def pooled(ps, songs):
    m = sum(ps[t]['matched'] for t in songs)
    n = sum(ps[t]['labels'] for t in songs)
    f = sum(ps[t]['extra'] for t in songs if t not in RECALL_ONLY)
    r, p = m / max(1, n), m / max(1, m + f)
    return r, p, 2 * r * p / max(1e-9, r + p)


def mean_se(v):
    m = sum(v) / len(v)
    var = sum((x - m) ** 2 for x in v) / max(1, len(v) - 1)
    return m, math.sqrt(var / len(v))


def song_f1(side, t):
    m = sum(ps[t]['matched'] for ps in side) / len(side)
    n = side[0][t]['labels']
    f = 0 if t in RECALL_ONLY else sum(ps[t]['extra'] for ps in side) / len(side)
    r, p = m / max(1, n), m / max(1e-9, m + f)
    return m, f, 2 * r * p / max(1e-9, r + p)


def sign_p(up, down):
    """Two-sided sign test: chance of a split at least this uneven when neither side is better."""
    n, k = up + down, min(up, down)
    return min(1.0, 2 * sum(math.comb(n, i) for i in range(k + 1)) / 2 ** n) if n else 1.0


def timing(side, songs):
    mins = sum(ps[t].get('minutes', 0) for ps in side for t in songs)
    if not mins:
        return ''
    false = sum(ps[t]['extra'] for ps in side for t in songs if t not in RECALL_ONLY)
    late = sorted(x for ps in side for t in songs for x in ps[t].get('late_ms', []))
    q = (lambda a: late[min(len(late) - 1, int(a * len(late)))]) if late else (lambda a: float('nan'))
    return f' | {false / mins:.2f} false per minute | late median {q(.5):.0f} ms, p90 {q(.9):.0f} ms'


def main():
    args = [a for a in sys.argv[1:] if not a.startswith('--')]
    key = sys.argv[sys.argv.index('--key') + 1] if '--key' in sys.argv else None
    args = [a for a in args if a != key]
    sides = [[per_song(p, key) for p in a.split(',')] for a in args[:2]]
    songs = sorted(set.intersection(*[set(ps) for side in sides for ps in side]))
    stats = []
    for name, side in zip('AB', sides):
        f1 = [pooled(ps, songs)[2] for ps in side]
        stats.append(mean_se(f1))
        print(f'{name}: F1 ' + ' '.join(f'{v:.4f}' for v in f1) + f' | mean {stats[-1][0]:.4f} ± {stats[-1][1]:.4f}'
              + timing(side, songs) + f' | {len(songs)} songs')
    diff, se = stats[1][0] - stats[0][0], math.hypot(stats[0][1], stats[1][1])
    print(f'B - A: {100 * diff:+.2f} ± {100 * se:.2f} F1 points')

    rows = []
    for t in songs:
        ma, fa, f1a = song_f1(sides[0], t)
        mb, fb, f1b = song_f1(sides[1], t)
        rows.append((mb - ma, fb - fa, f1b - f1a, t, sides[0][0][t]['labels']))
    up = sum(r[2] > 1e-9 for r in rows)
    down = sum(r[2] < -1e-9 for r in rows)
    print(f'songs better / worse on F1: {up} / {down} (sign test p = {sign_p(up, down):.2f})')
    rows.sort()
    print('hits change per song (B - A, mean over seeds), worst first: '
          + ', '.join(f'{t} {dm:+.1f} hits {df:+.1f} false ({n})' for dm, df, _, t, n in rows[:8]))
    lost = [t for dm, _, _, t, n in rows if dm <= -LOSS_HITS and n >= LOSS_LABELS]
    print(f'songs losing {LOSS_HITS}+ hits (of {LOSS_LABELS}+ labels): {lost}')


if __name__ == '__main__':
    main()
