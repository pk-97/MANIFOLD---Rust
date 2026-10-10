#!/usr/bin/env python3
"""Recall/precision/F1 over a song subset from a snare run log's per-song lines. Usage: detector_subset.py LOG SONG..."""
import re
import sys


def main():
    m = n = f = 0
    for line in open(sys.argv[1]):
        g = re.match(r'\s+(\S+)\s+cutoff \S+: (\d+)/(\d+) caught, (\d+) false', line)
        if g and g.group(1) in sys.argv[2:]:
            m, n, f = m + int(g.group(2)), n + int(g.group(3)), f + int(g.group(4))
    r, p = m / max(1, n), m / max(1, m + f)
    print(f'subset R {r:.3f} P {p:.3f} F1 {2 * r * p / max(1e-9, r + p):.3f} ({m}/{n}, {f} false)')


if __name__ == '__main__':
    main()
