#!/usr/bin/env python3
"""BUG-9ngk8.2.1 (fast breakbeats and rolls): where does the released snare pipeline (snare_release.reference:
candidates, then the net at its cutoff with a 60 ms refractory) lose fast snares?

Each clip puts snare one-shots (the loudest isolated hits of a snare stem) on a music bed at TEMPO bpm, 4 bars per
pattern: backbeat (2 and 4, the control), eighths, sixteenths, a sixteenth roll that rises 12 dB, and a break
(sixteenth grid, accents on 2 and 4, ghost notes 12 dB down). Every placed hit is a snare. Prints per pattern: hits,
hits with a candidate within 35 ms, and hits caught by a fire within 70 ms. The bed is the no-kick part of Pattern;
its own snares are not counted. The model saw these songs; only the arrangement is new.
Usage: snare_roll_sim.py"""
import sys
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parents[3]))
from tools.audio_analysis import snare_release as R  # noqa: E402  (sets the recipe env before the research modules)
import numpy as np  # noqa: E402

SR = 48000
TEMPO = 174.0
ABL = Path('/Users/peterkiemann/Library/CloudStorage/Dropbox/Music Production/Ableton Projects')
SNARE = ABL / '2024/Eleonora - Cold - Remix Project/STEMS - Cold/_ SNARE.wav'
BED = ABL / '2025/Pattern Project/MASTERS/32Bit/Pattern - NO KICK - 32Bit.wav'
BED_START_S = 60.0


def shots(n=4):
    from tools.audio_analysis.eval.detector_labels import stem_hits
    from tools.audio_analysis.eval.kick_goal_labels import load
    x = load(str(SNARE), SR)
    t = stem_hits(x, SR)[0]
    t = t[np.concatenate([[True], np.diff(t) > .3]) & np.concatenate([np.diff(t) > .3, [True]])]
    a = (t * SR).astype(int) - int(.003 * SR)
    a = a[(a > 0) & (a + int(.2 * SR) < len(x))]
    out = [x[i:i + int(.2 * SR)] for i in a]
    order = np.argsort([-np.abs(s).max() for s in out])[:n]
    return [out[i] * np.linspace(1, 0, len(out[i])) ** 2 for i in order]


def patterns():
    """{name: [(beat, gain dB)]} over 4 bars."""
    out = {'backbeat': [(b * 4 + k, 0) for b in range(4) for k in (1, 3)],
           'eighths': [(k / 2, 0) for k in range(32)],
           'sixteenths': [(k / 4, 0) for k in range(64)],
           'rising roll': [(k / 4, -12 + 12 * k / 63) for k in range(64)]}
    brk = []
    for k in range(64):
        pos = k % 16
        if pos in (4, 12):
            brk.append((k / 4, 0))
        elif pos in (2, 7, 10, 11, 14, 15):
            brk.append((k / 4, -12))
    out['break'] = brk
    return out


def main():
    import pickle
    from tools.audio_analysis.eval.detector_eval import nearest
    from tools.audio_analysis.eval.kick_goal_labels import load
    with open(R.FINAL, 'rb') as fh:
        final = pickle.load(fh)
    net = R.net_module(final)
    bed = load(str(BED), SR)[int(BED_START_S * SR):]
    beat = 60 / TEMPO
    tot = {}
    for j, shot in enumerate(shots()):
        for name, pat in patterns().items():
            times = np.array([1.0 + b * beat for b, _ in pat])
            x = bed[:int((times[-1] + 1.0) * SR)].copy() * .5
            for (b, gain), t in zip(pat, times):
                i = int(round(t * SR)) - int(.003 * SR)
                x[i:i + len(shot)] += shot * 10 ** (gain / 20)
            x = x / max(1.0, np.abs(x).max() / .98)
            r = R.reference(x, final, net)
            fired = r['emit_s'][np.searchsorted(r['avail_hop'], r['fires'])] if len(r['fires']) else np.zeros(0)
            cand = int(np.sum(nearest(times, r['onset_s']) <= .035)) if len(r['onset_s']) else 0
            hit = int(np.sum(nearest(times, fired) <= .07)) if len(fired) else 0
            a = tot.setdefault(name, [0, 0, 0])
            a[0], a[1], a[2] = a[0] + len(times), a[1] + cand, a[2] + hit
            print(f'shot {j} {name:12s}: {len(times)} snares, {cand} with a candidate, {hit} caught', flush=True)
    for name, (n, c, h) in tot.items():
        print(f'all shots {name:12s}: {n} snares, candidates {c / n:.2f}, caught {h / n:.2f}', flush=True)


if __name__ == '__main__':
    main()
