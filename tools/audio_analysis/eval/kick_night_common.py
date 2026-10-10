"""Shared night-research helpers: frozen logits, causal firing and nested cutoffs.

Every score here is evaluated with the evening protocol unchanged: the 60 ms
refractory at actual availability, emission at the completed hop, whole-song
exclusion from every fit and cutoff, and the coarse grid plus one inner-only
refinement. Only the score fed into that protocol changes between hypotheses.
"""
from __future__ import annotations

import json
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
if str(ROOT) not in sys.path:
    sys.path.insert(0, str(ROOT))

import numpy as np  # noqa: E402
from scipy.special import expit  # noqa: E402

from tools.audio_analysis.eval.kick_fusion_calibration import (  # noqa: E402
    THRESHOLDS, calibration_counts, choose_threshold, select_fires)
from tools.audio_analysis.eval.kick_night_dump import NIGHT, load_inputs, load_records  # noqa: E402
from tools.audio_analysis.eval.run_kick_dsp_experiments import evaluate  # noqa: E402

TRACKS = ('apricots_128bpm', 'bad_guy_128bpm', 'feel_the_vibration_174bpm', 'inhale_exhale_145bpm',
          'tears_140bpm', 'late_night', 'midnight_patience', 'miracle', 'heavy_on_mind')


class Data:
    """Frozen records plus exported outer/inner component logits."""

    def __init__(self):
        self.inputs = load_inputs()
        _, records = load_records(self.inputs)
        self.records = {r['track']: r for r in records}
        with np.load(NIGHT / 'logits.npz', allow_pickle=False) as z:
            self.z = {k: z[k] for k in z.files}
        self.meta = json.loads((NIGHT / 'logits_meta.json').read_text())

    def logits(self, outer, target, component):
        key = 'outer' if outer == target else target
        return self.z[f'{outer}|{key}|{component}']

    def features(self, track):
        return self.z[f'{track}|features']


def h18_logit(d, outer, target, w=.25):
    a, b, c = (d.logits(outer, target, k) for k in ('linear', 'kernel', 'interaction'))
    return (1 - w) * (.25 * a + .75 * b) + w * c


def h16_logit(d, outer, target):
    return d.logits(outer, target, 'kernel')


def baseline_logit(d, outer, target):
    return d.logits(outer, target, 'baseline')


def fire_times(record, fires):
    return [(i + 1) * record['hop'] / record['sample_rate'] for i in fires]


def run_track(record, scores, threshold, gate=None):
    """Return emitted hop indices and evening-format passage scores.

    `gate`, when given, is a boolean per candidate computed from past/current
    audio only; a candidate fires only if both the score and the gate pass.
    """
    s = np.asarray(scores, dtype=np.float64)
    if gate is not None:
        s = np.where(gate, s, -np.inf)
    fires = select_fires(s, record['available'], record['sample_rate'], record['hop'], threshold)
    return fires, evaluate(record['source'], fire_times(record, fires))


def nested_cutoff(d, outer, prob_fn, gate_fn=None):
    """Coarse grid plus one refinement, selected on the eight inner songs only."""
    inner = [t for t in TRACKS if t != outer]
    preds = [(d.records[u], prob_fn(outer, u), gate_fn(outer, u) if gate_fn else None) for u in inner]
    budget = calibration_counts([p for u in inner for p in d.records[u]['ref']['scores']['v5']])

    def counts(th):
        passages = [p for r, s, g in preds for p in run_track(r, s, th, g)[1]]
        c = calibration_counts(passages)
        return dict(threshold=th, **c, feasible=c['wide_extras'] <= budget['wide_extras']
                    and c['kick_free_extras'] <= budget['kick_free_extras'])

    rows = {th: counts(th) for th in THRESHOLDS}
    chosen = choose_threshold(list(rows.values()), budget)
    lower = [r['threshold'] for r in rows.values() if not r['feasible'] and r['threshold'] < chosen['threshold']]
    if lower:
        for th in np.linspace(max(lower), chosen['threshold'], 53)[1:-1].tolist():
            rows[th] = counts(th)
        chosen = choose_threshold([rows[t] for t in sorted(rows)], budget)
    return chosen['threshold'], chosen


def missed_ids(passages, tol='70'):
    """Identities of missed scored labels; the scored label set never changes."""
    return {(p['id'], round(t, 4)) for p in passages
            for t in p['accuracy_by_tolerance_ms'][tol]['missed_times_s']}


def summarise(by_track):
    """Totals at 35/50/70 ms, wide extras, kick-free fires and delays."""
    tot = {ms: dict(matched=0, extra=0) for ms in ('35', '50', '70')}
    wide = kick_free = labels = 0
    delays = []
    per = {}
    for t, passages in by_track.items():
        m70 = sum(p['accuracy_by_tolerance_ms']['70']['matched'] for p in passages)
        e70 = sum(p['accuracy_by_tolerance_ms']['70']['extra'] for p in passages)
        per[t] = (m70, e70, sum(p['labels'] for p in passages))
        labels += per[t][2]
        for ms in tot:
            for k in ('matched', 'extra'):
                tot[ms][k] += sum(p['accuracy_by_tolerance_ms'][ms][k] for p in passages)
        c = calibration_counts(passages)
        wide += c['wide_extras']
        kick_free += c['kick_free_extras']
        delays += [x['delay_ms'] for p in passages for x in p['association_early_35_late_200_ms']['pairs']]
    dl = np.asarray(delays) if delays else np.zeros(1)
    return dict(labels=labels, tol=tot, wide_extras=wide, kick_free_extras=kick_free, per_track=per,
                delay_p50=float(np.median(dl)), delay_p90=float(np.percentile(dl, 90)),
                delay_max=float(dl.max()), late_over_70=int(np.sum(dl > 70)))


def evaluate_scorer(d, prob_fn, gate_fn=None, name=''):
    """Full nested evaluation of one scorer across all nine outer songs."""
    by_track, cutoffs, fires = {}, {}, {}
    for t in TRACKS:
        th, _ = nested_cutoff(d, t, prob_fn, gate_fn)
        f, passages = run_track(d.records[t], prob_fn(t, t), th, gate_fn(t, t) if gate_fn else None)
        by_track[t], cutoffs[t], fires[t] = passages, th, f
    s = summarise(by_track)
    s.update(name=name, cutoffs=cutoffs)
    return s, by_track, fires


def h18_prob(d, w=.25):
    return lambda o, t: expit(h18_logit(d, o, t, w))


def baseline_prob(d):
    return lambda o, t: expit(baseline_logit(d, o, t))


def h16_prob(d):
    return lambda o, t: expit(h16_logit(d, o, t))


def retention(base_by_track, new_by_track):
    """Per-track lost/recovered label identities at 70 ms versus a reference."""
    out = {}
    for t in TRACKS:
        b, n = missed_ids(base_by_track[t]), missed_ids(new_by_track[t])
        out[t] = dict(lost=sorted(n - b), recovered=sorted(b - n))
    return out


def acceptance(summary, ret):
    m, e = summary['tol']['70']['matched'], summary['tol']['70']['extra']
    counts_ok = (m >= 223 and e <= 70) or (m >= 250 and e <= 89)
    worst_loss = max(len(v['lost']) for v in ret.values())
    return dict(counts_ok=counts_ok, cores_clear=summary['kick_free_extras'] == 0,
                retention_ok=worst_loss <= 1, worst_track_loss=worst_loss,
                passes=counts_ok and summary['kick_free_extras'] == 0 and worst_loss <= 1)
