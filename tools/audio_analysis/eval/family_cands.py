#!/usr/bin/env python3
"""Candidate recall for a family's labels: what share of its prominent events has a candidate within 35 ms, and how
many candidates per second it costs. Two causal finders (detector_cands): 'rise' (band energy rises over the last
4 hops' minimum) and 'change' (net bands in the family range rise over their own last-20 ms mean: catches pitch
changes too), plus their union.
Usage: family_cands.py FAMILY   (bass or synth; reads GOAL/{FAMILY}_labels.json)"""
import importlib
import json
import sys
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parents[3]))
import numpy as np  # noqa: E402

RISES = (3.0, 4.5, 6.0)
CHANGES = (1.0, 1.5, 2.0, 3.0)


def union(a, b, refractory=6):
    h = np.unique(np.concatenate([a, b]))
    keep, last = [], -100
    for x in h:
        if x - last >= refractory:
            keep.append(x)
            last = x
    return np.array(keep, int)


def main():
    from tools.audio_analysis.eval.detector_cands import HOP, band_hops, peak_candidates, rise_candidates, spectral_rise
    from tools.audio_analysis.eval.detector_eval import LAB_TOL, nearest
    from tools.audio_analysis.eval.detector_songs import audio, rate, spec
    from tools.audio_analysis.eval.kick_goal_eval import GOAL, Goal
    fam = importlib.import_module(f'tools.audio_analysis.eval.{sys.argv[1]}_labels').FAMILY
    lab = json.loads((GOAL / f'{fam.name}_labels.json').read_text())
    g = Goal(mode='v3')
    rows = {}
    dur = 0.0
    for t, s in lab.items():
        if not s['positives']:
            continue
        sr = rate(g, t)
        x = audio(g, t)
        dur += len(x) / sr
        pos = np.array(s['positives'])
        e = band_hops(x, sr, *fam.band)
        v = spectral_rise(spec(g, t), sr, *fam.band)
        found = {}
        for r in RISES:
            found[f'rise {r}'] = rise_candidates(e, r, 4, 6, 8)[0]
        for c in CHANGES:
            found[f'change {c}'] = peak_candidates(v, c, 6, 8)[0]
        found['union rise 4.5 + change 1.5'] = union(found['rise 4.5'], found['change 1.5'])
        for k, cand in found.items():
            on = (cand + 1) * HOP / sr
            hit = int(np.sum(nearest(pos, on) <= LAB_TOL)) if len(on) else 0
            a = rows.setdefault(k, [0, 0, 0])
            a[0], a[1], a[2] = a[0] + hit, a[1] + len(pos), a[2] + len(cand)
        print(f"{t:18s} {len(pos):5d} events | " + ' '.join(f"{k.split()[0][0]}{k.split()[-1]}:{int(np.sum(nearest(pos, (c + 1) * HOP / sr) <= LAB_TOL)) / len(pos):.2f}"
                                                          for k, c in found.items() if len(c)), flush=True)
    for k, (h, n, c) in rows.items():
        print(f'{fam.name} {k:28s}: recall {h / n:.3f} ({h}/{n}), {c / dur:.1f} candidates/s', flush=True)


if __name__ == '__main__':
    main()
