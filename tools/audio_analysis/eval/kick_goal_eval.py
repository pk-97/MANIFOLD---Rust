"""Shared evaluation for the 90/90 kick goal on truth v2.

Protocol, fixed before any scoring:
- Dev: nine songs, whole-song nested exclusion. For outer song o, inner models
  are fitted without o and the inner song; the threshold maximises pooled F1 at
  70 ms over the eight inner songs; the outer model (fitted without o) is cut
  there. The 60 ms refractory, emission at the completed availability hop and
  the evening matcher are unchanged.
- New songs: models fitted on all nine dev songs; the threshold maximises pooled
  F1 over the nine dev outer predictions; the new songs are scored once.
- Removed tail labels count as non-kicks: a fire there is an extra.
- Tail fire: an unmatched fire whose candidate onset has no fresh kick-stem
  attack within [-70, +35] ms while the kick stem is ringing (within 30 dB of
  its peak). Core fire: a fire inside a kick-free core (the nine dev cores; on
  new songs, spans of at least 4 s with no label and a silent kick stem).
- Target: pooled recall and precision >= 0.90 at 70 ms, every song >= 0.80
  recall, zero core fires, zero tail fires.
"""
from __future__ import annotations

import copy
import json
from pathlib import Path

import numpy as np
from scipy.special import expit, logit

from tools.audio_analysis.eval.kick_attack_rejection import read_audio
from tools.audio_analysis.eval.kick_fusion_bandwise import fusion_features
from tools.audio_analysis.eval.kick_goal_labels import DEV_STEMS, GOAL, NEW, fresh_onsets, kick_env_db, load
from tools.audio_analysis.eval.kick_night_common import NIGHT, TRACKS, Data
from tools.audio_analysis.eval.run_kick_dsp_experiments import evaluate
from tools.audio_analysis.eval.run_kick_fusion_trial import training_labels

REFRACTORY = .060
NEW_SONGS = ('pattern', 'back_to_you', 'burn_stems', 'cold_remix')
CORE_IDS = ('late_night_bass_heavy', 'midnight_patience_bass_heavy', 'expanded_midnight_patience_s2',
            'expanded_midnight_patience_s4', 'expanded_miracle_s1', 'miracle_bass', 'expanded_miracle_s4',
            'expanded_miracle_s6', 'heavy_on_mind_bass')
BAD_GUY = 'bad_guy_128bpm'


def fires(scores, available, sr, hop, th):
    """Causal firing: passing candidates in availability order, 60 ms refractory."""
    out, last = [], -np.inf
    for i in np.flatnonzero(scores >= th):
        a = available[i]
        if (a - last) * hop / sr >= REFRACTORY - 1e-12:
            out.append(i)
            last = a
    return np.asarray(out, dtype=int)


class Goal:
    """Dev and new-song records on truth v2, with kick-stem tail references."""

    def __init__(self, with_new=True, mode='strict'):
        """mode 'strict': Peter's rule, kick rolls over a ringing tail are non-kicks.
        mode 'loose': those rolls count as kicks (pending Peter's ruling)."""
        self.labels = json.loads((GOAL / 'labels_v2.json').read_text())
        self.mode = mode
        d = Data()
        self.night = d
        self.records = {}
        lags = json.loads((NIGHT / 'snapped_labels_frozen.json').read_text())['tracks']
        for t in TRACKS:
            r = d.records[t]
            src = copy.deepcopy(r['source'])
            row = self.labels['dev'][t]
            removed = {round(x['t'], 4) for x in row['removed'] if mode == 'strict' or x['cls'] != 'kick_tail'}
            if src['group'] != 'original_five':
                for p in src['passages']:
                    p['kick_times_s'] = [x for x in p['kick_times_s'] if round(x, 4) not in removed]
            else:
                src['truth'] = sorted(src['truth'] + row.get('added', []))
                for u in row.get('uncertain', []):
                    if not any(u - .07 <= x <= u + .2 for x in src['truth']):
                        src['regions'] = src['regions'] + [dict(start_s=u - .07, end_s=u + .2, reason='uncertain v2')]
            rec = dict(track=t, source=src, ref=r['ref'], sample_rate=r['sample_rate'], hop=r['hop'],
                       candidates=np.asarray(r['candidates']), available=np.asarray(r['available']),
                       features=np.asarray(d.features(t)), duration=None,
                       removed=sorted(removed), lag=lags[t]['song_lag_ms'] / 1000 if t in lags else 0.0)
            self._finish(rec, DEV_STEMS[t]['kick'] if t in DEV_STEMS else None)
            self.records[t] = rec
        if with_new:
            for name in NEW_SONGS:
                self.records[name] = self._new(name)

    def _new(self, name):
        info = self.labels['new'][name]
        sr = info['sample_rate']
        cache = GOAL / f'features_{name}.npz'
        if cache.exists():
            with np.load(cache) as z:
                cand, avail, feats, hop, dur = z['candidates'], z['available'], z['features'], int(z['hop']), float(z['duration'])
        else:
            mix = np.load(GOAL / f'{name}_mix.npy').astype(np.float64) if info['mix_path'] is None else read_audio(info['mix_path'], sr)[1]
            cand, avail, feats, hop = fusion_features(mix, sr)
            feats, dur = feats[:, :15], len(mix) / sr
            np.savez(cache, candidates=cand, available=avail, features=feats, hop=hop, duration=dur)
        shift = info['kick_lag_ms'] / 1000
        stem_kicks = fresh_onsets(kick_env_db(load(NEW[name]['kick'], sr), sr)) + shift
        regions = [(0.0, 1.2), (dur - 1.0, dur + 1.0)]
        for t in info['labels']:
            if not np.any(np.abs(stem_kicks - t) <= .07):
                regions.append((t - .07, t + .2))
        regions.sort()
        merged = []
        for a, b in regions:
            if merged and a <= merged[-1][1]:
                merged[-1] = (merged[-1][0], max(b, merged[-1][1]))
            else:
                merged.append((a, b))
        truth = [t for t in info['labels'] if not any(a <= t <= b for a, b in merged)]
        src = dict(track=name, group='original_five', truth=truth,
                   regions=[dict(start_s=a, end_s=b, reason='uncertain') for a, b in merged])
        rec = dict(track=name, source=src, ref=None, sample_rate=sr, hop=hop, candidates=cand, available=avail,
                   features=feats, duration=dur, removed=[], lag=shift)
        self._finish(rec, NEW[name]['kick'])
        return rec

    def _finish(self, rec, kick_path):
        sr, hop = rec['sample_rate'], rec['hop']
        dur = rec['duration'] or (rec['available'][-1] + 64) * hop / sr
        mask, y, _ = training_labels(rec['source'], rec['ref'], rec['candidates'], rec['available'], sr, hop, dur)
        rec['training_mask'], rec['labels'] = mask, y
        rec['onset_s'] = (rec['candidates'] + 1) * hop / sr
        rec['emit_s'] = (rec['available'] + 1) * hop / sr
        if kick_path is not None:
            e = kick_env_db(load(kick_path, sr), sr)
            rec['stem_kicks'] = fresh_onsets(e) + rec['lag']
            peak = np.percentile(e, 99.9)
            idx = np.clip(((rec['onset_s'] - rec['lag']) / .001).astype(int), 0, len(e) - 1)
            rec['ringing'] = e[idx] >= peak - 30
            silent = e < peak - 50
            rec['free_spans'] = self._free_spans(rec, silent) if rec['ref'] is None else []
        else:
            rec['stem_kicks'], rec['ringing'], rec['free_spans'] = None, None, []

    @staticmethod
    def _free_spans(rec, silent):
        truth = np.asarray(rec['source']['truth'])
        spans, start = [], None
        for i, s in enumerate(silent):
            t = i * .001 + rec['lag']
            if s and start is None:
                start = t
            elif not s and start is not None:
                if t - start >= 4.0 and not np.any((truth > start) & (truth < t)):
                    spans.append((start + .5, t - .2))
                start = None
        return spans


def training_set(g, tracks):
    xs, ys, ws = [], [], []
    for t in tracks:
        r = g.records[t]
        m = r['training_mask']
        y = r['labels'][m]
        w = np.where(y == 1, .5 / max(1, y.sum()), .5 / max(1, (1 - y).sum()))
        xs.append(r['features'][m]); ys.append(y); ws.append(w)
    return np.concatenate(xs), np.concatenate(ys), np.concatenate(ws)


def score(g, t, p, th):
    r = g.records[t]
    idx = fires(p, r['available'], r['sample_rate'], r['hop'], th)
    return idx, evaluate(r['source'], r['emit_s'][idx].tolist())


def counts(passages, ms='70'):
    return (sum(p['accuracy_by_tolerance_ms'][ms]['matched'] for p in passages),
            sum(p['accuracy_by_tolerance_ms'][ms]['extra'] for p in passages),
            sum(p['labels'] for p in passages))


def choose(g, preds, grid=None):
    """Threshold maximising pooled F1 at 70 ms over {track: probabilities}."""
    allp = np.concatenate(list(preds.values()))
    if grid is None:
        grid = np.unique(expit(np.quantile(logit(np.clip(allp, 1e-9, 1 - 1e-9)), np.linspace(.5, .9995, 60))))
    best = None
    for th in grid:
        m = e = n = 0
        for t, p in preds.items():
            a, b, c = counts(score(g, t, p, th)[1])
            m, e, n = m + a, e + b, n + c
        f1 = 2 * m / max(1, 2 * m + e + (n - m))
        if best is None or f1 > best[1] + 1e-12:
            best = (float(th), f1)
    if grid is not None and len(grid) > 30:
        i = int(np.searchsorted(grid, best[0]))
        lo, hi = grid[max(0, i - 1)], grid[min(len(grid) - 1, i + 1)]
        return choose(g, preds, np.linspace(lo, hi, 12)) if hi > lo else best
    return best


def tail_and_core(g, t, idx, passages):
    r = g.records[t]
    extras = {round(x, 6) for p in passages for x in p['accuracy_by_tolerance_ms']['70']['extra_times_s']}
    tail = core = 0
    for i in idx:
        em = round(float(r['emit_s'][i]), 6)
        if r['ringing'] is not None and em in extras:
            on = r['onset_s'][i]
            if r['ringing'][i] and not np.any((r['stem_kicks'] - on >= -.07) & (r['stem_kicks'] - on <= .035)):
                tail += 1
        if r['source']['group'] != 'original_five':
            core += any(p['start_s'] <= r['emit_s'][i] < p['end_s'] for p in r['source']['passages'] if p['id'] in CORE_IDS)
        else:
            core += any(a <= r['emit_s'][i] < b for a, b in r['free_spans'])
    return tail, core


def summarise(g, outcome):
    """outcome: {track: (fire idx, passages)} -> totals, per song, target check."""
    tot = {ms: [0, 0] for ms in ('35', '50', '70')}
    labels = tail = core = 0
    per, delays, free_s = {}, [], 0.0
    for t, (idx, passages) in outcome.items():
        for ms in tot:
            a, b, c = counts(passages, ms)
            tot[ms][0] += a; tot[ms][1] += b
        m, e, n = counts(passages)
        tl, cr = tail_and_core(g, t, idx, passages)
        labels += n; tail += tl; core += cr
        per[t] = dict(matched=m, extra=e, labels=n, recall=round(m / max(1, n), 3), tail=tl, core=cr)
        delays += [x['delay_ms'] for p in passages for x in p['association_early_35_late_200_ms']['pairs']]
        free_s += sum(b - a for a, b in g.records[t]['free_spans'])
    m, e = tot['70']
    dl = np.asarray(delays) if delays else np.zeros(1)
    no_bg = [v for k, v in per.items() if k != BAD_GUY]
    out = dict(labels=labels, tol={k: dict(matched=v[0], extra=v[1]) for k, v in tot.items()},
               recall=round(m / max(1, labels), 3), precision=round(m / max(1, m + e), 3),
               without_bad_guy=dict(matched=sum(v['matched'] for v in no_bg), extra=sum(v['extra'] for v in no_bg),
                                    labels=sum(v['labels'] for v in no_bg)),
               tail_fires=tail, core_fires=core, per_track=per,
               delay_ms=[round(float(np.median(dl)), 1), round(float(np.percentile(dl, 90)), 1), round(float(dl.max()), 1)],
               min_track_recall=min(v['recall'] for v in per.values() if v['labels']))
    out['meets_target'] = (out['recall'] >= .9 and out['precision'] >= .9 and out['min_track_recall'] >= .8
                           and tail == 0 and core == 0)
    return out


def nested(g, fit, predict, tracks=TRACKS, name=''):
    """Dev nested evaluation. fit(train_tracks) -> model; predict(model, track) -> probabilities."""
    outcome, cuts = {}, {}
    cache = {}

    def model(excl):
        key = frozenset(excl)
        if key not in cache:
            cache[key] = fit([u for u in tracks if u not in key])
        return cache[key]
    for o in tracks:
        inner = {u: predict(model({o, u}), u) for u in tracks if u != o}
        th, f1 = choose(g, inner)
        cuts[o] = th
        outcome[o] = score(g, o, predict(model({o}), o), th)
    s = summarise(g, outcome)
    s.update(name=name, cutoffs=cuts)
    return s, outcome, cache


def heldout(g, fit, predict, cache, dev=TRACKS, new=NEW_SONGS, name=''):
    """Fit on all dev songs; threshold from pooled dev outer predictions; score new songs once."""
    def model(excl):
        key = frozenset(excl)
        if key not in cache:
            cache[key] = fit([u for u in dev if u not in key])
        return cache[key]
    th, f1 = choose(g, {o: predict(model({o}), o) for o in dev})
    full = model(set())
    outcome = {t: score(g, t, predict(full, t), th) for t in new}
    s = summarise(g, outcome)
    s.update(name=name, threshold=th, dev_pooled_f1=round(f1, 4))
    return s, outcome
