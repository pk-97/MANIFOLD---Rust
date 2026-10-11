"""Every-note labels for the melodic families (Peter, 2026-10-11: reliable triggering on every audible bass or synth
note first; standout notes later). Same file layout as detector_labels, so detector_eval and detector_net score it.

Per song, from the project (read only through als_extract) shifted by the song's proven export offset:
- Events: every note on the family's part (part_classes: track name, then groups innermost outward; drum-rack pads by
  their own name) and every audio-clip start on the part whose sound begins there (a placed one-shot or sample).
  Only known times count (Peter, 2026-10-11): nothing is found by analysing inside a clip. Layers and chords within
  layer_s are one event.
- Audible: the part is sounding: the family band's mean level 10-100 ms after the note sits present_db over the
  song's quiet floor (5th percentile). Otherwise (muted, automated down) unscored, never negative. The bar only
  rejects silence: audible notes in a dense low end sit 6-10 dB over the floor, silenced ones under 0 (renew). An attack test
  cannot decide this: in a full mix the bass band rises 3 dB at 81% of moments with no bass note (2026-10-11).
  Notes with probability under 1 and quiet clip hits are unscored too.
- on_kick 'negative' (bass, Peter 2026-10-11: a bass note on a kick is the kick firing): events within kick_s of a
  kick are not events; the kick stays a vouched negative.
- Runs under dense_s apart are unscored (the scorer cannot tell the hits apart).
- Unscored parts (unknown tracks, and pads for synth): their notes are unscored windows.
- Vouched negatives: kicks, and notes of every other known part more than far_s from any family note.
- Loop spans: audio clips on unknown tracks (they may hold anything) and on the family's own part: inside a bass or
  synth recording only sharp hits are found, and a spectral-flux note finder marks a bass's sidechain pumping and
  wobbles as notes (set_me_free: a start every 70 ms), so smooth notes there stay unlabelled and fires count as false
  only on vouched negatives (2026-10-11).
Each positive carries a kind for per-kind readouts: 'overlap' (starts while the previous note sounds: a held chord, or a glide on a mono synth), 'fast' (under
200 ms after the previous event), 'short' (under a 16th), 'held' (a beat or more), else 'mid'; clip starts are 'clip'.
"""
import json
from dataclasses import dataclass
from pathlib import Path

import numpy as np

from tools.audio_analysis.eval.detector_eval import inside, nearest
from tools.audio_analysis.eval.part_classes import classify


@dataclass(frozen=True)
class NoteFamily:
    name: str
    part: str                       # the part class whose notes are events
    band: tuple                     # attack band in the mix (Hz): candidates and the audibility check
    stem_band: tuple                # band for hits inside the family's audio clips
    unscored_parts: tuple = ('?',)  # parts whose notes cannot be judged for this family
    on_kick: str = 'keep'           # 'negative': events within kick_s of a kick are not events
    kick_s: float = .030
    dense_s: float = .060
    layer_s: float = .030
    far_s: float = .070
    present_db: float = 3.0
    half: tuple = (-.07, .07)
    clip_onset_s: float = .030     # a clip start is a known hit when its sound begins within this
    tol_s: float = .030             # hit window, matched at the candidate: notes come ~3 a second, so the shared
                                    # 70 ms window would count firing anywhere as right


def silent(res, unmute=()):
    """Names of tracks that make no sound: their own speaker is off, or that of any group they sit in (a muted
    reference group silences its full-length reference clips). Groups in unmute count as playing: muted only for a
    stem export (detector_labels.export_groups)."""
    muted_groups = {tr['name'] for tr in res['tracks'] if tr['kind'] == 'GroupTrack' and not tr.get('speaker_on', True)} - set(unmute)
    return {tr['name'] for tr in res['tracks'] if not tr.get('speaker_on', True) or muted_groups & set(tr.get('groups') or ())}


def project_notes(res):
    """[(seconds, beat, duration in beats, part, probability)] for every played note, parts by part_classes."""
    out = []
    quiet = silent(res)
    for tr in res['tracks']:
        notes = [n for c in tr['midi_clips'] for n in c['notes']]
        if not notes or tr['name'] in quiet:
            continue
        cls = classify(tr['name'], tr.get('groups') or ())
        pads = {}

        def walk(devs):
            for d in devs or []:
                for b in d.get('branches') or []:
                    if b.get('key') is not None and b.get('on', True):
                        c = classify(b.get('name') or '')
                        pads[b['key']] = cls if c == '?' else c
                    walk(b.get('devices'))
        walk(tr.get('devices'))
        out += [(n['s'], n['beat'], n['duration'], pads.get(n['key'], cls), n['probability']) for n in notes]
    return sorted(out)


def playing_clips(res, unmute=()):
    """Audio clips that sound: not muted, on a track that is not silent."""
    quiet = silent(res, unmute)
    return [c for c in res['audio_clips'] if not c['disabled'] and c.get('track') not in quiet]


def rise(env, t):
    """Band rise (dB) at each time: the -10..+30 ms peak over the -60..-20 ms median, env on the 1 ms grid."""
    out = np.full(len(t), -np.inf)
    for k, s in enumerate(t):
        j = int(round(s / 1e-3))
        if 60 <= j < len(env) - 30:
            out[k] = env[j - 10:j + 30].max() - np.median(env[j - 60:j - 20])
    return out


def presence(env, t):
    """Band level (dB) 10-100 ms after each time over the song's quiet floor (5th percentile)."""
    floor = np.percentile(env, 5)
    out = np.full(len(t), -np.inf)
    for k, s in enumerate(t):
        j = int(round(s / 1e-3))
        if 0 <= j < len(env) - 100:
            out[k] = env[j + 10:j + 100].mean() - floor
    return out


def kinds(beats, durs, secs):
    """Kind per merged family event (see the module doc)."""
    out = []
    for i in range(len(beats)):
        if i and beats[i - 1] + durs[i - 1] > beats[i] + .05:
            out.append('overlap')
        elif i and secs[i] - secs[i - 1] < .2:
            out.append('fast')
        elif durs[i] < .25:
            out.append('short')
        elif durs[i] >= 1:
            out.append('held')
        else:
            out.append('mid')
    return out


def song(g, t, fam):
    """One song's labels dict, or None when the project has no notes or clips on the family's part."""
    from tools.audio_analysis.eval.als_extract import extract
    from tools.audio_analysis.eval.detector_labels import clip_starts, env_db, vouched
    from tools.audio_analysis.eval.detector_songs import audio, rate, wip
    from tools.audio_analysis.eval.kick_goal_melodic import _source
    W = wip()
    src = (W[t]['als'], W[t]['offset_s']) if t in W else _source(t)
    if not src:
        return None
    res = extract(src[0])
    off = src[1]
    notes = project_notes(res)
    playing = playing_clips(res)
    clips = [c for c in playing if classify(c.get('track') or '', c.get('groups') or (), c.get('file') or '') == fam.part]
    mine = [n for n in notes if n[3] == fam.part]
    if not mine and not clips:
        return None
    sr = rate(g, t)
    x = audio(g, t)
    dur = len(x) / sr
    env = env_db(x, sr, *fam.band)
    h0, h1 = fam.half
    unscored, count = [], {}

    # Family events: the first note of each layer, with its kind.
    sure = [n for n in mine if n[4] >= 1]
    unscored += [(n[0] + off + h0, n[0] + off + h1) for n in mine if n[4] < 1]
    secs, beats, durs = [], [], []
    for s, b, d, _, _ in sure:
        if secs and s - secs[-1] <= fam.layer_s:
            durs[-1] = max(durs[-1], d)
            continue
        secs.append(s)
        beats.append(b)
        durs.append(d)
    kind = kinds(beats, durs, secs)
    pos = np.array(secs) + off
    points = [(q['beat'], q['bpm']) for q in res['tempo_points']]
    # Known times only (Peter, 2026-10-11): a clip's start counts when a loud hit begins there (a placed one-shot or
    # sample); hits found further inside a clip are guesses, left to the loop spans.
    placed = clip_starts(clips, Path(src[0]).parent, sr, fam.stem_band, points, fam.clip_onset_s)
    pos = np.concatenate([pos, placed + off])
    kind = np.array(kind + ['clip'] * len(placed))
    order = np.argsort(pos, kind='stable')
    pos, kind = pos[order], kind[order]
    keep = np.concatenate([[True], np.diff(pos) > fam.layer_s]) if len(pos) else np.zeros(0, bool)
    pos, kind = pos[keep], kind[keep]
    inside_song = (pos > .1) & (pos < dur - .1)
    pos, kind = pos[inside_song], kind[inside_song]
    count['events'] = len(pos)

    # Project offsets can sit ~20 ms off: the shift in -30..+30 ms giving the most clear attacks, if it beats 0 by 10%.
    shifts = {s: int(np.sum(rise(env, pos + s / 1000) >= 6)) for s in range(-30, 31, 2)}
    best = max(shifts, key=lambda s: (shifts[s], -abs(s)))
    shift = best / 1000 if shifts[best] - shifts[0] > .1 * max(1, len(pos)) else 0.0
    pos = pos + shift

    heard = presence(env, pos) >= fam.present_db
    count['inaudible'] = int((~heard).sum())
    count['inaudible_kinds'] = {k: int(np.sum(~heard & (kind == k))) for k in np.unique(kind[~heard])}
    unscored += [(u + h0, u + h1) for u in pos[~heard]]
    pos, kind = pos[heard], kind[heard]

    kicks, _ = vouched(g, t, W, src, res if t in W else None)
    if fam.on_kick == 'negative' and len(pos) and len(kicks):
        on = nearest(pos, kicks) <= fam.kick_s
        count['on_kick'] = int(on.sum())
        pos, kind = pos[~on], kind[~on]

    runs = []
    for a, b in zip(pos[:-1], pos[1:]):
        if b - a < fam.dense_s:
            runs.append((a + h0, b + h1))
    dense = inside(pos, runs)
    count['dense'] = int(dense.sum())
    unscored += runs
    pos, kind = pos[~dense], kind[~dense]

    every = np.array([n[0] for n in mine]) + off + shift
    unscored += [(n[0] + off + shift + h0, n[0] + off + shift + h1) for n in notes if n[3] in fam.unscored_parts]
    others = np.array([n[0] for n in notes if n[3] not in (fam.part, 'control', *fam.unscored_parts)]) + off + shift
    neg = np.concatenate([kicks, others])
    neg = neg[nearest(neg, every) > fam.far_s] if len(every) else neg
    loops = [(c['start_s'] + off, c['end_s'] + off) for c in playing
             if classify(c.get('track') or '', c.get('groups') or (), c.get('file') or '') in ('?', fam.part)]
    return dict(kind='project', duration_s=dur, positives=list(map(float, pos)), kinds=list(map(str, kind)),
                unscored=[list(map(float, u)) for u in unscored], loop_spans=[list(map(float, u)) for u in loops],
                hard_neg=sorted(map(float, np.unique(np.round(neg, 4)))), positives_only=False, counts=count, tol_s=fam.tol_s,
                timing_shift_ms=round(1000 * shift, 1), offset_s=float(off))


def write(fam, path, log=print):
    """Label every song with a project and a proven offset that has the family's part; write the labels file."""
    from tools.audio_analysis.eval.detector_songs import wip
    from tools.audio_analysis.eval.kick_goal_eval import Goal
    g = Goal(mode='v3')
    W = wip()
    out = {}
    for t in [t for t in g.records if t not in W] + list(W):
        s = song(g, t, fam)
        if s is None:
            continue
        out[t] = s
        c = s['counts']
        log(f"{t:18s} {len(s['positives']):5d} events of {c['events']:5d} notes ({c['inaudible']} inaudible, "
            f"{c.get('on_kick', 0)} on a kick, {c['dense']} in fast runs), shift {s['timing_shift_ms']:+.0f} ms, "
            f"{len(s['hard_neg'])} vouched negatives")
    Path(path).write_text(json.dumps(out))
    tot = {k: sum(s['counts'].get(k, 0) for s in out.values()) for k in ('events', 'inaudible', 'on_kick', 'dense')}
    kinds_all = {}
    for s in out.values():
        for k in s['kinds']:
            kinds_all[k] = kinds_all.get(k, 0) + 1
    log(f"total {sum(len(s['positives']) for s in out.values())} {fam.name} events in {len(out)} songs ({tot}); kinds {kinds_all} -> {path}")
    return out
