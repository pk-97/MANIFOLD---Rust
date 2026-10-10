#!/usr/bin/env python3
"""Listening check for snare labels: the 30 s of a song with the most labelled snares, mix at -6 dB, a short high blip
(2.5 kHz) on every kept snare and a low blip (400 Hz) on every dropped (too quiet) one. Writes OUT_DIR/{song}_snares.wav.
Usage: snare_listen.py OUT_DIR SONG..."""
import json
import sys
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parents[3]))
import numpy as np  # noqa: E402
from scipy.io import wavfile  # noqa: E402

WIN = 30.0


def blip(sr, hz):
    n = int(.03 * sr)
    return np.sin(2 * np.pi * hz * np.arange(n) / sr) * np.hanning(n) * .35


def main():
    from tools.audio_analysis.eval.kick_goal_eval import GOAL, Goal
    from tools.audio_analysis.eval.run_kick_goal_tail_component import mix_audio
    out = Path(sys.argv[1])
    g = Goal(mode='v3')
    lab = json.loads((GOAL / 'snare_labels.json').read_text())
    for t in sys.argv[2:]:
        sr = g.records[t]['sample_rate']
        x = mix_audio(g, t)
        kept, dropped = np.array(lab[t]['positives']), np.array(lab[t]['dropped'])
        both = np.sort(np.concatenate([kept, dropped]))
        a = max(0.0, both[int(np.argmax([np.sum((both >= s) & (both < s + WIN)) for s in both]))] - .5)
        y = x[int(a * sr):int((a + WIN) * sr)] * .5
        for times, hz in ((kept, 2500), (dropped, 400)):
            b = blip(sr, hz)
            for s in times[(times >= a) & (times < a + WIN - .05)]:
                i = int((s - a) * sr)
                y[i:i + len(b)] += b
        wavfile.write(out / f'{t}_snares.wav', sr, (np.clip(y, -1, 1) * 32767).astype(np.int16))
        k = np.sum((kept >= a) & (kept < a + WIN))
        d = np.sum((dropped >= a) & (dropped < a + WIN))
        print(f'{t}: {a:.1f}-{a + WIN:.1f} s of the master, {k} kept (high blip), {d} dropped (low blip)')


if __name__ == '__main__':
    main()
