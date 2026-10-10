#!/usr/bin/env python3
"""Stem-only audit of master labels with no event in the named kick stem.

For each reviewed master label, measure the 45-140 Hz rise (40 ms max over the
preceding 50 ms median, dB) and level in the kick, drums and bass stems, using
the per-song stem lag from kick_night_snap_labels (lags derived without labels).
No detector output is read until the classification is frozen; detector status
is joined afterwards for reporting only.
"""
from __future__ import annotations

import json
import pickle
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[3]))

import numpy as np  # noqa: E402
from scipy.signal import resample_poly  # noqa: E402

from tools.audio_analysis.eval.kick_attack_rejection import read_audio  # noqa: E402
from tools.audio_analysis.eval.kick_night_common import NIGHT, Data, missed_ids  # noqa: E402
from tools.audio_analysis.eval.kick_night_snap_labels import STEMS  # noqa: E402
from tools.audio_analysis.eval.kick_night_stem_audit import band_db, rise  # noqa: E402

OTHER = {'late_night': {'drums': 'LATE NIGHT STEMS/Late Night - Drums Stem.wav', 'bass': 'LATE NIGHT STEMS/Late Night - Bass and Sub Stem.wav'},
         'midnight_patience': {'drums': 'MIDNIGHT PATIENCE STEMS/Drums.wav', 'bass': 'MIDNIGHT PATIENCE STEMS/Bass and Sub.wav'},
         'miracle': {'drums': 'MIRACLE STEMS/Drums.wav'},
         'heavy_on_mind': {'drums': 'HEAVY ON MIND STEMS/DRUMS.wav', 'bass': 'HEAVY ON MIND STEMS/BASS AND SUB.wav'}}


def main():
    d = Data()
    snap = json.loads((NIGHT / 'snapped_labels_frozen.json').read_text())
    report = {}
    for track, info in snap['tracks'].items():
        sr, _ = read_audio(d.records[track]['source']['audio_path'])
        lag = info['song_lag_ms'] / 1000
        stems = {'kick': info['kick_stem'], **OTHER[track]}
        db = {}
        for name, rel in stems.items():
            ssr, x = read_audio(str(STEMS / rel))
            if ssr != sr:
                x = resample_poly(x, sr, ssr)
            db[name] = band_db(x, sr)
        rows = []
        for x in info['labels']:
            t = x['midpoint_s'] - lag
            row = dict(passage=x['passage'], t=x['midpoint_s'], kick_event=x['physical_s'] is not None)
            for name in db:
                r_, lvl = rise(db[name], t - .01)
                row[f'{name}_rise'], row[f'{name}_level'] = round(r_, 1), round(lvl, 1)
            if row['kick_event']:
                cls = 'kick_stem'
            elif row.get('drums_rise', -99) >= 9 and row.get('drums_level', -99) >= -45:
                cls = 'drums_stem_low_attack'
            elif row.get('bass_rise', -99) >= 6:
                cls = 'bass_attack_only'
            else:
                cls = 'no_stem_low_attack'
            row['class'] = cls
            rows.append(row)
        report[track] = rows
    frozen = NIGHT / 'label_source_audit.json'
    frozen.write_text(json.dumps(report, indent=1))
    with open(NIGHT / 'replay.pkl', 'rb') as f:
        replay = pickle.load(f)
    for track, rows in report.items():
        missed = {n: {x[1] for x in missed_ids(replay[n][1][track])} for n in ('baseline', 'h18')}
        counts = {}
        for row in rows:
            c = counts.setdefault(row['class'], dict(n=0, h18_missed=0, baseline_missed=0))
            c['n'] += 1
            c['h18_missed'] += round(row['t'], 4) in missed['h18']
            c['baseline_missed'] += round(row['t'], 4) in missed['baseline']
        for cls, c in counts.items():
            print(track, cls, c)
        odd = [r for r in rows if r['class'] in ('bass_attack_only', 'no_stem_low_attack')]
        print('   examples', [(r['t'], r['class'], r.get('drums_rise'), r.get('drums_level'), r.get('bass_rise')) for r in odd[:8]])


if __name__ == '__main__':
    main()
