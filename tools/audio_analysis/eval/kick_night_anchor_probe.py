#!/usr/bin/env python3
"""Diagnostic: which label-free song statistic tracks each song's best cutoff?

Uses the per-song oracle cutoffs from kick_night_diagnose.py (label-informed,
diagnostic only). Reports spread of (oracle cutoff - statistic) across the nine
songs; a small spread means that statistic could anchor a song-adaptive cutoff.
"""
from __future__ import annotations

import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[3]))

import numpy as np  # noqa: E402

from tools.audio_analysis.eval.kick_night_common import (  # noqa: E402
    NIGHT, TRACKS, Data, baseline_logit, h16_logit, h18_logit)


def main():
    d = Data()
    diag = json.loads((NIGHT / 'diagnosis.json').read_text())
    fns = dict(baseline=baseline_logit, h16=h16_logit, h18=lambda dd, o, t: h18_logit(dd, o, t))
    out = {}
    for name, fn in fns.items():
        rows = {}
        for t in TRACKS:
            lg = fn(d, t, t)
            r = d.records[t]
            rate = len(lg) / (r['cache_metadata']['duration_s'])
            stats = dict(median=np.median(lg), p90=np.percentile(lg, 90), p95=np.percentile(lg, 95),
                         p98=np.percentile(lg, 98), p99=np.percentile(lg, 99), max=lg.max())
            rows[t] = dict(oracle=diag[name]['tracks'][t]['oracle']['cut_logit'],
                           nested=diag[name]['tracks'][t]['stats']['nested_cut_logit'],
                           pos_median=diag[name]['tracks'][t]['stats']['pos_median'],
                           candidates_per_s=round(rate, 1), **{k: round(float(v), 3) for k, v in stats.items()})
        spread = {}
        for k in ('nested', 'median', 'p90', 'p95', 'p98', 'p99', 'max', 'pos_median'):
            diff = np.array([rows[t]['oracle'] - rows[t][k] for t in TRACKS if t != 'bad_guy_128bpm'])
            spread[k] = dict(mean=round(float(diff.mean()), 3), std=round(float(diff.std()), 3))
        out[name] = dict(rows=rows, spread_without_bad_guy=spread)
        print('==', name, 'std of (oracle - stat), Bad Guy excluded:',
              {k: v['std'] for k, v in spread.items()})
        for t, r in rows.items():
            print(f"  {t[:18]:18s} oracle {r['oracle']:6.2f} nested {r['nested']:6.2f} med {r['median']:6.2f} "
                  f"p95 {r['p95']:6.2f} p99 {r['p99']:6.2f} pos_med {r['pos_median']} rate {r['candidates_per_s']}")
    (NIGHT / 'anchor_probe.json').write_text(json.dumps(out, indent=1))


if __name__ == '__main__':
    main()
