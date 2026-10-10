#!/usr/bin/env python3
"""Which labels force the extras that block 95/95, and what the extras are.

Follows kick_night_oracle_frontier.py under H22 main. Declared before looking:
- Costly labels: per song, labels whose best timely-candidate score sits below the
  song's oracle cutoff (label-informed max of matches minus extras at 70 ms) but
  that the 90%-recall threshold must reach. Each gets the frontier classes:
  no kick-stem attack, masked in the mix, or visible but ranked low.
- Extras on the original five, whose only percussion stem is the full drum stem:
  a drum-stem attack whose first 40 ms puts less 30-140 Hz energy share than the
  smallest share among that song's labelled kick onsets is a non-kick drum hit
  (the onset-shape rule from kick_night_drum_onset_shape.py); otherwise kick-like.
"""
from __future__ import annotations

import json
import math
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[3]))

import numpy as np  # noqa: E402
from scipy.special import expit  # noqa: E402

from tools.audio_analysis.eval.kick_attack_rejection import read_audio  # noqa: E402
from tools.audio_analysis.eval.kick_night_common import NIGHT, TRACKS, Data  # noqa: E402
from tools.audio_analysis.eval.kick_night_diagnose import emission_times, scored_labels  # noqa: E402
from tools.audio_analysis.eval.kick_night_drum_onset_shape import energies  # noqa: E402
from tools.audio_analysis.eval.kick_night_oracle_frontier import (  # noqa: E402
    ATTACK_DB, EMIT_TO_ATTACK, TOL, classify, stem_dbs, sweep)
from tools.audio_analysis.eval.kick_night_stem_audit import AUDIO, band_db, rise  # noqa: E402
from tools.audio_analysis.eval.run_kick_night_h22 import CONFIGS, load_inputs, make_h22  # noqa: E402

ORIGINAL_FIVE = ('apricots_128bpm', 'bad_guy_128bpm', 'feel_the_vibration_174bpm', 'inhale_exhale_145bpm', 'tears_140bpm')


def low_share(x, sr, t):
    e = energies(x, sr, t)
    return (e['sub'] + e['low']) / (sum(e.values()) + 1e-20)


def mono(x):
    x = np.asarray(x, dtype=np.float64)
    return x.mean(axis=1) if x.ndim > 1 else x


def main():
    d = Data()
    glide, act = load_inputs()
    h22, _ = make_h22(d, glide, act, *CONFIGS['h18_gate_glide_lowfit'])
    frontier = json.loads((NIGHT / 'oracle_frontier.json').read_text())
    report = {}
    for t in TRACKS:
        r = d.records[t]
        lg = h22(t, t)
        labels = [x for _, x in scored_labels(r['source'])[0]]
        rows = sweep(r, lg, labels)
        orc = max(rows, key=lambda x: (x[1] - x[2], x[0]))
        cut90 = frontier['cases'][t]['threshold_logit']
        orc_logit = float(np.log(orc[0] / (1 - orc[0])))
        em = emission_times(r)
        msr, mix = read_audio(r['source']['audio_path'])
        mix_db = band_db(mono(mix), msr)
        stems = stem_dbs(t, None)
        costly, classes = [], {}
        for lab in labels:
            near = np.abs(em - lab) <= TOL
            if not near.any():
                continue
            s = float(lg[near].max())
            if cut90 <= s < orc_logit:
                kr = rise(stems['kick'][0], lab - stems['kick'][1])[0]
                mr = rise(mix_db, lab)[0]
                c = classify('missed', kr, 0.0, mr)
                classes[c] = classes.get(c, 0) + 1
                costly.append(dict(time_s=round(lab, 3), logit=round(s, 2), kick_stem_rise_db=round(kr, 1),
                                   mix_rise_db=round(mr, 1), cls=c))
        extras = {}
        case_rows = frontier['cases'][t]['cases']
        if t in ORIGINAL_FIVE:
            dsr, drums = read_audio(str(AUDIO / t / 'drums.wav'))
            drums = mono(drums)
            kick_floor = min(low_share(drums, dsr, lab) for lab in labels)
        for c in case_rows:
            if c['kind'] != 'extra':
                continue
            cls = c['cls']
            if t in ORIGINAL_FIVE and cls == 'kick_stem_attack_present':
                sh = low_share(drums, dsr, c['time_s'] - EMIT_TO_ATTACK)
                c['drum_low_share'] = round(sh, 3)
                cls = 'non_kick_drum_hit' if sh < kick_floor else 'kick_like_drum_hit'
            c['cls_refined'] = cls
            extras[cls] = extras.get(cls, 0) + 1
        report[t] = dict(oracle_cut_logit=round(orc_logit, 3), oracle_matched=orc[1], oracle_extra=orc[2],
                         cut90_logit=cut90, extras_at_90=frontier['cases'][t]['extra'],
                         costly_labels=len(costly), costly_classes=classes, costly=costly,
                         extras_refined=extras, kick_floor_share=round(kick_floor, 3) if t in ORIGINAL_FIVE else None,
                         extra_cases=[c for c in case_rows if c['kind'] == 'extra'])
        print(t, 'oracle', orc[1], '+', orc[2], 'at', round(orc_logit, 2), '| 90% cut', cut90, '+', frontier['cases'][t]['extra'],
              '| costly labels', len(costly), classes, '| extras', extras, flush=True)
    (NIGHT / 'oracle_labels.json').write_text(json.dumps(report, indent=1))


if __name__ == '__main__':
    main()
