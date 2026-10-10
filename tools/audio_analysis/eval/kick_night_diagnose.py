#!/usr/bin/env python3
"""Night diagnosis: where misses come from, how songs shift, where scorers disagree.

Read-only over frozen outer-fold scores. Per-song oracle cutoffs use labels and
are diagnostic only; they never select a detector setting.
"""
from __future__ import annotations

import json
import pickle
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[3]))

import numpy as np  # noqa: E402
from scipy.special import expit  # noqa: E402

from tools.audio_analysis.eval.kick_night_common import (  # noqa: E402
    NIGHT, TRACKS, Data, baseline_logit, h16_logit, h18_logit, run_track, summarise)

TOL = .070


def scored_labels(source):
    """(id, time) of every scored label plus scored intervals and ignored times."""
    if source['group'] == 'original_five':
        regions = [(r['start_s'], r['end_s']) for r in source['regions']]
        labels = [(source['track'], t) for t in source['truth']
                  if not any(a <= t <= b for a, b in regions)]
        return labels, [(0.0, 1e9)], regions, []
    labels, cores, excl, ignored = [], [], [], []
    for p in source['passages']:
        ex = [(r['start_s'] - .07, r['end_s'] + .2) for r in p.get('uncertain_regions', [])]
        excl += ex
        cores.append((p['start_s'], p['end_s']))
        for t in p['kick_times_s']:
            if any(a <= t <= b for a, b in ex):
                ignored.append(t)
            elif p['start_s'] <= t < p['end_s']:
                labels.append((p['id'], t))
            else:
                ignored.append(t)
    return labels, cores, excl, ignored


def emission_times(record):
    return (np.asarray(record['available']) + 1) * record['hop'] / record['sample_rate']


def auc(pos, neg):
    if len(pos) == 0 or len(neg) == 0:
        return float('nan')
    allv = np.concatenate([pos, neg])
    ranks = np.argsort(np.argsort(allv)) + 1
    return float((ranks[:len(pos)].sum() - len(pos) * (len(pos) + 1) / 2) / (len(pos) * len(neg)))


def song_stats(d, track, logit_fn, cutoff_prob):
    r = d.records[track]
    em = emission_times(r)
    lg = logit_fn(d, track, track)
    labels, cores, excl, ignored = scored_labels(r['source'])
    every_label = [t for _, t in labels] + ignored
    in_core = np.array([any(a <= e < b for a, b in cores) and not any(a <= e <= b for a, b in excl) for e in em])
    near = np.array([any(-.035 <= e - t <= .2 for t in every_label) for e in em]) if every_label else np.zeros(len(em), bool)
    neg = lg[in_core & ~near]
    pos, has_cand = [], 0
    for _, t in labels:
        m = np.abs(em - t) <= TOL
        if m.any():
            has_cand += 1
            pos.append(lg[m].max())
    pos = np.asarray(pos)
    cut = float(np.log(cutoff_prob / (1 - cutoff_prob)))
    return dict(labels=len(labels), with_timely_candidate=has_cand, negatives=int(len(neg)),
                auc=round(auc(pos, neg), 4), pos_median=round(float(np.median(pos)), 3) if len(pos) else None,
                neg_p95=round(float(np.percentile(neg, 95)), 3) if len(neg) else None,
                neg_p99=round(float(np.percentile(neg, 99)), 3) if len(neg) else None,
                nested_cut_logit=round(cut, 3),
                pos_below_cut=int(np.sum(pos < cut)), neg_above_cut=int(np.sum(neg >= cut)))


def oracle(d, track, logit_fn):
    """Per-song label-informed cutoff maximising matches minus extras at 70 ms."""
    r = d.records[track]
    p = expit(logit_fn(d, track, track))
    best = None
    for th in np.unique(np.quantile(p, np.linspace(.5, 1, 201))):
        _, passages = run_track(r, p, th)
        m = sum(x['accuracy_by_tolerance_ms']['70']['matched'] for x in passages)
        e = sum(x['accuracy_by_tolerance_ms']['70']['extra'] for x in passages)
        if best is None or m - e > best[1] - best[2]:
            best = (float(th), m, e)
    return dict(cut_logit=round(float(np.log(best[0] / (1 - best[0]))), 3), matched=best[1], extra=best[2])


def miss_causes(d, track, logit_fn, cutoff_prob, oracle_logit, passages):
    """Classify each missed label at 70 ms by the first stage that lost it."""
    r = d.records[track]
    em = emission_times(r)
    lg = logit_fn(d, track, track)
    cut = float(np.log(cutoff_prob / (1 - cutoff_prob)))
    fires = set(run_track(r, expit(lg), cutoff_prob)[0])
    fire_em = np.array(sorted((i + 1) * r['hop'] / r['sample_rate'] for i in fires))
    out = {}
    for p in passages:
        for t in p['accuracy_by_tolerance_ms']['70']['missed_times_s']:
            m = np.abs(em - t) <= TOL
            if not m.any():
                cause = 'no_timely_candidate'
            elif lg[m].max() >= cut:
                near_fire = fire_em[(fire_em >= t - .2) & (fire_em <= t + .2)] if len(fire_em) else []
                cause = 'refractory_or_matching' if len(near_fire) else 'passing_but_unemitted'
            elif lg[m].max() >= oracle_logit:
                cause = 'cutoff_transfer'
            else:
                cause = 'ranking'
            out.setdefault(cause, []).append(round(t, 3))
    return out


def extra_kinds(d, track, passages):
    r = d.records[track]
    labels, cores, excl, ignored = scored_labels(r['source'])
    every = [t for _, t in labels] + ignored
    kinds = {'late_or_duplicate_near_label': 0, 'kick_free_core': 0, 'unrelated': 0}
    for p in passages:
        free = p['labels'] == 0 and p['id'] != track
        for e in p['accuracy_by_tolerance_ms']['70']['extra_times_s']:
            if any(-.035 <= e - t <= .2 for t in every):
                kinds['late_or_duplicate_near_label'] += 1
            elif free:
                kinds['kick_free_core'] += 1
            else:
                kinds['unrelated'] += 1
    return kinds


def main():
    d = Data()
    with open(NIGHT / 'replay.pkl', 'rb') as f:
        replay = pickle.load(f)
    fns = dict(baseline=baseline_logit, h16=h16_logit, h18=lambda dd, o, t: h18_logit(dd, o, t))
    report = {}
    for name, fn in fns.items():
        s, by_track, fires = replay[name]
        rows = {}
        causes_total = {}
        for t in TRACKS:
            st = song_stats(d, t, fn, s['cutoffs'][t])
            orc = oracle(d, t, fn)
            causes = miss_causes(d, t, fn, s['cutoffs'][t], orc['cut_logit'], by_track[t])
            for k, v in causes.items():
                causes_total[k] = causes_total.get(k, 0) + len(v)
            rows[t] = dict(stats=st, oracle=orc, nested=dict(matched=s['per_track'][t][0], extra=s['per_track'][t][1]),
                           miss_causes={k: len(v) for k, v in causes.items()}, missed=causes,
                           extra_kinds=extra_kinds(d, t, by_track[t]))
        report[name] = dict(tracks=rows, miss_causes_total=causes_total,
                            oracle_total=dict(matched=sum(r['oracle']['matched'] for r in rows.values()),
                                              extra=sum(r['oracle']['extra'] for r in rows.values())))
        print('==', name, 'miss causes', causes_total, 'oracle', report[name]['oracle_total'])
        for t, r in rows.items():
            st = r['stats']
            print(f"  {t[:18]:18s} L{st['labels']:3d} cand{st['with_timely_candidate']:3d} auc {st['auc']:.3f} "
                  f"pos_med {st['pos_median']} neg95 {st['neg_p95']} neg99 {st['neg_p99']} cut {st['nested_cut_logit']} "
                  f"oracle_cut {r['oracle']['cut_logit']} nested {r['nested']['matched']}/{r['nested']['extra']} "
                  f"oracle {r['oracle']['matched']}/{r['oracle']['extra']} causes {r['miss_causes']} extras {r['extra_kinds']}")
    (NIGHT / 'diagnosis.json').write_text(json.dumps(report, indent=1))


if __name__ == '__main__':
    main()
