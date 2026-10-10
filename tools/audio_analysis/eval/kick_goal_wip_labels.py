#!/usr/bin/env python3
"""Section-scored training songs from Peter's WIP mixdowns.

Usage: kick_goal_wip_labels.py [--wip2]   (writes GOAL/labels_wip.json, or with --wip2 GOAL/labels_wip2.json, and
GOAL/{song}_mix.npy)

--wip2: the proven 2024/2025 WIPs (kick_goal_wip_2425; every wip_labels/{song}.json it wrote).

Source: the 2026 project survey (2026-10-10), one JSON per project in
~/.cache/manifold/ableton/wip_labels/. Each WIP's export start was proven against
its newest .als on whole bars (usually beat 64) with one constant lag (about
50 ms: MP3 encoder delay), with Corrosion and Got It as known-good controls.
Kicks are the project's kick notes inside usable 8-bar sections: sections where
no other drum source plays, plus sections where loops play but the mix shows at
most 2 kick-like hits per 8 bars off the kick notes (Peter: Got It's kicks are
clean). Only those sections are scored; the rest of each song is unscored.
The WIPs are decoded as the survey decoded them (soundfile, resample_poly to
48 kHz), so its lag holds.
"""
from __future__ import annotations

import json
import math
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[3]))

import numpy as np  # noqa: E402
import soundfile as sf  # noqa: E402
from scipy.signal import resample_poly  # noqa: E402

from tools.audio_analysis.eval.kick_goal_labels import GOAL, sha  # noqa: E402

SR = 48000
SURVEY = Path.home() / '.cache/manifold/ableton/wip_labels'
WIP = ('48_hours', 'facade', 'got_it', 'renew', 'flight')
WIP2_LABELS = GOAL / 'labels_wip2.json'
# The 2024/2025 WIP songs (kick_goal_wip_2425), once labelled.
WIP2 = tuple(json.loads(WIP2_LABELS.read_text())['new']) if WIP2_LABELS.exists() else ()


def decode(path):
    x, sr = sf.read(path, dtype='float64', always_2d=True)
    x = x.mean(axis=1)
    if sr != SR:
        g = math.gcd(SR, sr)
        x = resample_poly(x, SR // g, sr // g)
    return x


def labels(name):
    s = json.loads((SURVEY / f'{name}.json').read_text())
    mix = decode(s['wip'])
    spans = sorted([tuple(x) for x in s['scored_spans_s']] + [tuple(x) for x in s['other_drums_but_clean_spans_s']])
    kicks = sorted(float(t) for t in s['kicks_s'] + s['kicks_in_other_drums_but_clean_s'])
    np.save(GOAL / f'{name}_mix.npy', mix.astype(np.float32))
    print(f'{name}: {len(kicks)} kicks in {len(spans)} sections, {sum(b - a for a, b in spans) / 60:.2f} of {len(mix) / SR / 60:.2f} min scored',
          flush=True)
    return dict(kind='sections', mix_kind='wip', mix_path=None, mix_parts=[s['wip']], mix_sha256=sha(s['wip']), sample_rate=SR,
                duration_s=round(len(mix) / SR, 3), als=s['als'], als_sha256=s['als_sha256'], offset_beats=s['offset_beats'],
                lag_ms=s['lag_ms'], kick_stem=None, kick_lag_ms=0.0, kick_lag_z=None, labels=kicks,
                scored_spans_s=[list(x) for x in spans])


def main():
    from tools.audio_analysis.eval.kick_goal_eval import GOAL as G
    if '--wip2' in sys.argv:
        W = json.loads((G / 'snare_wip.json').read_text())
        names = tuple(t for t in W if (SURVEY / f'{t}.json').exists())
        path = WIP2_LABELS
    else:
        names, path = WIP, GOAL / 'labels_wip.json'
    out = {name: labels(name) for name in names}
    path.write_text(json.dumps(dict(method=__doc__, new=out), indent=1))
    print(path, sha(path))


if __name__ == '__main__':
    main()
