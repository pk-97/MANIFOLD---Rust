#!/usr/bin/env python3
"""F_h22 listening renders beside the evening's A (linear15) and E (H18) files.

Same nine 8-second excerpts, the same 75% stereo mix and the same 15 ms 1800 Hz
decaying click at each actual emission time as render_kick_comparison.py.
Clicks mark detector outputs, not true kicks; no listening verdict is claimed.
"""
from __future__ import annotations

import hashlib
import json
import pickle
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[3]))

import numpy as np  # noqa: E402
import soundfile as sf  # noqa: E402

from tools.audio_analysis.eval.kick_night_common import NIGHT, Data, fire_times  # noqa: E402

EVENING = Path.home() / '.cache/manifold/kick-research-2026-10-09-evening/listening'


def main():
    d = Data()
    cases = json.loads((EVENING / 'h18_manifest.json').read_text())['cases']
    with open(NIGHT / 'validate_h22.pkl', 'rb') as f:
        fires = pickle.load(f)[2]
    out = NIGHT / 'listening'
    out.mkdir(exist_ok=True)
    rows = []
    for case in cases:
        track = case['track']
        r = d.records[track]
        path = Path(r['source']['audio_path'])
        if hashlib.sha256(path.read_bytes()).hexdigest() != case['source_sha256']:
            raise ValueError('source identity changed')
        with sf.SoundFile(path) as audio:
            sr = audio.samplerate
            first = round(case['start_s'] * sr)
            audio.seek(first)
            samples = audio.read(round(case['duration_s'] * sr), dtype='float64', always_2d=True)
        start, duration = first / sr, len(samples) / sr
        t = np.arange(round(.015 * sr)) / sr
        click = .12 * np.sin(2 * np.pi * 1800 * t) * np.exp(-t / .004)
        times = [x for x in fire_times(r, fires[track]) if start <= x < start + duration]
        output = samples * .75
        for emitted in times:
            frame = round((emitted - start) * sr)
            end = min(len(output), frame + len(click))
            output[frame:end] += click[:end - frame, None]
        if np.max(np.abs(output), initial=0) >= 1:
            raise ValueError('render would clip')
        target = out / f'{track}_F_h22.wav'
        sf.write(target, output, sr, subtype='PCM_16')
        rows.append(dict(track=track, start_s=start, duration_s=duration, clicks=len(times), file=str(target),
                         compare_with=[str(EVENING / f'{track}_A_linear15.wav'), str(EVENING / f'{track}_E_threeway.wav')]))
        print(track, 'clicks', len(times), flush=True)
    (out / 'manifest.json').write_text(json.dumps(dict(
        instructions='F is H22 main (H18 gated toward linear+glide where the kernel lacks support). Compare with the '
                     "evening's A (linear15) and E (H18). Same mix level and click; clicks are detector outputs only.",
        cases=rows), indent=1))


if __name__ == '__main__':
    main()
