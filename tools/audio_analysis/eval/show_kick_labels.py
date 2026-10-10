#!/usr/bin/env python3
"""Kick labels for a live-show recording, carried over from a song's kick stem through the show's .als.

Usage: show_kick_labels.py EXTRACT.json RECORDING_PREFIX MASTER_FILE KICK_STEM [OUT.json]

Chain: kick-stem notes (kick_goal_rolls.kick_notes) -> master seconds (label-free
envelope lag, z >= 8) -> arrangement beats (the master clip's warp) -> recording
seconds (the recording clip's warp markers).
A live set can cut, filter or fade a master by hand, which the .als does not
record, so each bar is scored only when the master is audibly there:
the 40-150 Hz rise envelopes of recording and master peak within BAR_LAG_MS
at a correlation >= BAR_CORR. (BAR_CORR was .4 at lag 0 until take 0012 showed
Midnight present at a steady +1 ms under extra live low end, r ~ .38.)
Bars overlapping another drum clip (track or file name contains "drum") are
masked: they carry kicks the stem does not.
Orphans check the labels from the audio side: hits in scored bars at least
half as strong as the median labelled kick, with no label within ORPHAN_MS.
The master's own orphans over the same bars are the song's bass; a take whose
orphan count exceeds the master's by more than ORPHAN_MAX of its labels is untrusted
(unlabelled live kicks, or a broken chain).
"""
from __future__ import annotations

import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[3]))

import numpy as np  # noqa: E402
import soundfile as sf  # noqa: E402
from scipy.signal import butter, sosfiltfilt  # noqa: E402

from tools.audio_analysis.eval.als_extract import clip_arr_beat, clip_seconds  # noqa: E402
from tools.audio_analysis.eval.kick_attack_rejection import read_audio  # noqa: E402
from tools.audio_analysis.eval.kick_goal_labels import kick_env_db, lag  # noqa: E402
from tools.audio_analysis.eval.kick_goal_rolls import kick_notes  # noqa: E402

BAR_CORR = .3
ORPHAN_MS = 30
ORPHAN_MAX = .05
BAR_LAG_MS = 3
LAG_Z_MIN = 8.0
SEARCH_MS = 50


def rise_env(path, t0, dur):
    info = sf.info(path)
    a, sr = sf.read(path, start=max(0, int(round(t0 * info.samplerate))), frames=int(round(dur * info.samplerate)), always_2d=True)
    x = sosfiltfilt(butter(4, [40, 150], btype='band', fs=sr, output='sos'), a.mean(axis=1))
    hop = sr // 1000
    n = len(x) // hop
    e = np.log10(np.mean(x[:n * hop].reshape(n, hop) ** 2, axis=1) + 1e-10)
    return np.maximum(0, np.diff(e, prepend=e[0]))


def bar_present(rec_path, rec_t0, master_path, master_t0, dur):
    """(best correlation, its lag ms) of one bar, recording against master."""
    r = rise_env(rec_path, rec_t0 - SEARCH_MS / 1000, dur + 2 * SEARCH_MS / 1000)
    m = rise_env(master_path, master_t0, dur)
    n = len(m)
    best = (-2.0, 0)
    for k in range(min(2 * SEARCH_MS + 1, len(r) - n + 1)):
        seg = r[k:k + n]
        c = float(np.corrcoef(seg, m)[0, 1]) if seg.std() > 0 and m.std() > 0 else 0.0
        best = max(best, (c, k - SEARCH_MS))
    return best


def orphans(rec_path, spans, kicks):
    """Strong low-end hits inside scored spans with no label within ORPHAN_MS."""
    kicks = np.asarray(kicks)
    hits, label_strength = [], []
    for a, b in spans:
        e = rise_env(rec_path, a, b - a)
        t = a + np.arange(len(e)) / 1000.0
        pk = np.flatnonzero((e[1:-1] >= e[:-2]) & (e[1:-1] > e[2:])) + 1
        hits += [(t[i], e[i]) for i in pk]
        label_strength += [e[max(0, int((k - a) * 1000) - 15):int((k - a) * 1000) + 25].max() for k in kicks if a <= k < b - .025]
    if not label_strength:
        return []
    floor = .5 * float(np.median(label_strength))
    strong, last = [], -1.0
    for t, v in sorted(hits):
        if v >= floor and t - last > .1:
            strong.append(t)
            last = t
    return [t for t in strong if not len(kicks) or np.min(np.abs(kicks - t)) > ORPHAN_MS / 1000]


def main():
    extract = json.loads(Path(sys.argv[1]).read_text())
    rec_prefix, master_file, kick_stem = sys.argv[2], sys.argv[3], sys.argv[4]
    clips = extract['audio_clips']
    rec = next(c for c in clips if c['file'].startswith(rec_prefix))
    songs = [c for c in clips if c['file'] == master_file and not c['disabled'] and c['warped']]
    drums = [c for c in clips if not c['disabled'] and c is not rec and c['file'] != master_file
             and ('drum' in c['file'].lower() or 'drum' in c['track'].lower())]
    master_path = songs[0]['file_path']
    sr = 48000
    stem, master = read_audio(kick_stem, sr)[1], read_audio(master_path, sr)[1]
    shift, z = lag(stem, master, sr)
    if z < LAG_Z_MIN:
        sys.exit(f'kick stem does not line up with {master_file}: lag z {z:.1f}')
    notes = kick_notes(stem, sr, kick_env_db(stem, sr)) + shift
    rec_s = lambda b: clip_seconds(rec, b)  # noqa: E731
    kicks, bars = [], []
    for c in songs:
        b = c['start_beat']
        while b + 4 <= c['end_beat'] + 1e-6:
            masked = any(d['start_beat'] < b + 4 and d['end_beat'] > b for d in drums)
            corr, best = bar_present(rec['file_path'], rec_s(b), master_path, clip_seconds(c, b), rec_s(b + 4) - rec_s(b))
            scored = not masked and corr >= BAR_CORR and abs(best) <= BAR_LAG_MS
            bars.append(dict(start_beat=b, start_s=rec_s(b), end_s=rec_s(b + 4), master_start_s=clip_seconds(c, b),
                             master_end_s=clip_seconds(c, b + 4), corr=round(corr, 3), lag_ms=best,
                             masked_drums=masked, scored=scored))
            if scored:
                lo, hi = clip_seconds(c, b), clip_seconds(c, b + 4)
                kicks += [rec_s(clip_arr_beat(c, t)) for t in notes if lo <= t < hi]
            b += 4
    spans = [(r['start_s'], r['end_s']) for r in bars if r['scored']]
    master_spans = [(r['master_start_s'], r['master_end_s']) for r in bars if r['scored']]
    master_kicks = sorted(t for t in notes if any(a <= t < b for a, b in master_spans))
    rec_orph = orphans(rec['file_path'], spans, sorted(kicks))
    master_orph = np.asarray(orphans(master_path, master_spans, master_kicks))
    # Counts, not one-to-one matches: peak picking jitters by tens of ms between two
    # renders, so take 0008 (a near-pure master copy, r .98) still mismatched 268 hits.
    extra = max(0, len(rec_orph) - len(master_orph))
    trusted = extra <= ORPHAN_MAX * max(1, len(kicks))
    out = dict(recording=rec['file_path'], master=master_path, kick_stem=kick_stem, stem_to_master_lag_s=shift, lag_z=z,
               rule=dict(bar_corr=BAR_CORR, bar_lag_ms=BAR_LAG_MS, orphan_ms=ORPHAN_MS, orphan_max=ORPHAN_MAX),
               trusted=trusted, live_extra_hits=extra, kicks_s=sorted(kicks), bars=bars, scored_spans_s=spans)
    dest = Path(sys.argv[5]) if len(sys.argv) > 5 else Path.home() / '.cache/manifold/ableton' / f'kicks_{Path(rec["file"]).stem}_{Path(master_file).stem[:40]}.json'
    dest.write_text(json.dumps(out, indent=1))
    n_scored = sum(r['scored'] for r in bars)
    print(f'{rec["file"][:30]}: lag {1000 * shift:.1f} ms z {z:.1f}; bars {len(bars)} scored {n_scored} '
          f'masked(drums) {sum(r["masked_drums"] for r in bars)} absent {len(bars) - n_scored - sum(r["masked_drums"] for r in bars)}; kicks {len(kicks)} unlabelled strong hits {len(rec_orph)} (master {len(master_orph)}), live extras {extra}, trusted {trusted}')


if __name__ == '__main__':
    main()
