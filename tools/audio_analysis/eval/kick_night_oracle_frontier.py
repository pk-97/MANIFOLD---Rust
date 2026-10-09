#!/usr/bin/env python3
"""Per-song oracle frontier on the final night scores, with the cases that block 95/95.

Diagnostic only: thresholds here are picked per song with the labels, which no
detector can do. If even this misses the target, the gap is information the
scores do not carry, not cutoff selection.

Per scorer and song, every threshold in (400 score quantiles + each label's best
timely-candidate score) gives (matched, extra) at 70 ms. Reported: the recall
ceiling over all thresholds, the fewest extras reaching 90% and 95% recall, the
best recall at >= 95% precision, and an exact search over per-song choices for
the ultimate target (>= 95% recall and precision pooled, >= 90% per track), with
and without Bad Guy.

Cases, declared before looking: under H22 main, at each song's highest threshold
reaching 90% recall (else its recall ceiling with the fewest extras), every
missed label and every extra gets 45-140 Hz rises (dB, 40 ms max over the prior
50 ms median, the stem-audit rule) in the mix and in each stem, aligned by the
frozen label-free stem lag. Classes:
- missed, kick/drum stem rise < 9 dB: label without a kick-stem attack
- missed, stem rise >= 9 dB, mix rise < 3 dB: masked; the attack is absent from the mix low band
- missed, stem rise >= 9 dB, mix rise >= 3 dB: visible in the mix, ranked below this song's non-kicks
- extra, kick/drum stem rise >= 9 dB: kick-stem attack present (late duplicate or unlabelled kick)
- extra, bass stem rise >= 9 dB: bass-line onset
- extra, neither: other low-band onset
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
from tools.audio_analysis.eval.kick_night_common import (  # noqa: E402
    NIGHT, TRACKS, Data, baseline_logit, h16_logit, h18_logit, run_track)
from tools.audio_analysis.eval.kick_night_diagnose import emission_times, scored_labels  # noqa: E402
from tools.audio_analysis.eval.kick_night_stem_audit import AUDIO, band_db, rise  # noqa: E402
from tools.audio_analysis.eval.run_kick_night_h22 import CONFIGS, load_inputs, make_h22  # noqa: E402

STEMS = Path('/Users/peterkiemann/Library/CloudStorage/Dropbox/Music Production/Ableton Projects/STEMS')
OWN_STEMS = {
    'late_night': dict(kick='LATE NIGHT STEMS/Late Night - Kicks Stem.wav', bass='LATE NIGHT STEMS/Late Night - Bass and Sub Stem.wav'),
    'midnight_patience': dict(kick='MIDNIGHT PATIENCE STEMS/Kick.wav', bass='MIDNIGHT PATIENCE STEMS/Bass and Sub.wav'),
    'miracle': dict(kick='MIRACLE STEMS/Kick.wav', bass='MIRACLE STEMS/Subs.wav'),
    'heavy_on_mind': dict(kick='HEAVY ON MIND STEMS/KICK.wav', bass='HEAVY ON MIND STEMS/BASS AND SUB.wav'),
}
ATTACK_DB, MIX_DB, EMIT_TO_ATTACK = 9.0, 3.0, .045
TOL = .070


def sweep(record, logit, labels):
    p = expit(logit)
    em = emission_times(record)
    marks = [p[np.abs(em - t) <= TOL].max() for t in labels if (np.abs(em - t) <= TOL).any()]
    ths = np.unique(np.concatenate([np.quantile(p, np.linspace(0, 1, 401)), marks]))
    rows = []
    for th in ths:
        _, passages = run_track(record, p, th)
        rows.append((float(th), sum(x['accuracy_by_tolerance_ms']['70']['matched'] for x in passages),
                     sum(x['accuracy_by_tolerance_ms']['70']['extra'] for x in passages)))
    return rows


def frontier(rows):
    """Pareto points (matched, extra): most matches for each extra count."""
    best = {}
    for _, m, e in rows:
        best[e] = max(best.get(e, -1), m)
    pts, top = [], -1
    for e in sorted(best):
        if best[e] > top:
            pts.append((best[e], e))
            top = best[e]
    return pts


def song_summary(rows, n):
    pts = frontier(rows)
    ceiling = max(m for m, _ in pts)

    def fewest(r):
        need = math.ceil(r * n)
        ok = [e for m, e in pts if m >= need]
        return min(ok) if ok else None
    at95p = [m for m, e in pts if m + e > 0 and m / (m + e) >= .95]
    return dict(labels=n, ceiling=ceiling, extras_for_90=fewest(.9), extras_for_95=fewest(.95),
                best_matched_at_95_precision=max(at95p) if at95p else 0, frontier=pts)


def pooled(summaries, tracks, per_track_floor):
    """Exact per-song choice search: for each total extra count, the most total matches."""
    cap = 1500
    dp = {0: 0}
    for t in tracks:
        s = summaries[t]
        floor = math.ceil(.9 * s['labels']) if per_track_floor else 0
        pts = [(m, e) for m, e in s['frontier'] if m >= floor]
        if not pts:
            return dict(feasible=False, reason=f'{t} cannot reach 90% recall at any threshold')
        nxt = {}
        for e0, m0 in dp.items():
            for m, e in pts:
                if e0 + e <= cap and nxt.get(e0 + e, -1) < m0 + m:
                    nxt[e0 + e] = m0 + m
        dp = nxt
    n = sum(summaries[t]['labels'] for t in tracks)
    target = [(m, e) for e, m in dp.items() if m >= .95 * n and m / (m + e) >= .95]
    bal = max(dp.items(), key=lambda kv: min(kv[1] / n, kv[1] / max(1, kv[1] + kv[0])))
    best_at_95p = max([m for e, m in dp.items() if m / max(1, m + e) >= .95], default=0)
    return dict(feasible=bool(target), labels=n, best_balanced=dict(matched=bal[1], extra=bal[0],
                recall=round(bal[1] / n, 3), precision=round(bal[1] / max(1, bal[1] + bal[0]), 3)),
                best_matched_at_95_precision=best_at_95p, best_recall_at_95_precision=round(best_at_95p / n, 3))


def stem_dbs(track, sr_master_len):
    """{name: (band_db array, lag_s)}: stem time t sits at t + lag in the master."""
    if track in OWN_STEMS:
        lags = json.loads((NIGHT / 'snapped_labels_frozen.json').read_text())['tracks']
        lag = lags[track]['song_lag_ms'] / 1000
        out = {}
        for k, rel in OWN_STEMS[track].items():
            sr, x = read_audio(str(STEMS / rel))
            x = np.asarray(x, dtype=np.float64)
            out[k] = (band_db(x.mean(axis=1) if x.ndim > 1 else x, sr), lag)
        return out
    out = {}
    for k, name in (('kick', 'drums'), ('bass', 'bass')):
        sr, x = read_audio(str(AUDIO / track / f'{name}.wav'))
        x = np.asarray(x, dtype=np.float64)
        out[k] = (band_db(x.mean(axis=1) if x.ndim > 1 else x, sr), 0.0)
    return out


def classify(kind, kick_r, bass_r, mix_r):
    if kind == 'missed':
        if not kick_r >= ATTACK_DB:
            return 'label_without_kick_stem_attack'
        return 'masked_in_mix' if mix_r < MIX_DB else 'visible_but_ranked_low'
    if kick_r >= ATTACK_DB:
        return 'kick_stem_attack_present'
    return 'bass_line_onset' if bass_r >= ATTACK_DB else 'other_low_band_onset'


def cases(d, track, logit, rows, n, stems, mix_db):
    r = d.records[track]
    need = math.ceil(.9 * n)
    hit = [x for x in rows if x[1] >= need]
    if hit:
        th, m, e = max(hit, key=lambda x: x[0])
        basis = '90% recall'
    else:
        ceil_m = max(x[1] for x in rows)
        th, m, e = min((x for x in rows if x[1] == ceil_m), key=lambda x: x[2])
        basis = 'recall ceiling'
    p = expit(logit)
    em = emission_times(r)
    _, passages = run_track(r, p, th)
    out = []
    for kind, key, shift in (('missed', 'missed_times_s', 0.0), ('extra', 'extra_times_s', EMIT_TO_ATTACK)):
        for pa in passages:
            for t in pa['accuracy_by_tolerance_ms']['70'][key]:
                a = t - shift
                kr = rise(stems['kick'][0], a - stems['kick'][1])[0]
                br = rise(stems['bass'][0], a - stems['bass'][1])[0]
                mr = rise(mix_db, a)[0]
                near = np.abs(em - t) <= TOL
                out.append(dict(kind=kind, passage=pa['id'], time_s=round(t, 3),
                                best_logit=round(float(logit[near].max()), 2) if kind == 'missed' and near.any() else None,
                                kick_stem_rise_db=round(kr, 1), bass_stem_rise_db=round(br, 1), mix_rise_db=round(mr, 1),
                                cls=classify(kind, kr, br, mr)))
    tally = {}
    for c in out:
        tally[c['cls']] = tally.get(c['cls'], 0) + 1
    return dict(basis=basis, threshold_logit=round(float(np.log(th / (1 - th))), 3), matched=m, extra=e,
                tally=tally, cases=out)


def main():
    d = Data()
    glide, act = load_inputs()
    h22_main, _ = make_h22(d, glide, act, *CONFIGS['h18_gate_glide_lowfit'])
    gate = lambda o, t: act[f'{o}|{t}'] / (act[f'{o}|{t}'] + 5.0)  # noqa: E731
    scorers = {
        'baseline': lambda t: baseline_logit(d, t, t),
        'h16': lambda t: h16_logit(d, t, t),
        'h18': lambda t: h18_logit(d, t, t),
        'h21_h18_gated_linear': lambda t: gate(t, t) * h18_logit(d, t, t) + (1 - gate(t, t)) * d.logits(t, t, 'linear'),
        'h22_main': lambda t: h22_main(t, t),
    }
    report = dict(method=__doc__, scorers={}, cases={})
    labels = {t: [x for _, x in scored_labels(d.records[t]['source'])[0]] for t in TRACKS}
    sweeps = {}
    for name, fn in scorers.items():
        per = {}
        for t in TRACKS:
            rows = sweep(d.records[t], fn(t), labels[t])
            sweeps[(name, t)] = rows
            per[t] = song_summary(rows, len(labels[t]))
        res = dict(per_track={t: {k: v for k, v in s.items() if k != 'frontier'} for t, s in per.items()},
                   pooled_with_track_floor=pooled(per, TRACKS, True),
                   pooled_no_track_floor=pooled(per, TRACKS, False),
                   pooled_without_bad_guy=pooled(per, [t for t in TRACKS if t != 'bad_guy_128bpm'], False),
                   pooled_without_bad_guy_with_track_floor=pooled(per, [t for t in TRACKS if t != 'bad_guy_128bpm'], True))
        report['scorers'][name] = res
        print('==', name, 'ceilings', {t[:6]: f"{s['ceiling']}/{s['labels']}" for t, s in per.items()}, flush=True)
        print('   extras for 90%', {t[:6]: s['extras_for_90'] for t, s in per.items()}, flush=True)
        for k in ('pooled_with_track_floor', 'pooled_no_track_floor', 'pooled_without_bad_guy',
                  'pooled_without_bad_guy_with_track_floor'):
            print('  ', k, res[k], flush=True)
    for t in TRACKS:
        msr, mix = read_audio(d.records[t]['source']['audio_path'])
        mix = np.asarray(mix, dtype=np.float64)
        mix_db = band_db(mix.mean(axis=1) if mix.ndim > 1 else mix, msr)
        stems = stem_dbs(t, len(mix))
        c = cases(d, t, scorers['h22_main'](t), sweeps[('h22_main', t)], len(labels[t]), stems, mix_db)
        report['cases'][t] = c
        print('cases', t, c['basis'], c['matched'], '/', len(labels[t]), '+', c['extra'], c['tally'], flush=True)
    path = NIGHT / 'oracle_frontier.json'
    path.write_text(json.dumps(report, indent=1, default=float))
    print(path)


if __name__ == '__main__':
    main()
