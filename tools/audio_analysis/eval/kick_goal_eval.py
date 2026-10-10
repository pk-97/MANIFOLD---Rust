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
  new songs, spans of at least 4 s with no label, drum-bus kicks included, and a
  silent kick stem).
- Target: pooled recall and precision >= 0.90 at 70 ms, every song >= 0.80
  recall, zero core fires, zero tail fires.
"""
from __future__ import annotations

import copy
import hashlib
import os
import json
import sys
import types
from concurrent.futures import ProcessPoolExecutor
from multiprocessing import get_context
from pathlib import Path

import numpy as np
import scipy
from scipy.special import expit, logit

from tools.audio_analysis.eval.kick_attack_rejection import read_audio
from tools.audio_analysis.eval.kick_fusion_bandwise import fusion_features
from tools.audio_analysis.eval.kick_goal_labels import DEV_STEMS, GOAL, MORE, NEW, drum_kicks, fresh_onsets, kick_env_db, load
from tools.audio_analysis.eval.kick_goal_rolls import kick_notes
from tools.audio_analysis.eval.kick_goal_trigger_labels import TRIGGER
from tools.audio_analysis.eval.kick_goal_wip_labels import WIP
from tools.audio_analysis.eval.kick_night_common import NIGHT, TRACKS, Data
from tools.audio_analysis.eval.live_kick_baseline import match_events, score_events
from tools.audio_analysis.eval.master_kick_comparison import score_passage
from tools.audio_analysis.eval.run_kick_dsp_experiments import evaluate
from tools.audio_analysis.eval.run_kick_fusion_trial import training_labels

# Runner outputs; inputs are always read from GOAL.
OUT = Path(os.environ.get('KICK_GOAL_OUT', GOAL))
KEYED = GOAL / 'keyed_cache'
PKG = 'tools.audio_analysis.eval'


def code_hash(*fns):
    """sha256 of the source files of every eval module reachable from fns' modules
    through module globals: an edit anywhere in that closure changes the key."""
    seen, todo = set(), [f.__module__ for f in fns]
    while todo:
        name = todo.pop()
        if name in seen:
            continue
        seen.add(name)
        for v in vars(sys.modules[name]).values():
            dep = v.__name__ if isinstance(v, types.ModuleType) else getattr(v, '__module__', None)
            if isinstance(dep, str) and dep.startswith(PKG + '.') and dep in sys.modules:
                todo.append(dep)
    h = hashlib.sha256()
    for name in sorted(seen):
        h.update(name.encode() + Path(sys.modules[name].__file__).read_bytes())
    return h.hexdigest()


def stem_derived(fn, path, sr):
    """fn(load(path, sr), sr), cached on the audio bytes, sr, the code closure and
    the numpy/scipy versions, so a hit is never stale. Writes are atomic."""
    paths = path if isinstance(path, (list, tuple)) else [path]
    h = hashlib.sha256(repr((fn.__name__, sr, np.__version__, scipy.__version__, code_hash(fn, load))).encode())
    for p in paths:
        h.update(hashlib.sha256(Path(p).read_bytes()).digest())
    f = KEYED / f'{fn.__name__}_{h.hexdigest()[:32]}.npy'
    if not f.exists():
        KEYED.mkdir(exist_ok=True)
        tmp = f.with_name(f'{f.name}.{os.getpid()}.tmp')
        with open(tmp, 'wb') as fh:
            np.save(fh, fn(load(path, sr), sr))
        os.replace(tmp, f)
    return np.load(f)


def stem_notes(track, kick_path, sr):
    """Kick notes (stem time, s) of a kick stem, rolls included; cached per track."""
    path = GOAL / f'notes_{track}.npy'
    if not path.exists():
        x = load(kick_path, sr)
        np.save(path, kick_notes(x, sr, kick_env_db(x, sr)))
    return np.load(path)


REFRACTORY = .060
TRUTH = os.environ.get('KICK_GOAL_TRUTH', 'strict')
MORE_ON = os.environ.get('KICK_GOAL_MORE') == '1'
TRIGGER_ON = os.environ.get('KICK_GOAL_TRIGGER') == '1'
WIP_ON = os.environ.get('KICK_GOAL_WIP') == '1'
SUFFIX = ('' if TRUTH == 'strict' else f'_{TRUTH}') + ('_more' if MORE_ON else '') + ('_trig' if TRIGGER_ON else '') + ('_wip' if WIP_ON else '')
NEW_SONGS = ('pattern', 'back_to_you', 'burn_stems', 'cold_remix')
# KICK_GOAL_MORE=1 adds the campaign 2 training songs (kick_goal_labels.MORE, labels_more.json).
MORE_SONGS = tuple(MORE) if MORE_ON else ()
STEM_CFG = {**NEW, **MORE}
# KICK_GOAL_TRIGGER=1 adds the songs labelled from a kick trigger track (kick_goal_trigger_labels, labels_trigger.json).
TRIGGER_SONGS = tuple(TRIGGER) if TRIGGER_ON else ()
# KICK_GOAL_WIP=1 adds the section-scored WIP mixdowns (kick_goal_wip_labels, labels_wip.json).
WIP_SONGS = WIP if WIP_ON else ()
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
        """mode 'v3' (primary since Peter's 2026-10-10 ruling, every new kick note
        triggers): on songs with a kick stem, truth is the stem's kick notes,
        rolls over a ringing tail included (kick_goal_rolls), plus reviewed
        drum-bus kicks; songs without a stem keep their reviewed labels.
        mode 'strict': rolls over a ringing tail are non-kicks (superseded).
        mode 'loose': the reviewed rolls restored (superseded by v3)."""
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
            if src['group'] != 'original_five' and mode == 'v3' and t in DEV_STEMS:
                removed = set()
                notes = stem_notes(t, DEV_STEMS[t]['kick'], r['sample_rate']) + lags[t]['song_lag_ms'] / 1000
                bus = [x['t'] for x in row['kept'] if x['cls'] == 'drums_bus_kick']
                for p in src['passages']:
                    inside = [float(x) for x in notes if p['start_s'] <= x < p['end_s']]
                    extra = [x for x in bus if p['start_s'] <= x < p['end_s'] and not any(abs(x - y) <= .07 for y in inside)]
                    p['kick_times_s'] = sorted(inside + extra)
            elif src['group'] != 'original_five':
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
        if MORE_SONGS:
            self.labels['new'].update(json.loads((GOAL / 'labels_more.json').read_text())['new'])
        if TRIGGER_SONGS:
            self.labels['new'].update(json.loads((GOAL / 'labels_trigger.json').read_text())['new'])
        if WIP_SONGS:
            self.labels['new'].update(json.loads((GOAL / 'labels_wip.json').read_text())['new'])
        if with_new:
            for name in NEW_SONGS + MORE_SONGS + TRIGGER_SONGS + WIP_SONGS:
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
        trigger = info.get('kind') == 'trigger'
        sections = info.get('kind') == 'sections'
        if sections:
            stem_kicks, labels = np.asarray(info['labels']), list(info['labels'])
        elif trigger:
            assert not info['rejected'], f'{name}: rejected by kick_goal_trigger_labels'
            stem_kicks, labels = np.asarray(info['labels']), list(info['labels'])
        elif self.mode == 'v3':
            stem_kicks = stem_notes(name, STEM_CFG[name]['kick'], sr) + shift
            labels = sorted([float(x) for x in stem_kicks if 1.0 <= x <= dur - 1.0] +
                            [x for x in info['labels'] if not np.any(np.abs(stem_kicks - x) <= .07)])
        else:
            stem_kicks = fresh_onsets(stem_derived(kick_env_db, STEM_CFG[name]['kick'], sr)) + shift
            labels = info['labels']
        regions = [(0.0, 1.2), (dur - 1.0, dur + 1.0)] + [(u - .07, u + .2) for u in info.get('uncertain', [])]
        if sections:
            # Only the proven sections are scored: everything between them is unscored.
            edges = [0.0] + [x for a, b in sorted(info['scored_spans_s']) for x in (a, b)] + [dur + 1.0]
            regions += [(a, b) for a, b in zip(edges[0::2], edges[1::2]) if b > a]
        for t in labels:
            if not np.any(np.abs(stem_kicks - t) <= .07):
                regions.append((t - .07, t + .2))
        regions.sort()
        merged = []
        for a, b in regions:
            if merged and a <= merged[-1][1]:
                merged[-1] = (merged[-1][0], max(b, merged[-1][1]))
            else:
                merged.append((a, b))
        truth = [t for t in labels if not any(a <= t <= b for a, b in merged)]
        src = dict(track=name, group='original_five', truth=truth,
                   regions=[dict(start_s=a, end_s=b, reason='uncertain') for a, b in merged])
        rec = dict(track=name, source=src, ref=None, sample_rate=sr, hop=hop, candidates=cand, available=avail, all_labels=labels,
                   features=feats, duration=dur, removed=[], lag=shift)
        if sections:
            self._finish(rec, None)
            rec['stem_kicks'] = stem_kicks
        elif trigger:
            self._finish(rec, None, TRIGGER[name]['drums'])
        else:
            self._finish(rec, STEM_CFG[name]['kick'])
        return rec

    def _finish(self, rec, kick_path, drums_path=None):
        sr, hop = rec['sample_rate'], rec['hop']
        dur = rec['duration'] or (rec['available'][-1] + 64) * hop / sr
        mask, y, _ = training_labels(rec['source'], rec['ref'], rec['candidates'], rec['available'], sr, hop, dur)
        rec['training_mask'], rec['labels'] = mask, y
        rec['onset_s'] = (rec['candidates'] + 1) * hop / sr
        rec['emit_s'] = (rec['available'] + 1) * hop / sr
        if kick_path is not None:
            e = stem_derived(kick_env_db, kick_path, sr)
            rec['stem_kicks'] = (stem_notes(rec['track'], kick_path, sr) if self.mode == 'v3' else fresh_onsets(e)) + rec['lag']
            peak = np.percentile(e, 99.9)
            idx = np.clip(((rec['onset_s'] - rec['lag']) / .001).astype(int), 0, len(e) - 1)
            rec['ringing'] = e[idx] >= peak - 30
            silent = e < peak - 50
            rec['free_spans'] = self._free_spans(rec, silent) if rec['ref'] is None else []
        else:
            rec['stem_kicks'], rec['ringing'], rec['free_spans'] = None, None, []
            if drums_path is not None:
                # Trigger songs: the verified trigger kicks stand in for stem notes and kick-free
                # spans need a silent drums premaster. No kick stem, so no tail-fire check.
                e = stem_derived(kick_env_db, drums_path, sr)
                rec['stem_kicks'] = np.asarray(rec['all_labels'])
                rec['free_spans'] = self._free_spans(rec, e < np.percentile(e, 99.9) - 50)

    @staticmethod
    def _free_spans(rec, silent):
        truth = np.asarray(rec.get('all_labels', rec['source']['truth']))
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


def training_set(g, tracks, whole=False, feats='features'):
    """Song-equal, class-balanced rows. whole=True uses whole-song kick-stem truth
    where the song has a kick stem (add_whole_song_truth), else the scored truth."""
    xs, ys, ws = [], [], []
    for t in tracks:
        r = g.records[t]
        m, y_all = (r['train_mask'], r['train_y']) if whole and 'train_mask' in r else (r['training_mask'], r['labels'])
        y = y_all[m]
        w = np.where(y == 1, .5 / max(1, y.sum()), .5 / max(1, (1 - y).sum()))
        xs.append(r[feats][m]); ys.append(y); ws.append(w)
    return np.concatenate(xs), np.concatenate(ys), np.concatenate(ws)


def add_whole_song_truth(g):
    """Training-only truth over whole dev own-stem songs, by the new-song rule:
    fresh kick-stem attacks (with the level floor), drum-bus kicks without one
    are uncertain, first and last 1.2 s excluded. Scoring still uses v2."""
    for t in TRACKS:
        if t not in DEV_STEMS:
            continue
        r = g.records[t]
        sr, hop = r['sample_rate'], r['hop']
        dur = len(read_audio(r['source']['audio_path'])[1]) / sr
        bus = stem_derived(drum_kicks, DEV_STEMS[t]['drums'][0], sr) + r['lag']
        kicks = r['stem_kicks']
        regions = [(0.0, 1.2), (dur - 1.2, dur + 1.0)] + [(u - .07, u + .2) for u in bus if not np.any(np.abs(kicks - u) <= .07)]
        regions.sort()
        merged = []
        for a, b in regions:
            if merged and a <= merged[-1][1]:
                merged[-1] = (merged[-1][0], max(b, merged[-1][1]))
            else:
                merged.append((a, b))
        truth = [float(k) for k in kicks if 1.2 < k < dur - 1.2 and not any(a <= k <= b for a, b in merged)]
        src = dict(track=t, group='original_five', truth=truth, regions=[dict(start_s=a, end_s=b) for a, b in merged])
        m, y, _ = training_labels(src, None, r['candidates'], r['available'], sr, hop, dur)
        r['train_mask'], r['train_y'] = m, y
    for t, r in g.records.items():
        if 'train_mask' not in r:
            r['train_mask'], r['train_y'] = r['training_mask'], r['labels']


def score(g, t, p, th):
    r = g.records[t]
    idx = fires(p, r['available'], r['sample_rate'], r['hop'], th)
    return idx, evaluate(r['source'], r['emit_s'][idx].tolist())


def counts(passages, ms='70'):
    return (sum(p['accuracy_by_tolerance_ms'][ms]['matched'] for p in passages),
            sum(p['accuracy_by_tolerance_ms'][ms]['extra'] for p in passages),
            sum(p['labels'] for p in passages))


def _outside(x, spans):
    keep = np.ones(len(x), bool)
    for a, b in spans:
        keep &= ~((a <= x) & (x <= b))
    return keep


def count_prep(g, t):
    """Threshold-free scoring state of song t for fast_counts, built once per Goal
    (records are read-only after construction)."""
    prep = g.__dict__.setdefault('count_prep', {})
    if t not in prep:
        r = g.records[t]
        av, em, src, sr, hop = r['available'], r['emit_s'], r['source'], r['sample_rate'], r['hop']
        # fires() then reduces to a jump: after a fire at i, the next is the first pass at or after nxt[i].
        assert av.dtype.kind in 'iu' and np.all(np.diff(av) >= 0), f'{t}: availability must be sorted integers'
        d = np.arange(1, int(np.ceil(REFRACTORY * sr / hop)) + 3)
        ok = d * hop / sr >= REFRACTORY - 1e-12
        assert ok.any()
        nxt = np.searchsorted(av, av + d[np.argmax(ok)], 'left')
        if src['group'] == 'original_five':
            score_events([], src['truth'], src['regions'])
            parts = [(_outside(em, [(x['start_s'], x['end_s']) for x in src['regions']]), sorted(src['truth']), None)]
        else:
            parts = []
            for p in src['passages']:
                score_passage([], p)
                ex = [(x['start_s'] - .07, x['end_s'] + .2) for x in p.get('uncertain_regions', [])]
                truth = [x for x in p['kick_times_s'] if not any(a <= x <= b for a, b in ex)]
                keep = _outside(em, ex) & (p['review_start_s'] <= em) & (em <= p['review_end_s'])
                core = (p['start_s'], p['end_s'], {i for i, x in enumerate(truth) if p['start_s'] <= x < p['end_s']})
                parts.append((keep, truth, core))
        prep[t] = nxt, parts
    return prep[t]


def greedy_matches(pred, truth, lo, hi):
    """Size of the maximum one-to-one matching with lo <= pred - truth <= hi on sorted
    lists. The pairable truths of each prediction form a window that moves forward with
    it, so matching the earliest pairable pair first is optimal: it equals the count of
    live_kick_baseline.match_events, which also maximises count first."""
    i = j = m = 0
    while i < len(pred) and j < len(truth):
        e = pred[i] - truth[j]
        if e < lo:
            i += 1
        elif e > hi:
            j += 1
        else:
            m, i, j = m + 1, i + 1, j + 1
    return m


def fast_fires(g, t, p, th):
    """fires() on song t, stepping from fire to fire instead of over every pass."""
    nxt = count_prep(g, t)[0]
    ps = np.flatnonzero(p >= th)
    idx, k = [], 0
    while k < len(ps):
        idx.append(ps[k])
        k = ps.searchsorted(nxt[ps[k]])
    return np.asarray(idx, dtype=int)


def fast_counts(g, t, p, th, ms='70'):
    """counts(score(g, t, p, th)[1], ms), exactly, without the full report: the same
    fires, the greedy count on whole songs and the exact matcher on passages (their
    core counts depend on which pairs it picks)."""
    parts = count_prep(g, t)[1]
    idx = fast_fires(g, t, p, th)
    em = g.records[t]['emit_s'][idx]
    tol = int(ms) / 1000
    m = e = n = 0
    for keep, truth, core in parts:
        pred = em[keep[idx]].tolist()
        if core is None:
            a = greedy_matches(pred, truth, -tol - 1e-9, tol + 1e-9)
            m, e, n = m + a, e + len(pred) - a, n + len(truth)
            continue
        start, end, scored = core
        pairs = match_events(pred, truth, tol, tol)
        used = {i for _, i in pairs}
        m += sum(j in scored for j, _ in pairs)
        e += sum(start <= x < end and i not in used for i, x in enumerate(pred))
        n += len(scored)
    return m, e, n


def jobs():
    """Worker processes for the nested runners: KICK_GOAL_JOBS, default 1 (serial)."""
    return int(os.environ.get('KICK_GOAL_JOBS', '1'))


def run_tasks(work, tasks, init, initargs=()):
    """Iterator of work(task) in task order. The caller has already run init(*initargs)
    here; with KICK_GOAL_JOBS = n > 1, n single-threaded spawned workers each run it
    and share the tasks. Results do not depend on n (fixed seeds, ordered merge)."""
    n = jobs()
    if n <= 1:
        yield from map(work, tasks)
        return
    os.environ.update(OMP_NUM_THREADS='1', OPENBLAS_NUM_THREADS='1', VECLIB_MAXIMUM_THREADS='1', MKL_NUM_THREADS='1')
    with ProcessPoolExecutor(n, mp_context=get_context('spawn'), initializer=init, initargs=initargs) as ex:
        yield from ex.map(work, tasks)


def choose(g, preds, grid=None):
    """Threshold maximising pooled F1 at 70 ms over {track: probabilities}."""
    allp = np.concatenate(list(preds.values()))
    if grid is None:
        grid = np.unique(expit(np.quantile(logit(np.clip(allp, 1e-9, 1 - 1e-9)), np.linspace(.5, .9995, 60))))
    best = None
    for th in grid:
        m = e = n = 0
        for t, p in preds.items():
            a, b, c = fast_counts(g, t, p, th)
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
