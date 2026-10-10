#!/usr/bin/env python3
"""Spectral shape of labelled versus unlabelled low-band drum-stem onsets.

Model-independent: onsets come from the drum stem alone (kick_night_stem_audit).
A kick concentrates its first 40 ms below 140 Hz; a snare/clap carries most
energy above 140 Hz with broadband noise above 1 kHz.
"""
from __future__ import annotations

import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[3]))

import numpy as np  # noqa: E402
from scipy.signal import butter, sosfilt  # noqa: E402

from tools.audio_analysis.eval.kick_attack_rejection import read_audio  # noqa: E402
from tools.audio_analysis.eval.kick_night_common import NIGHT  # noqa: E402

AUDIO = Path('/Users/peterkiemann/MANIFOLD - Rust/tests/fixtures/audio')
BANDS = dict(sub=(30, 60), low=(60, 140), body=(140, 400), mid=(400, 1000), high=(1000, 8000))


def energies(x, sr, t):
    a, b = int((t - .005) * sr), int((t + .04) * sr)
    out = {}
    for name, (lo, hi) in BANDS.items():
        sos = butter(4, (lo, hi), btype='band', fs=sr, output='sos')
        y = sosfilt(sos, x[max(0, a - int(.2 * sr)):b])[-(b - a):]
        out[name] = float(np.sum(y ** 2))
    return out


def main():
    audit = json.loads((NIGHT / 'stem_audit.json').read_text())
    report = {}
    for track, row in audit.items():
        if track == 'apricots_128bpm':
            continue
        sr, x = read_audio(str(AUDIO / track / 'drums.wav'))
        from tools.audio_analysis.eval.kick_night_stem_audit import band_db, onsets
        on = onsets(band_db(x, sr))
        labels = [f for f in row['fires']]  # unused; labels come from coverage below
        unl = {o[0] for o in row['drum_onsets_unlabeled']}
        rows = []
        for t, r_db, lvl in on:
            e = energies(x, sr, t)
            tot = sum(e.values()) + 1e-20
            rows.append(dict(t=t, unlabeled=t in unl, rise_db=r_db, level_db=lvl,
                             below140_share=round((e['sub'] + e['low']) / tot, 3),
                             high_share=round(e['high'] / tot, 3), body_share=round(e['body'] / tot, 3)))
        lab = [r for r in rows if not r['unlabeled']]
        un = [r for r in rows if r['unlabeled']]
        def med(rs, k):
            return round(float(np.median([r[k] for r in rs])), 3) if rs else None
        print(f"== {track}: labelled {len(lab)} unlabelled {len(un)}")
        for k in ('below140_share', 'body_share', 'high_share', 'level_db'):
            print(f"   {k}: labelled {med(lab, k)} [min {min((r[k] for r in lab), default=None)}] "
                  f"unlabelled {med(un, k)} [max {max((r[k] for r in un), default=None)}]")
        kick_like = [r for r in un if r['below140_share'] >= min(r2['below140_share'] for r2 in lab)]
        print(f"   unlabelled onsets with labelled-range low share: {[(r['t'], r['below140_share'], r['level_db']) for r in kick_like]}")
        report[track] = rows
    (NIGHT / 'drum_onset_shape.json').write_text(json.dumps(report, indent=1))


if __name__ == '__main__':
    main()
