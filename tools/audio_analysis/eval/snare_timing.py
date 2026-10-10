#!/usr/bin/env python3
"""Timing check for a song's snare labels (kept + dropped): where the mix's 1-8 kHz attack peak sits within +-80 ms of
each label (median shift and spread), and the attack rise there. A wrong export offset shows as a large or scattered
shift. Usage: snare_timing.py SONG..."""
import json
import os
import sys
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parents[3]))
import numpy as np
from tools.audio_analysis.eval.snare_labels import env_db
from tools.audio_analysis.eval.detector_songs import audio, rate
from tools.audio_analysis.eval.kick_goal_eval import GOAL, Goal


def main():
    g = Goal(mode='v3')
    lab = json.loads((GOAL / 'snare_labels.json').read_text())
    for t in sys.argv[1:]:
        x, sr = audio(g, t), rate(g, t)
        e = env_db(x, sr, 1000, 8000)
        d = np.diff(e, prepend=e[0])
        hits = np.sort(np.concatenate([lab[t]['positives'], lab[t]['dropped']]))
        sh, rise = [], []
        for h in hits:
            j = int(h * 1000)
            if 100 <= j < len(e) - 100:
                k = j - 80 + int(np.argmax(d[j - 80:j + 80]))
                sh.append(k - j)
                rise.append(e[k:k + 20].max() - np.median(e[k - 60:k - 20]))
        sh = np.array(sh)
        print(f'{t}: {len(hits)} labels, attack shift median {np.median(sh):+.0f} ms, IQR {np.percentile(sh, 25):+.0f}..{np.percentile(sh, 75):+.0f} ms, '
              f'within 10 ms {np.mean(np.abs(sh - np.median(sh)) <= 10):.0%}, median rise {np.median(rise):.1f} dB')


if __name__ == '__main__':
    main()
