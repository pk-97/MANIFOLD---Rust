#!/usr/bin/env python3
"""Snare candidate finder (causal) and its recall on the snare labels.

detector_cands.rise_candidates on the 1-8 kHz band (5 ms energy per 256-sample hop): a rise of at least rise_db over
the minimum of the previous LOOKBACK hops, re-armed when the rise falls back, REFRACTORY hops apart; availability =
candidate + DEADLINE hops, as for kicks. Usage: snare_cands.py [RISE_DB ...]  (prints recall within 35 ms and
candidates per second)."""
import json
import sys
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parents[3]))
import numpy as np  # noqa: E402
from tools.audio_analysis.eval import detector_cands  # noqa: E402
from tools.audio_analysis.eval.detector_cands import HOP  # noqa: E402

LOOKBACK, REFRACTORY, DEADLINE = 4, 6, 8
BAND = (1000, 8000)


def band_hops(x, sr, lo=BAND[0], hi=BAND[1]):
    return detector_cands.band_hops(x, sr, lo, hi)


def candidates(x, sr, rise_db=6.0):
    return detector_cands.rise_candidates(band_hops(x, sr), rise_db, LOOKBACK, REFRACTORY, DEADLINE)


def main():
    from tools.audio_analysis.eval.kick_goal_eval import GOAL, Goal
    from tools.audio_analysis.eval.run_kick_goal_tail_component import mix_audio
    g = Goal(mode='v3')
    lab = json.loads((GOAL / 'snare_labels.json').read_text())
    xs = {t: mix_audio(g, t) for t in lab}
    for rise in [float(a) for a in sys.argv[1:]] or [6.0]:
        hit = tot = n = dur = 0
        per = []
        for t, s in lab.items():
            sr = g.records[t]['sample_rate']
            cand, _ = candidates(xs[t], sr, rise)
            on = (cand + 1) * HOP / sr
            pos = np.array(s['positives'])
            j = np.clip(np.searchsorted(on, pos), 1, max(1, len(on) - 1))
            near = np.minimum(np.abs(pos - on[j - 1]), np.abs(pos - on[np.minimum(j, len(on) - 1)]))
            h = int(np.sum(near <= .035))
            hit, tot, n, dur = hit + h, tot + len(pos), n + len(cand), dur + s['duration_s']
            per.append(f'{t} {h}/{len(pos)}')
        print(f'rise {rise:4.1f} dB: recall {hit / tot:.3f} ({hit}/{tot}), {n / dur:.1f} candidates/s | ' + ', '.join(per), flush=True)


if __name__ == '__main__':
    main()
