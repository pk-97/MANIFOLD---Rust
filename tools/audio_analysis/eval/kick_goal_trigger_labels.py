#!/usr/bin/env python3
"""Kick truth from a project's kick trigger track, checked against its drums premaster.

Usage: kick_goal_trigger_labels.py [SONG ...]   (writes GOAL/labels_trigger.json and GOAL/{song}_mix.npy)

Peter, 2026-10-10: his kick trigger MIDI track ("DS Kick") marks the kicks. The
.als is read through als_extract, which proves the file unchanged.
- Strong low hit in the drums premaster: the 40-150 Hz level rises 12 dB within
  10 ms, within 30 dB of the file's loudest, one per 80 ms; time is the rise
  peak less 5 ms.
- Export offset: premasters start at some arrangement beat. The offset is the
  mode of every hit-minus-note difference (10 ms bins), refined by the median
  of the differences within 30 ms of it.
- A trigger note with a hit within HIT_MS is a kick. One without is dropped and
  its spot is not scored.
- A kick-shaped drum hit (kick_goal_labels.drum_kicks: at least 25% of its
  first 40 ms below 140 Hz) with no trigger note within KICK_NEAR_MS is not
  scored: a break or loop kick the trigger does not mark. A snare trigger note
  on it overrides: snares stay non-kicks. Every other drum hit is a non-kick.
  (Masking every strong low hit instead hid 42-70% of the break-heavy songs;
  most of those hits are snares and toms with low body.)
- Mix: the drums premaster plus the no-drums premaster.
- A song whose unscored spots would cover more than MASK_MAX of it is rejected:
  its drums carry too many kicks the trigger does not mark.
"""
from __future__ import annotations

import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[3]))

import numpy as np  # noqa: E402
from scipy.signal import butter, sosfilt  # noqa: E402

from tools.audio_analysis.eval.als_extract import extract  # noqa: E402
from tools.audio_analysis.eval.kick_goal_labels import ABL, GOAL, drum_kicks, load, sha  # noqa: E402

SR = 48000
HIT_MS = 20
KICK_NEAR_MS = 70
RISE_DB = 12
RANGE_DB = 30
GAP_MS = 80
MASK = (-.07, .2)
MASK_MAX = .25


def song(als, kick, premasters, name, snare=()):
    return dict(als=ABL / als, kick=kick, snare=snare, drums=ABL / premasters / f'{name} - PREMASTER - DRUMS.wav',
                rest=ABL / premasters / f'{name} - PREMASTER - NO DRUMS.wav')


TRIGGER = {
    'corrosion': song('2026/Katsu Don Project/Katsu Don - V4.als', '44-DS Kick', '2026/Katsu Don Project/FINALS/PREMASTERS T2',
                      'Corrosion', snare=('37 SNARE TRIGGER',)),
    'hung_up': song('2025/Rainfall Project/Rainfall - Master V2.als', '71-DS Kick', '2025/Rainfall Project/FINALS/PREMASTERS T2',
                    '03 - Hung Up'),
    'right_where': song('2025/Steady Project/Steady - Up Tempo MASTER V5.als', '65-DS Kick',
                        '2025/Steady Project/FINALS/PREMASTERS T2', '01 Right Where I Need You'),
    'murmur': song('2025/snackpack Project/snackpack (Murmur) - EE & PK - MASTER V3.als', '55-DS Kick',
                   '2025/snackpack Project/FINALS/PREMASTERS T2', '04 - Murmur'),
}


def strong_low_hits(x, sr):
    lo = sosfilt(butter(4, [40, 150], btype='band', fs=sr, output='sos'), x)
    hop = sr // 1000
    n = len(lo) // hop
    e = 10 * np.log10(np.mean(lo[:n * hop].reshape(n, hop) ** 2, axis=1) + 1e-12)
    rise = np.zeros(n)
    rise[10:] = e[10:] - e[:-10]
    pk = np.flatnonzero((rise[1:-1] >= rise[:-2]) & (rise[1:-1] > rise[2:]) & (rise[1:-1] >= RISE_DB)
                        & (e[1:-1] >= e.max() - RANGE_DB)) + 1
    hits, last = [], -10 ** 9
    for i in pk:
        if i - last > GAP_MS:
            hits.append(i)
            last = i
    return (np.array(hits) - 5) / 1000.0


def note_times(res, name):
    track = next(t for t in res['tracks'] if t['name'] == name)
    return np.array(sorted({round(n['s'], 4) for c in track['midi_clips'] for n in c['notes']}))


def nearest(a, b):
    """Distance from each of a to the nearest of b."""
    if not len(b):
        return np.full(len(a), np.inf)
    j = np.clip(np.searchsorted(b, a), 1, len(b) - 1)
    return np.minimum(np.abs(a - b[j - 1]), np.abs(a - b[j]))


def export_offset(hits, notes):
    vals, cnt = np.unique(np.round((hits[:, None] - notes[None, :]).ravel(), 2), return_counts=True)
    off = vals[np.argmax(cnt)]
    d = hits - off - notes[np.argmin(np.abs(notes[None, :] + off - hits[:, None]), axis=1)]
    return off + float(np.median(d[np.abs(d) < .03]))


def labels(name, cfg):
    res = extract(cfg['als'])
    drums, rest = load(cfg['drums'], SR), load(cfg['rest'], SR)
    if abs(len(drums) - len(rest)) > SR:
        sys.exit(f'{name}: drums and no-drums premasters differ in length')
    n = min(len(drums), len(rest))
    mix, dur = drums[:n] + rest[:n], n / SR
    hits = strong_low_hits(drums[:n], SR)
    notes = note_times(res, cfg['kick'])
    off = export_offset(hits, notes)
    notes = notes + off
    snares = np.sort(np.concatenate([note_times(res, s) for s in cfg['snare']]) + off) if cfg['snare'] else np.array([])
    hit_ms = HIT_MS / 1000
    on_hit = nearest(notes, hits) <= hit_ms
    inside = (notes >= 1.0) & (notes <= dur - 1.0)
    kicks, dropped = notes[on_hit & inside], notes[~on_hit & inside]
    shaped = np.sort(drum_kicks(drums[:n], SR))
    other = shaped[(nearest(shaped, notes) > KICK_NEAR_MS / 1000) & (nearest(shaped, snares) > hit_ms)]
    uncertain = np.sort(np.concatenate([dropped, other]))
    spans = sorted((max(0.0, u + MASK[0]), min(dur, u + MASK[1])) for u in uncertain)
    masked, end = 0.0, 0.0
    for a, b in spans:
        masked += max(0.0, b - max(a, end))
        end = max(end, b)
    bpm = res['tempo_points'][0]['bpm']
    row = dict(kind='trigger', mix_kind='stem_sum', mix_path=None, mix_parts=[str(cfg['drums']), str(cfg['rest'])],
               mix_sha256=None, parts_sha256=[sha(cfg['drums']), sha(cfg['rest'])], sample_rate=SR, duration_s=round(dur, 3),
               als=res['source'], kick_track=cfg['kick'], snare_tracks=list(cfg['snare']), kick_stem=None, kick_lag_ms=0.0,
               kick_lag_z=None, export_offset_s=round(off, 4), export_offset_beats=round(off * bpm / 60, 2),
               trigger_notes=int(inside.sum()), kicks_on_hit=len(kicks), dropped_notes=len(dropped), strong_low_hits=len(hits),
               kick_shaped_hits=len(shaped), unmarked_kick_shaped=len(other), masked_share=round(masked / dur, 3), rejected=bool(masked / dur > MASK_MAX),
               labels=[round(float(t), 4) for t in kicks], uncertain=[round(float(t), 4) for t in uncertain])
    print(f'{name}: offset {off:+.3f} s ({row["export_offset_beats"]:+.2f} beats); notes {row["trigger_notes"]} on a hit '
          f'{len(kicks)} dropped {len(dropped)}; kick-shaped drum hits {len(shaped)}, unmarked {len(other)}; '
          f'unscored {100 * row["masked_share"]:.0f}% of {dur:.0f} s{"  REJECTED" if row["rejected"] else ""}', flush=True)
    if not row['rejected']:
        np.save(GOAL / f'{name}_mix.npy', mix.astype(np.float32))
    return row


def main():
    names = sys.argv[1:] or list(TRIGGER)
    path = GOAL / 'labels_trigger.json'
    out = json.loads(path.read_text())['new'] if path.exists() else {}
    for name in names:
        out[name] = labels(name, TRIGGER[name])
    path.write_text(json.dumps(dict(method=__doc__, new=out), indent=1, default=str))
    print(path, sha(path))


if __name__ == '__main__':
    main()
