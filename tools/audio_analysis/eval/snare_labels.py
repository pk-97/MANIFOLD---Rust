#!/usr/bin/env python3
"""Snare + clap labels for the snare detector (one class: the backbeat snare/clap; rolls count; rimshots and
cross-sticks do not — Peter, 2026-10-10). Writes GOAL/snare_labels.json.

Per song, in mix time (s):
- positives: confirmed snare/clap hits. Project MIDI (snare/clap-named tracks or drum-rack pads, rim excluded, muted
  pads only on trigger tracks) shifted by the song's proven export offset (kick_goal_melodic._source); one-shot snare
  clips (clip starts); or attacks in snare/clap stems (sample-aligned with the mix). Layers within 30 ms are one hit.
  Peter wants the PROMINENT backbeat: a hit counts only when the mix shows a 1-8 kHz attack at it (rise >= PROMINENT_DB
  within -10..+30 ms) and its peak is within MAIN_DB of the song's main snares (prominent); this applies to notes,
  clips and stem hits alike; others,
  and stem hits 12+ dB under the loud ones, are unscored.
- unscored: windows that cannot be judged — unconfirmed notes, quiet stem hits.
- loop_spans: audio clips on other drum tracks (loops, breaks, tops, fills) whose snares are baked in. Inside them the
  positives still count, but no other candidate is judged a non-snare except the vouched ones (hard_neg).
- hard_neg: vouched non-snares — kicks (the kick labels) and project melodic note starts more than 70 ms from any
  snare, plus rimshot and cross-stick hits and other drum voices (hat, ride, percussion: OTHER_VOICE track or pad notes).
- positives_only: recall-only songs (Lowkey: its WIP offset is the survey's, not proven against a stem).
Projects are read only through als_extract (read-only, sha-checked).
"""
import glob
import json
import re
import sys
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parents[3]))
import numpy as np  # noqa: E402
from scipy.signal import butter, sosfilt  # noqa: E402

SNARE = re.compile(r'snare|snr|clap|clp', re.I)
NOT_SNARE = re.compile(r'rim|xstick|x-stick|cross', re.I)
# Other drum voices: their notes are vouched non-snares (Peter, 2026-10-10: hats, rides and percussion are not snares).
OTHER_VOICE = re.compile(r'hat|\bhh|ride|cymbal|crash|perc|shaker|shkr|tamb|tom|conga|bongo|cowbell|clave|wood|block|triangle|guiro|cabasa', re.I)
OTHER_DRUMS = re.compile(r'drum|break|_top|top_?loop|perc|groove|fill|hat|ride|cymbal|shaker|tamb', re.I)
MIDI_SONGS = ('flight', '48_hours', 'pattern', 'back_to_you', 'lowkey', 'corrosion', 'burn_stems', 'midnight_patience')
# Project songs whose snares live (also) in audio clips on snare/clap tracks: one-shots, freezes, consolidations.
CLIP_SONGS = ('murmur', 'default_haze', 'late_night')
STEM_SONGS = {'cold_remix': ('_ SNARE.wav',), 'gerrit': ('Drums snare 1.wav', 'Drums snare 2 (popcorn).wav'), 'business': ('@Claps.wav',),
              'worship': ('Worship Stems - MM_clap_smacc_dry-24b.wav', 'Worship Stems - MM_crisp_ass_trap_snare-24b.wav',
                          'Worship Stems - MM_jungleclap-24b.wav')}
RECALL_ONLY = ('lowkey',)
PROMINENT_DB = 9.0
MAIN_DB = 6.0
DENSE_S, DENSE_PAD = .200, .250
# Stem songs: stems whose hits are vouched non-snares (finger snaps: Peter, 2026-10-10), and stems that may hold extra
# snares (drum buses, fills, percussion) whose hits are left unscored.
STEM_NEG = {'worship': ('Worship Stems - KSHMR Snap 01-24b.wav', 'Worship Stems - KSHMR Snap 05-24b.wav', 'Worship Stems - MM_2099_snap_01-24b.wav')}
STEM_DOUBT = {'worship': ('Worship Stems - Bus 1-24b.wav', 'Worship Stems - Bus 2-24b.wav', 'Worship Stems - KSHMR Acoustic Fill 128BPM 03-24b.wav',
                          'Worship Stems - MM_percussion_billow-24b.wav')}
LAYER_S, FAR_S, HALF = .030, .070, (-.07, .07)


def env_db(x, sr, lo, hi):
    y = sosfilt(butter(4, (lo, hi), btype='band', fs=sr, output='sos'), x) ** 2
    k = int(.005 * sr)
    c = np.concatenate([[0.0], np.cumsum(y)])
    idx = np.round(np.arange(int(len(x) / sr / 1e-3)) * 1e-3 * sr).astype(int)
    return 10 * np.log10((c[np.clip(idx + 1, 0, len(y))] - c[np.clip(idx + 1 - k, 0, len(y))]) / k + 1e-12)


def on_attack(env, t):
    i = np.round(np.asarray(t) / 1e-3).astype(int)
    ok = np.zeros(len(i), bool)
    for n, j in enumerate(i):
        if 60 <= j < len(env) - 30:
            ok[n] = env[j - 10:j + 30].max() - np.median(env[j - 60:j - 20]) >= PROMINENT_DB
    return ok


def prominent(env, t):
    """Peter's snare (2026-10-10): as loud in the mix as the song's main snares, like a drum and bass snare. A hit
    needs the attack (on_attack) and a 1-8 kHz peak within MAIN_DB of the song's main snare level (the 90th percentile
    of its attacking hits); quieter ones (ghost notes, soft break hits) are unscored."""
    t = np.asarray(t)
    i = np.round(t / 1e-3).astype(int)
    ok = on_attack(env, t)
    lvl = np.array([env[j - 10:j + 30].max() if 60 <= j < len(env) - 30 else -np.inf for j in i])
    if not ok.any():
        return ok
    return ok & (lvl >= np.percentile(lvl[ok], 90) - MAIN_DB)


def timing_shift(env, t):
    """Offset (s) in -30..+30 ms that gives the most hits with a clear 1-8 kHz attack (on_attack). Project offsets can sit
    ~20 ms off (sample attack delay, device latency). Kept at 0 unless another shift beats it by 10% of the hits."""
    t = np.asarray(t)
    if not len(t):
        return 0.0
    counts = {s: int(on_attack(env, t + s / 1000).sum()) for s in range(-30, 31)}
    best = max(counts, key=lambda s: (counts[s], -abs(s)))
    return best / 1000 if counts[best] - counts[0] > .1 * len(t) else 0.0


def merge_layers(t):
    t = np.sort(np.asarray(t, float))
    return t[np.concatenate([[True], np.diff(t) > LAYER_S])] if len(t) else t


def project_parts(res):
    """(snare/clap note times, rim/cross-stick note times, other-drum audio spans, one-shot clips by track)."""
    snare, rim = [], []
    for tr in res['tracks']:
        notes = [n for c in tr['midi_clips'] for n in c['notes']]
        if not notes:
            continue
        trigger = 'trigger' in tr['name'].lower()
        if not tr.get('speaker_on', True) and not trigger:
            continue
        keys, rim_keys, other_keys = set(), set(), set()

        def walk(devs):
            for d in devs or []:
                for b in d.get('branches') or []:
                    nm = b.get('name') or ''
                    if b.get('key') is not None and (b.get('on', True) or trigger):
                        if NOT_SNARE.search(nm):
                            rim_keys.add(b['key'])
                        elif SNARE.search(nm):
                            keys.add(b['key'])
                        elif OTHER_VOICE.search(nm):
                            other_keys.add(b['key'])
                    walk(b.get('devices'))
        walk(tr.get('devices'))
        rim += [n['s'] for n in notes if n['key'] in rim_keys]
        rim += [n['s'] for n in notes if n['key'] in other_keys]
        if keys:
            snare += [n['s'] for n in notes if n['key'] in keys]
        elif SNARE.search(tr['name']) and not NOT_SNARE.search(tr['name']):
            snare += [n['s'] for n in notes]
        elif NOT_SNARE.search(tr['name']):
            rim += [n['s'] for n in notes]
        elif OTHER_VOICE.search(tr['name']) and not SNARE.search(tr['name']):
            rim += [n['s'] for n in notes]
    spans, clips = [], {}
    for c in res['audio_clips']:
        trn, f = c.get('track') or '', str(c.get('file') or '')
        if SNARE.search(trn) and not OTHER_DRUMS.search(trn + f):
            clips.setdefault(trn, []).append(c)
        elif NOT_SNARE.search(trn + f):
            clips.setdefault('_rim', []).append(c['start_s'])
        elif (OTHER_DRUMS.search(trn) or OTHER_DRUMS.search(f) or SNARE.search(trn + f)) and not re.search(r'kick|vocal|vox|synth|bass|string|pad|chord', trn + f, re.I):
            spans.append((c['start_s'], c['end_s']))
    return np.array(snare), np.array(rim), spans, clips


def clip_hits(rows, als_dir, sr):
    """Hits inside snare/clap-track audio clips, in arrangement seconds: one-shots, frozen tracks and consolidated parts
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
        h_loud, h_quiet = stem_hits(np.concatenate([np.zeros(sr // 10), seg]), sr)
        loud += list(c['start_s'] + h_loud - .1)
        quiet += list(c['start_s'] + h_quiet - .1)
    return np.array(loud), np.array(quiet)


def stem_hits(x, sr):
    e = env_db(x, sr, 150, 10000)
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


def song(g, t):
    from tools.audio_analysis.eval.als_extract import extract
    from tools.audio_analysis.eval.kick_goal_eval import STEM_CFG, load
    from tools.audio_analysis.eval.kick_goal_melodic import _source, melodic_onsets
    from tools.audio_analysis.eval.detector_songs import audio, rate, wip
    W = wip()
    r = g.records.get(t, {})
    sr = rate(g, t)
    x = audio(g, t)
    dur = len(x) / sr
    env = env_db(x, sr, 1000, 8000)
    unscored, rim, loops = [], np.zeros(0), []
    src = (W[t]['als'], W[t]['offset_s']) if t in W else _source(t)
    if t in STEM_SONGS:
        info = g.labels['new'].get(t) or {}
        dirs = {Path(p).parent for p in info.get('mix_parts') or []}
        if t in STEM_CFG:
            k = STEM_CFG[t]['kick']
            dirs |= {Path(p).parent for p in (k if isinstance(k, list) else [k])}
        pos, quiet = [], []
        for name in STEM_SONGS[t]:
            path = next(d / name for d in dirs if (d / name).exists())
            loud, q = stem_hits(load(str(path), sr), sr)
            pos += list(loud)
            quiet += list(q)
        pos = merge_layers(pos)
        unscored += [(u + HALF[0], u + HALF[1]) for u in quiet]
        for name in STEM_DOUBT.get(t, ()):
            loud, q = stem_hits(load(str(next(d / name for d in dirs if (d / name).exists())), sr), sr)
            unscored += [(u + HALF[0], u + HALF[1]) for u in np.concatenate([loud, q])]
        for name in STEM_NEG.get(t, ()):
            loud, q = stem_hits(load(str(next(d / name for d in dirs if (d / name).exists())), sr), sr)
            rim = np.concatenate([rim, loud, q])
        ok = prominent(env, pos)
        dropped = list(pos[~ok])
        unscored += [(u + HALF[0], u + HALF[1]) for u in pos[~ok]]
        pos = pos[ok]
        kind = 'stems'
    else:
        res = extract(src[0])
        off = src[1]
        notes, rim_notes, spans, clips = project_parts(res)
        loops = [(a + off, b + off) for a, b in spans]
        rim_clips = list(clips.get('_rim', []))
        rim = np.concatenate([rim_notes, rim_clips]) + off if len(rim_notes) or rim_clips else np.zeros(0)
        c_loud, c_quiet = clip_hits([c for k, v in clips.items() if k != '_rim' for c in v], Path(src[0]).parent, sr)
        unscored += [(u + off + HALF[0], u + off + HALF[1]) for u in c_quiet]
        pos = merge_layers(np.concatenate([notes, c_loud]) + off)
        kind = 'project'
        pos = pos[(pos > .1) & (pos < dur - .1)]
        shift = timing_shift(env, pos)
        pos = pos + shift
        ok = prominent(env, pos)
        dropped = list(pos[~ok])
        unscored += [(u + HALF[0], u + HALF[1]) for u in pos[~ok]]
        pos = pos[ok]
    dense = dense_spans(np.concatenate([pos, dropped]))
    pos = pos[~inside_spans(pos, dense)]
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
    neg = np.concatenate([kicks, mel, rim])
    if len(pos):
        j = np.clip(np.searchsorted(pos, neg), 1, len(pos) - 1) if len(pos) > 1 else np.zeros(len(neg), int)
        near = np.minimum(np.abs(neg - pos[np.clip(j - 1, 0, len(pos) - 1)]), np.abs(neg - pos[j]))
        neg = neg[near > FAR_S]
    return dict(kind=kind, duration_s=dur, positives=sorted(map(float, pos)), unscored=[list(map(float, u)) for u in unscored], loop_spans=[list(map(float, u)) for u in loops],
                hard_neg=sorted(map(float, np.unique(np.round(neg, 4)))), positives_only=t in RECALL_ONLY, dropped=sorted(map(float, dropped)), dense=dense, timing_shift_ms=round(1000 * shift, 1) if kind == 'project' else 0.0,
                offset_s=float(src[1]) if src else None)


def inside_spans(t, spans):
    t = np.asarray(t)
    out = np.zeros(len(t), bool)
    for a, b in spans:
        out |= (t >= a) & (t <= b)
    return out


def dense_spans(hits):
    """Fast breaks and rolls (Peter, 2026-10-10: unscored for now, BUG for later): runs of labelled hits under DENSE_S
    apart, padded by DENSE_PAD and joined when under 1 s apart."""
    h = np.sort(np.asarray(hits, float))
    spans = []
    for a, b in zip(h[:-1], h[1:]):
        if b - a < DENSE_S:
            lo, hi = a - DENSE_PAD, b + DENSE_PAD
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


def main():
    from tools.audio_analysis.eval.kick_goal_eval import GOAL, Goal
    g = Goal(mode='v3')
    out = {}
    from tools.audio_analysis.eval.detector_songs import wip
    for t in MIDI_SONGS + CLIP_SONGS + tuple(STEM_SONGS) + tuple(wip()):
        out[t] = song(g, t)
        s = out[t]
        print(f"{t:14s} {s['kind']:5s} {len(s['positives']):4d} snares, {len(s['unscored']):4d} unscored windows "
              f"(loops cover {cover(s['loop_spans']):5.1f} of {s['duration_s']:.0f} s), {len(s['hard_neg']):5d} vouched non-snares"
              f"{' (recall only)' if s['positives_only'] else ''}", flush=True)
    path = GOAL / 'snare_labels.json'
    path.write_text(json.dumps(out))
    print(f"total {sum(len(s['positives']) for s in out.values())} snares in {len(out)} songs -> {path}")


if __name__ == '__main__':
    main()
