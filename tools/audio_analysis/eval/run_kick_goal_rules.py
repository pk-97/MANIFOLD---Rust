#!/usr/bin/env python3
"""H-T: does the low-band dip rule stop sub-body retriggers without losing kicks? Strict v2.

Usage: run_kick_goal_rules.py BASE   (nested_{BASE}.npz saved)

Exactly nested on saved predictions: for outer o the cutoff and DIP_DB are
chosen together by pooled F1 over the inner songs scored by models fitted
without o and that song; o is scored by the model fitted without o.
Configs: T0 dip 0 (plain refractory; control), T1 dip chosen from {3, 6, 9, 12} dB.
Expected failure: Cold's long sub bodies never dip 3 dB before the next kick at
125 BPM, so the rule blocks real kicks.
Pass: T1 cuts tail fires by >= 50% versus T0 with pooled F1 not lower.
"""
from __future__ import annotations

import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[3]))

import numpy as np  # noqa: E402
from scipy.special import expit, logit  # noqa: E402

from tools.audio_analysis.eval.kick_goal_eval import (  # noqa: E402
    GOAL, NEW_SONGS, TRACKS, Goal, counts, summarise)
from tools.audio_analysis.eval.kick_goal_lowbank import band_envelopes  # noqa: E402
from tools.audio_analysis.eval.kick_goal_rules import fires_dip, low_envelope  # noqa: E402
from tools.audio_analysis.eval.run_kick_dsp_experiments import evaluate  # noqa: E402
from tools.audio_analysis.eval.run_kick_goal_data import ALL, line  # noqa: E402
from tools.audio_analysis.eval.run_kick_goal_tail_component import mix_audio  # noqa: E402

DIPS = (3.0, 6.0, 9.0, 12.0)


def low_cache(g, t):
    path = GOAL / f'lowenv_{t}.npy'
    if not path.exists():
        np.save(path, low_envelope(band_envelopes(mix_audio(g, t), g.records[t]['sample_rate'])).astype(np.float32))
    return np.load(path)


def run(g, low, t, p, th, dip):
    r = g.records[t]
    idx = fires_dip(p, r['available'], r['onset_s'], r['sample_rate'], r['hop'], th, low[t], dip)
    return idx, evaluate(r['source'], r['emit_s'][idx].tolist())


def choose(g, low, preds, dips):
    allp = np.concatenate(list(preds.values()))
    grid = np.unique(expit(np.quantile(logit(np.clip(allp, 1e-9, 1 - 1e-9)), np.linspace(.5, .9995, 60))))
    best = None
    for dip in dips:
        for th in grid:
            m = e = n = 0
            for t, p in preds.items():
                a, b, c = counts(run(g, low, t, p, th, dip)[1])
                m, e, n = m + a, e + b, n + c
            f1 = 2 * m / max(1, 2 * m + e + (n - m))
            if best is None or f1 > best[2] + 1e-12:
                best = (float(th), dip, f1)
    return best


def main():
    base = sys.argv[1]
    g = Goal(mode='strict')
    nested = np.load(GOAL / f'nested_{base}.npz')
    low = {t: low_cache(g, t) for t in ALL}
    out = {}
    for name, dips in (('T0_refractory', (0.0,)), ('T1_dip', DIPS)):
        outcome, chosen = {}, {}
        for o in ALL:
            th, dip, _ = choose(g, low, {u: nested[f'{o}|{u}'] for u in ALL if u != o}, dips)
            outcome[o] = run(g, low, o, nested[o], th, dip)
            chosen[o] = (th, dip)
        s = summarise(g, outcome)
        s.update(name=name, chosen=chosen)
        out[name] = dict(all=s, dev=summarise(g, {t: outcome[t] for t in TRACKS}),
                         new=summarise(g, {t: outcome[t] for t in NEW_SONGS}))
        for k in ('all', 'dev', 'new'):
            print(line(f'{name} {k}', out[name][k]), flush=True)
        print('   chosen dips', sorted({v[1] for v in chosen.values()}), flush=True)
    (GOAL / f'results_rules_{base}.json').write_text(json.dumps(out, indent=1, default=float))


if __name__ == '__main__':
    main()
