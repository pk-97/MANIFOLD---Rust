#!/usr/bin/env python3
"""Recall-only training songs: WIP mixdowns whose kick trigger notes are certain kicks
but whose break loops carry kicks no project track marks.

Usage: kick_goal_recall_labels.py   (writes GOAL/labels_recall.json and GOAL/{song}_mix.npy)

The offset is not proven from the audio: breaks and bass hit every 16th, so the
mix fits quarter-beat shifts as well, and the projects' only unwarped clips are
muted references. It rests on Ableton's MP3 exports: every proven WIP starts on a
whole bar with a 48-54 ms lag, and these three fit beat 64 with 50-52 ms. A
kick-solo bounce from each project would prove it.

Peter, 2026-10-10: these projects have usable timing and audio. The 2026 survey
(~/.cache/manifold/ableton/wip_labels/) dropped them because breaks play under the
kick almost throughout, so no section has every kick marked.
- Kicks: the kick track's notes, moved by the export offset, where the mix's
  40-150 Hz level rises at least RISE_DB over RISE_MS somewhere within HIT_MS. A
  note without one is not scored: it may not have sounded. (The trigger songs'
  12 dB strong-hit test finds only 53% of Lowkey's notes: its kicks sit under
  breaks and bass.)
- Export offset: the survey's (whole bars plus one encoder lag, fitted to the kick
  pattern). A free fit on the mix's low hits is pulled by the bass (Lowkey: 0.75
  beat off). The song is rejected when fewer than MIN_ON_HIT of its notes land
  on a hit.
- Scoring: only a window around each kick (MASK) is scored, so a song counts toward
  recall and never toward precision. Training uses its kicks only: nothing in it
  is taught as a non-kick (positives_only).
"""
from __future__ import annotations

import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[3]))

import numpy as np  # noqa: E402
from scipy.signal import butter, sosfilt  # noqa: E402

from tools.audio_analysis.eval.als_extract import extract  # noqa: E402
from tools.audio_analysis.eval.kick_goal_labels import GOAL, sha  # noqa: E402
from tools.audio_analysis.eval.kick_goal_trigger_labels import HIT_MS, MASK, note_times  # noqa: E402
from tools.audio_analysis.eval.kick_goal_wip_labels import SR, SURVEY, decode  # noqa: E402

# Recognition is left out: 12 kick notes and an export offset the survey could not prove.
RECALL = ('lowkey', 'default_haze', 'overwhelming_force')
MIN_ON_HIT = .75
RISE_DB = 6
RISE_MS = 15


def low_rise(x, times):
    """Largest 40-150 Hz level rise over RISE_MS (dB, 1 ms grid) peaking within HIT_MS of each time."""
    lo = sosfilt(butter(4, [40, 150], btype='band', fs=SR, output='sos'), x)
    hop = SR // 1000
    n = len(lo) // hop
    e = 10 * np.log10(np.mean(lo[:n * hop].reshape(n, hop) ** 2, axis=1) + 1e-12)
    rise = np.zeros(n)
    rise[RISE_MS:] = e[RISE_MS:] - e[:-RISE_MS]
    return np.array([rise[max(0, int(t * 1000) - HIT_MS):int(t * 1000) + HIT_MS + 1].max(initial=0.0) for t in times])


def labels(name):
    s = json.loads((SURVEY / f'{name}.json').read_text())
    res = extract(s['als'])
    mix = decode(s['wip'])
    dur = len(mix) / SR
    notes = note_times(res, s['kick_sources'][0])
    off = s['offset_s']
    notes = notes + off
    inside = (notes >= 1.0) & (notes <= dur - 1.0)
    on_hit = low_rise(mix, notes) >= RISE_DB
    if np.mean(on_hit[inside]) < MIN_ON_HIT:
        sys.exit(f'{name}: only {np.mean(on_hit[inside]):.2f} of its notes on a mix hit')
    kicks = notes[inside & on_hit]
    spans = []
    for k in kicks:
        a, b = k + MASK[0], k + MASK[1]
        if spans and a <= spans[-1][1]:
            spans[-1][1] = b
        else:
            spans.append([a, b])
    np.save(GOAL / f'{name}_mix.npy', mix.astype(np.float32))
    print(f'{name}: offset {off:+.3f} s, {len(kicks)} kicks of {int(inside.sum())} notes inside the WIP '
          f'({int((inside & ~on_hit).sum())} without a mix hit, unscored)', flush=True)
    return dict(kind='sections', positives_only=True, mix_kind='wip', mix_path=None, mix_parts=[s['wip']],
                mix_sha256=sha(s['wip']), sample_rate=SR, duration_s=round(dur, 3), als=s['als'], als_sha256=s['als_sha256'],
                kick_track=s['kick_sources'][0], offset_s=round(off, 4), kick_stem=None, kick_lag_ms=0.0, kick_lag_z=None,
                labels=[float(k) for k in kicks], scored_spans_s=spans)


def main():
    out = {name: labels(name) for name in RECALL}
    path = GOAL / 'labels_recall.json'
    path.write_text(json.dumps(dict(method=__doc__, new=out), indent=1))
    print(path, sha(path))


if __name__ == '__main__':
    main()
