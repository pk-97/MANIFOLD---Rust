#!/usr/bin/env python3
"""Snare candidate finder (causal) and its recall on the snare labels.

Per 256-sample hop at 48 kHz: log energy of the 1-8 kHz band over the hop's last 5 ms. A candidate is a hop where
that energy rises at least RISE_DB above the minimum of the previous LOOKBACK hops and is the first such hop since the
energy last fell back (one candidate per attack, REFRACTORY hops apart). Availability = candidate + DEADLINE hops,
as for kicks. Usage: snare_cands.py [RISE_DB ...]  (prints recall within 35 ms and candidates per second)."""
import json
import sys
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parents[3]))
import numpy as np  # noqa: E402
from scipy.signal import butter, sosfilt  # noqa: E402

HOP, LOOKBACK, REFRACTORY, DEADLINE = 256, 4, 6, 8


def band_hops(x, sr, lo=1000, hi=8000):
    y = sosfilt(butter(4, (lo, hi), btype='band', fs=sr, output='sos'), x) ** 2
    k = int(.005 * sr)
    c = np.concatenate([[0.0], np.cumsum(y)])
    ends = np.arange(HOP, len(x) + 1, HOP)
    return 10 * np.log10((c[ends] - c[ends - k]) / k + 1e-12)


def candidates(x, sr, rise_db=6.0):
    e = band_hops(x, sr)
    cand, last, armed = [], -100, True
    for h in range(LOOKBACK, len(e)):
        r = e[h] - e[h - LOOKBACK:h].min()
        if r < rise_db / 2:
            armed = True
        if armed and r >= rise_db and h - last >= REFRACTORY:
            cand.append(h)
            last, armed = h, False
    cand = np.array(cand, int)
    return cand, cand + DEADLINE


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
