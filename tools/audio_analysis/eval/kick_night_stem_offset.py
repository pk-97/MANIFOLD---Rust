#!/usr/bin/env python3
"""Fullband waveform offset between each kick stem and its master (labels unused).

Positive lag means the master is later. Three 16 s windows per song, centred on
reviewed passages, searched over +-1 s with FFT correlation, as the evening's
Midnight alignment did.
"""
from __future__ import annotations

import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[3]))

import numpy as np  # noqa: E402
from scipy.signal import correlate, resample_poly  # noqa: E402

from tools.audio_analysis.eval.kick_attack_rejection import read_audio  # noqa: E402
from tools.audio_analysis.eval.kick_night_common import NIGHT, Data  # noqa: E402
from tools.audio_analysis.eval.kick_night_snap_labels import KICK, STEMS  # noqa: E402


def main():
    d = Data()
    out = {}
    for track, rel in KICK.items():
        r = d.records[track]
        sr, master = read_audio(r['source']['audio_path'])
        ssr, stem = read_audio(str(STEMS / rel))
        if ssr != sr:
            stem = resample_poly(stem, sr, ssr)
        centres = sorted({round((p['start_s'] + p['end_s']) / 2) for p in r['source']['passages']})
        picks = [centres[0], centres[len(centres) // 2], centres[-1]]
        rows = []
        for c in picks:
            a, b = int((c - 8) * sr), int((c + 8) * sr)
            s = stem[a:b]
            m = master[max(0, a - sr):b + sr]
            xc = correlate(m, s, mode='valid', method='fft')
            norm = np.sqrt(np.sum(s ** 2) * np.convolve(m ** 2, np.ones(len(s)), mode='valid')) + 1e-12
            z = xc / norm
            k = int(np.argmax(z))
            lag = (max(0, a - sr) + k - a) / sr
            second = np.max(np.where(np.abs(np.arange(len(z)) - k) > int(.02 * sr), z, -1))
            rows.append(dict(centre_s=c, lag_ms=round(1000 * lag, 3), corr=round(float(z[k]), 3), runner_up=round(float(second), 3)))
        out[track] = dict(sample_rate=sr, stem_rate=ssr, master_s=len(master) / sr, stem_s=len(stem) / sr, windows=rows)
        print(track, sr, ssr, round(len(master) / sr, 3), round(len(stem) / sr, 3), rows, flush=True)
    (NIGHT / 'stem_offsets.json').write_text(json.dumps(out, indent=1))


if __name__ == '__main__':
    main()
