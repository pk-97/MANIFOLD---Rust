#!/usr/bin/env python3
"""Score the song-excluded fires on the extra reviewed passages (D2 and the 73).

Fires come from each song's outer fold (models and cutoffs never saw that song),
so these are development passages of known families, not untouched validation.
The 73 entered other songs' training as coverage in H10 and later; D2 entered
none of the H16/H18/H22 components. Pattern needs new features and is not scored.
"""
from __future__ import annotations

import json
import pickle
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[3]))

from tools.audio_analysis.eval.kick_night_common import NIGHT, Data, fire_times  # noqa: E402
from tools.audio_analysis.eval.verify_kick_evening_passages import score_reviewed_core as score_passage  # noqa: E402

LABELS = Path(__file__).resolve().parents[3] / 'tests/fixtures/audio_labels'
FILES = {'D2': 'evening_extension_passages_2026-10-09.json', 'additional73': 'evening_validation_passages_2026-10-09.json'}


def main():
    d = Data()
    fires = {}
    for name in ('baseline', 'h18', 'h22'):
        with open(NIGHT / f'validate_{name}.pkl', 'rb') as f:
            fires[name] = pickle.load(f)[2]
    report = {}
    for set_name, fname in FILES.items():
        data = json.loads((LABELS / fname).read_text())
        for name in fires:
            tot = dict(labels=0, matched70=0, extra70=0, matched50=0, negative_core_fires=0, per_track={})
            for tr in data['tracks']:
                r = d.records[tr['track']]
                if tr['audio_sha256'] != r['source']['audio_sha256']:
                    raise ValueError('passage audio differs from the scored master')
                times = fire_times(r, fires[name][tr['track']])
                m = e = n = 0
                for core in tr['cores']:
                    if not core['scoring_ready']:
                        continue
                    p = score_passage(times, core)
                    a = p['accuracy_by_tolerance_ms']
                    tot['labels'] += p['labels']; n += p['labels']
                    tot['matched70'] += a['70']['matched']; m += a['70']['matched']
                    tot['extra70'] += a['70']['extra']; e += a['70']['extra']
                    tot['matched50'] += a['50']['matched']
                    if p['labels'] == 0:
                        tot['negative_core_fires'] += a['70']['extra']
                tot['per_track'][tr['track']] = (m, e, n)
            report[f'{set_name}|{name}'] = tot
            print(set_name, name, {k: v for k, v in tot.items() if k != 'per_track'}, tot['per_track'], flush=True)
    (NIGHT / 'extra_passages.json').write_text(json.dumps(report, indent=1))


if __name__ == '__main__':
    main()
