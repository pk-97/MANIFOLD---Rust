#!/usr/bin/env python3
"""Replay baseline linear15, H16 kernel and H18 .25 through the night helpers.

Acceptance for the helpers: baseline 223 matches + 89 extras and H18 262 + 89 at
70 ms on the 381 labels, identical to the evening scoreboard. Writes per-candidate
outer scores and emitted hops for later diagnosis.
"""
from __future__ import annotations

import json
import pickle
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[3]))

from tools.audio_analysis.eval.kick_night_common import (  # noqa: E402
    NIGHT, Data, acceptance, baseline_prob, evaluate_scorer, h16_prob, h18_prob, retention)


def main():
    d = Data()
    results = {}
    for name, fn in (('baseline', baseline_prob(d)), ('h16', h16_prob(d)), ('h18', h18_prob(d))):
        s, by_track, fires = evaluate_scorer(d, fn, name=name)
        results[name] = (s, by_track, fires)
        print(name, s['tol'], 'wide', s['wide_extras'], 'cores', s['kick_free_extras'],
              'delay', round(s['delay_p50'], 2), round(s['delay_p90'], 2), s['late_over_70'], flush=True)
        print('  per-track', s['per_track'], flush=True)
    for name in ('h16', 'h18'):
        ret = retention(results['baseline'][1], results[name][1])
        print(name, 'acceptance', acceptance(results[name][0], ret))
        print('  lost', {t: v['lost'] for t, v in ret.items() if v['lost']})
    with open(NIGHT / 'replay.pkl', 'wb') as f:
        pickle.dump({k: (v[0], v[1], v[2]) for k, v in results.items()}, f)
    (NIGHT / 'replay_summary.json').write_text(json.dumps({k: v[0] for k, v in results.items()}, indent=1, default=str))


if __name__ == '__main__':
    main()
