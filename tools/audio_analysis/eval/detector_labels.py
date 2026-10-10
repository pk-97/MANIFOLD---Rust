"""Shared event labeller: one Family config turns Peter's projects, stems and WIP mixdowns into held-out labels.

Per song, in mix time (s), the labels file holds (detector_eval scores against it):
- positives: confirmed events. Project MIDI on the family's tracks or drum-rack pads (excluded voices left out, muted
  pads only on trigger tracks), shifted by the song's proven export offset; hits inside the family's audio clips
  (one-shots, freezes, consolidations); or attacks in the family's stems (sample-aligned with the mix). Layers within
  layer_s are one event. Only PROMINENT events count (Peter, 2026-10-10: the ones you hear in the mix): an attack in
  the family band (rise >= prominent_db) whose peak is within main_db of the song's main events; others are unscored.
- unscored: windows that cannot be judged (quiet or doubtful events, fast runs under dense_s apart, padded).
- loop_spans: audio clips on other parts that may hold the family's events baked in; inside them only positives and
  vouched negatives count.
- hard_neg: vouched non-events: kicks, melodic note starts and excluded or other voices more than far_s from any
  positive.
- positives_only: recall-only songs.
Projects are read only through als_extract (read-only, sha-checked).
"""
import glob
import re
from dataclasses import dataclass, field
from pathlib import Path

import numpy as np
from scipy.signal import butter, sosfilt

from tools.audio_analysis.eval.detector_eval import inside


@dataclass(frozen=True)
class Family:
    name: str
    voice: re.Pattern            # the family's tracks, pads and clip tracks
    exclude: re.Pattern          # look-alike voices that are vouched negatives (rimshots for snares)
    other_voice: re.Pattern      # other voices whose notes are vouched negatives
    other_parts: re.Pattern      # audio clips that may hold the family's events baked in (loop spans)
    not_other_parts: re.Pattern  # ... unless the clip is clearly something else
    band: tuple                  # the family's attack band (Hz) in the mix
    midi_songs: tuple = ()
    clip_songs: tuple = ()
    stem_songs: dict = field(default_factory=dict)
    stem_neg: dict = field(default_factory=dict)
    stem_doubt: dict = field(default_factory=dict)
    recall_only: tuple = ()
    wip: bool = True             # add the proven WIP songs (detector_wip)
    prominent_db: float = 9.0
    main_db: float = 6.0
    dense_s: float = .200
    dense_pad: float = .250
    layer_s: float = .030
    far_s: float = .070
    half: tuple = (-.07, .07)
    stem_band: tuple = (150, 10000)


def env_db(x, sr, lo, hi):
    """1 ms grid log energy of one band over the last 5 ms."""
    y = sosfilt(butter(4, (lo, hi), btype='band', fs=sr, output='sos'), x) ** 2
    k = int(.005 * sr)
    c = np.concatenate([[0.0], np.cumsum(y)])
    idx = np.round(np.arange(int(len(x) / sr / 1e-3)) * 1e-3 * sr).astype(int)
    return 10 * np.log10((c[np.clip(idx + 1, 0, len(y))] - c[np.clip(idx + 1 - k, 0, len(y))]) / k + 1e-12)


def on_attack(env, t, db=9.0):
    """Whether each time has an attack: the -10..+30 ms peak sits db over the median of -60..-20 ms."""
    i = np.round(np.asarray(t) / 1e-3).astype(int)
    ok = np.zeros(len(i), bool)
    for n, j in enumerate(i):
        if 60 <= j < len(env) - 30:
            ok[n] = env[j - 10:j + 30].max() - np.median(env[j - 60:j - 20]) >= db
    return ok


def prominent(env, t, fam):
    """An attack (on_attack) whose peak is within main_db of the song's main events (90th percentile of the
    attacking ones)."""
    t = np.asarray(t)
    i = np.round(t / 1e-3).astype(int)
    ok = on_attack(env, t, fam.prominent_db)
    lvl = np.array([env[j - 10:j + 30].max() if 60 <= j < len(env) - 30 else -np.inf for j in i])
    if not ok.any():
        return ok
    return ok & (lvl >= np.percentile(lvl[ok], 90) - fam.main_db)


def timing_shift(env, t, fam):
    """Offset (s) in -30..+30 ms that gives the most events with a clear attack. Project offsets can sit ~20 ms off
    (sample attack delay, device latency). Kept at 0 unless another shift beats it by 10% of the events."""
    t = np.asarray(t)
    if not len(t):
        return 0.0
    counts = {s: int(on_attack(env, t + s / 1000, fam.prominent_db).sum()) for s in range(-30, 31)}
    best = max(counts, key=lambda s: (counts[s], -abs(s)))
    return best / 1000 if counts[best] - counts[0] > .1 * len(t) else 0.0


def merge_layers(t, layer_s=.030):
    t = np.sort(np.asarray(t, float))
    return t[np.concatenate([[True], np.diff(t) > layer_s])] if len(t) else t


def project_parts(res, fam):
    """(family note times, vouched-negative note times, other-part audio spans, family audio clips by track; the
    excluded voices' clip starts under '_neg')."""
    pos, neg = [], []
    for tr in res['tracks']:
        notes = [n for c in tr['midi_clips'] for n in c['notes']]
        if not notes:
            continue
        trigger = 'trigger' in tr['name'].lower()
        if not tr.get('speaker_on', True) and not trigger:
            continue
        keys, neg_keys, other_keys = set(), set(), set()

        def walk(devs):
            for d in devs or []:
                for b in d.get('branches') or []:
                    nm = b.get('name') or ''
                    if b.get('key') is not None and (b.get('on', True) or trigger):
                        if fam.exclude.search(nm):
                            neg_keys.add(b['key'])
                        elif fam.voice.search(nm):
                            keys.add(b['key'])
                        elif fam.other_voice.search(nm):
                            other_keys.add(b['key'])
                    walk(b.get('devices'))
        walk(tr.get('devices'))
        neg += [n['s'] for n in notes if n['key'] in neg_keys]
        neg += [n['s'] for n in notes if n['key'] in other_keys]
        if keys:
            pos += [n['s'] for n in notes if n['key'] in keys]
        elif fam.voice.search(tr['name']) and not fam.exclude.search(tr['name']):
            pos += [n['s'] for n in notes]
        elif fam.exclude.search(tr['name']):
            neg += [n['s'] for n in notes]
        elif fam.other_voice.search(tr['name']) and not fam.voice.search(tr['name']):
            neg += [n['s'] for n in notes]
    spans, clips = [], {}
    for c in res['audio_clips']:
        trn, f = c.get('track') or '', str(c.get('file') or '')
        if fam.voice.search(trn) and not fam.other_parts.search(trn + f):
            clips.setdefault(trn, []).append(c)
        elif fam.exclude.search(trn + f):
            clips.setdefault('_neg', []).append(c['start_s'])
        elif (fam.other_parts.search(trn) or fam.other_parts.search(f) or fam.voice.search(trn + f)) and not fam.not_other_parts.search(trn + f):
            spans.append((c['start_s'], c['end_s']))
    return np.array(pos), np.array(neg), spans, clips


def stem_hits(x, sr, band=(150, 10000)):
    """Attacks in a solo part (stem or clip render): (loud, quiet) times (s); quiet = 12+ dB under the loud ones."""
    e = env_db(x, sr, *band)
    out, last = [], -1000
    for i in range(60, len(e) - 1):
        if i - last < 60:
            continue
        if e[i] - np.median(e[i - 60:i - 20]) >= 10 and e[i] >= e[i - 1] and e[i] >= e[i + 1] - .5:
            j = i + int(np.argmax(e[i:i + 15]))
            out.append(j)
            last = j
    h = np.array(out, int)
    lvl = e[h] if len(h) else np.zeros(0)
    loud = lvl >= (np.percentile(lvl, 90) - 12 if len(h) else 0)
    return h[loud] * 1e-3, h[~loud] * 1e-3


def clip_hits(rows, als_dir, sr, band=(150, 10000)):
    """Hits inside the family's audio clips, in arrangement seconds: one-shots, frozen tracks and consolidated parts
    alike (each is that track rendered on its own, so stem_hits applies). Files missing at their stored path are looked
    up by name under the project folder (read only). Assumes unwarped or 1:1 clips. Returns (loud, quiet)."""
    from tools.audio_analysis.eval.kick_goal_eval import load
    loud, quiet, found, cache = [], [], {}, {}
    for c in rows:
        name = c.get('file')
        if not name:
            continue
        p = c.get('file_path')
        if not (p and Path(p).exists()):
            if name not in found:
                found[name] = next((str(q) for q in Path(als_dir).rglob(glob.escape(name)) if q.is_file()), None)
            p = found[name]
        if not p:
            continue
        if p not in cache:
            cache[p] = load(p, sr)
        a = int(max(0.0, c['file_start_s']) * sr)
        seg = cache[p][a:a + int((c['end_s'] - c['start_s']) * sr)]
        if len(seg) < sr // 50:
            continue
        h_loud, h_quiet = stem_hits(np.concatenate([np.zeros(sr // 10), seg]), sr, band)
        loud += list(c['start_s'] + h_loud - .1)
        quiet += list(c['start_s'] + h_quiet - .1)
    return np.array(loud), np.array(quiet)


def dense_spans(hits, fam):
    """Fast runs (breaks, rolls): labelled events under dense_s apart, padded by dense_pad and joined when under 1 s
    apart. Unscored for now (BUG-9ngk8.2.1 (fast breakbeats and rolls))."""
    h = np.sort(np.asarray(hits, float))
    spans = []
    for a, b in zip(h[:-1], h[1:]):
        if b - a < fam.dense_s:
            lo, hi = a - fam.dense_pad, b + fam.dense_pad
            if spans and lo - spans[-1][1] < 1.0:
                spans[-1][1] = max(spans[-1][1], hi)
            else:
                spans.append([lo, hi])
    return spans


def cover(spans):
    tot, end = 0.0, -1e9
    for a, b in sorted(spans):
        a = max(a, end)
        tot += max(0.0, b - a)
        end = max(end, b)
    return tot


def song(g, t, fam):
    """One song's labels dict (see the module doc)."""
    from tools.audio_analysis.eval.als_extract import extract
    from tools.audio_analysis.eval.kick_goal_eval import STEM_CFG, load
    from tools.audio_analysis.eval.kick_goal_melodic import _source, melodic_onsets
    from tools.audio_analysis.eval.detector_songs import audio, rate, wip
    W = wip() if fam.wip else {}
    r = g.records.get(t, {})
    sr = rate(g, t)
    x = audio(g, t)
    dur = len(x) / sr
    env = env_db(x, sr, *fam.band)
    h0, h1 = fam.half
    unscored, neg_own, loops = [], np.zeros(0), []
    src = (W[t]['als'], W[t]['offset_s']) if t in W else _source(t)
    if t in fam.stem_songs:
        info = g.labels['new'].get(t) or {}
        dirs = {Path(p).parent for p in info.get('mix_parts') or []}
        if t in STEM_CFG:
            k = STEM_CFG[t]['kick']
            dirs |= {Path(p).parent for p in (k if isinstance(k, list) else [k])}

        def hits(name):
            return stem_hits(load(str(next(d / name for d in dirs if (d / name).exists())), sr), sr, fam.stem_band)
        pos, quiet = [], []
        for name in fam.stem_songs[t]:
            loud, q = hits(name)
            pos += list(loud)
            quiet += list(q)
        pos = merge_layers(pos, fam.layer_s)
        unscored += [(u + h0, u + h1) for u in quiet]
        for name in fam.stem_doubt.get(t, ()):
            loud, q = hits(name)
            unscored += [(u + h0, u + h1) for u in np.concatenate([loud, q])]
        for name in fam.stem_neg.get(t, ()):
            loud, q = hits(name)
            neg_own = np.concatenate([neg_own, loud, q])
        ok = prominent(env, pos, fam)
        dropped = list(pos[~ok])
        unscored += [(u + h0, u + h1) for u in pos[~ok]]
        pos = pos[ok]
        kind = 'stems'
    else:
        res = extract(src[0])
        off = src[1]
        notes, neg_notes, spans, clips = project_parts(res, fam)
        loops = [(a + off, b + off) for a, b in spans]
        neg_clips = list(clips.get('_neg', []))
        neg_own = np.concatenate([neg_notes, neg_clips]) + off if len(neg_notes) or neg_clips else np.zeros(0)
        c_loud, c_quiet = clip_hits([c for k, v in clips.items() if k != '_neg' for c in v], Path(src[0]).parent, sr, fam.stem_band)
        unscored += [(u + off + h0, u + off + h1) for u in c_quiet]
        pos = merge_layers(np.concatenate([notes, c_loud]) + off, fam.layer_s)
        kind = 'project'
        pos = pos[(pos > .1) & (pos < dur - .1)]
        shift = timing_shift(env, pos, fam)
        pos = pos + shift
        ok = prominent(env, pos, fam)
        dropped = list(pos[~ok])
        unscored += [(u + h0, u + h1) for u in pos[~ok]]
        pos = pos[ok]
    dense = dense_spans(np.concatenate([pos, dropped]), fam)
    pos = pos[~inside(pos, dense)]
    unscored += dense
    pos = pos[(pos > .1) & (pos < dur - .1)]
    if t in W:
        # WIP songs: kicks and melodic notes straight from the project, shifted by the proven offset.
        from tools.audio_analysis.eval.detector_wip import kick_beats
        from tools.audio_analysis.eval.kick_goal_melodic import NOT_MELODIC
        kicks = kick_beats(res) * 60 / W[t]['bpm'] + src[1]
        mel = np.array([n['s'] for tr in res['tracks'] if tr.get('speaker_on', True) and not NOT_MELODIC.search(tr['name'])
                        for c in tr['midi_clips'] for n in c['notes']]) + src[1]
    else:
        kicks = np.asarray(r.get('stem_kicks') if r.get('stem_kicks') is not None else r.get('all_labels', r.get('truth', [])), float)
        mel = melodic_onsets(t) + r.get('lag', 0.0)
    neg = np.concatenate([kicks, mel, neg_own])
    if len(pos):
        j = np.clip(np.searchsorted(pos, neg), 1, len(pos) - 1) if len(pos) > 1 else np.zeros(len(neg), int)
        near = np.minimum(np.abs(neg - pos[np.clip(j - 1, 0, len(pos) - 1)]), np.abs(neg - pos[j]))
        neg = neg[near > fam.far_s]
    return dict(kind=kind, duration_s=dur, positives=sorted(map(float, pos)), unscored=[list(map(float, u)) for u in unscored],
                loop_spans=[list(map(float, u)) for u in loops], hard_neg=sorted(map(float, np.unique(np.round(neg, 4)))),
                positives_only=t in fam.recall_only, dropped=sorted(map(float, dropped)), dense=dense,
                timing_shift_ms=round(1000 * shift, 1) if kind == 'project' else 0.0, offset_s=float(src[1]) if src else None)


def songs(fam):
    """Every song the family labels, in file order."""
    from tools.audio_analysis.eval.detector_songs import wip
    return fam.midi_songs + fam.clip_songs + tuple(fam.stem_songs) + (tuple(wip()) if fam.wip else ())


def write(fam, path, log=print):
    """Label every song of the family and write the labels file (json)."""
    import json
    from tools.audio_analysis.eval.kick_goal_eval import Goal
    g = Goal(mode='v3')
    out = {}
    for t in songs(fam):
        out[t] = s = song(g, t, fam)
        log(f"{t:14s} {s['kind']:5s} {len(s['positives']):4d} events, {len(s['unscored']):4d} unscored windows "
            f"(loops cover {cover(s['loop_spans']):5.1f} of {s['duration_s']:.0f} s), {len(s['hard_neg']):5d} vouched negatives"
            f"{' (recall only)' if s['positives_only'] else ''}")
    Path(path).write_text(json.dumps(out))
    log(f"total {sum(len(s['positives']) for s in out.values())} {fam.name} events in {len(out)} songs -> {path}")
    return out
