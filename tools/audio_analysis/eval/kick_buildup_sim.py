#!/usr/bin/env python3
"""BUG-qy8q7 (drop kicks missed after a high-passed build-up roll): reproduce it on simulated build-ups and find
which part of the released kick pipeline (kick_release.reference: trees + net blend, then the 8 s song-relative
stage) loses the kicks.

Each clip: 8 bars of build-up then 8 bars of drop at TEMPO bpm over a kick-free music bed. Build: a kick roll on
quarters (bars 1-4), eighths (5-6) and sixteenths (7-8), each hit high-passed (4th-order Butterworth) at a cutoff
sweeping exponentially from 150 Hz to 2.5 kHz (build_hp_soft: 100 to 600 Hz). Drop: the same kick, full range, on quarters. Controls: the same
roll unfiltered, and the drop alone. Kicks are one-shots cut from a kick stem; the bed is that song's no-kick part.
Every placed kick is a kick (Peter wants the rolls too).

The final model saw these songs in training; only the filtering and the arrangement are new, so this reproduces the
failure, it does not score a fix. Fixes are scored held out with run_kick_goal_fast.py.
Usage: kick_buildup_sim.py [MODE ...]   (prints per clip and section: kicks caught by the shipped fires, by blend-only fires, and kicks with a candidate)
"""
import os
import sys
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parents[3]))
from tools.audio_analysis import kick_release as K  # noqa: E402  (sets the recipe env before the research modules)
import numpy as np  # noqa: E402
from scipy.signal import butter, sosfilt  # noqa: E402

SR = 48000
TEMPO = 174.0
BED_START_S = 60.0
SOURCES = {  # song: (kick stem, no-kick bed)
    'pattern': ('2025/Pattern Project/MASTERS/32Bit/Pattern - KICK STEM - 32Bit.wav',
                '2025/Pattern Project/MASTERS/32Bit/Pattern - NO KICK - 32Bit.wav'),
}
MODES = tuple(sys.argv[1:]) or ('build_hp', 'build_hp_soft', 'build_flat', 'drop_only')
ABL = Path('/Users/peterkiemann/Library/CloudStorage/Dropbox/Music Production/Ableton Projects')
HP = (150.0, 2500.0)
HP_SOFT = (100.0, 600.0)  # a gentler sweep: the roll keeps enough body to be caught


def one_shot(stem, n=6):
    """The n loudest isolated kicks of a kick stem, each 250 ms from 5 ms before its attack."""
    from tools.audio_analysis.eval.kick_goal_rolls import kick_notes
    from tools.audio_analysis.eval.kick_goal_labels import kick_env_db
    t = kick_notes(stem, SR, kick_env_db(stem, SR))
    t = t[np.concatenate([[True], np.diff(t) > .3]) & np.concatenate([np.diff(t) > .3, [True]])]
    a = (t * SR).astype(int) - int(.005 * SR)
    a = a[(a > 0) & (a + int(.25 * SR) < len(stem))]
    shots = [stem[i:i + int(.25 * SR)] for i in a]
    order = np.argsort([-np.abs(s).max() for s in shots])[:n]
    return [shots[i] * np.hanning(2 * len(shots[i]))[len(shots[i]):] ** .1 for i in order]


def clip(shot, bed, mode):
    """(audio, kick times, section per kick) for mode in build_hp, build_hp_soft, build_flat, drop_only."""
    beat = 60 / TEMPO
    lead = 2.0
    times, cut, sec = [], [], []
    if mode != 'drop_only':
        for bar in range(8):
            div = 1 if bar < 4 else 2 if bar < 6 else 4
            for k in range(4 * div):
                t = lead + (bar * 4 + k / div) * beat
                times.append(t)
                lo, hi = HP_SOFT if mode == 'build_hp_soft' else HP
                cut.append(lo * (hi / lo) ** ((t - lead) / (32 * beat)) if mode.startswith('build_hp') else None)
                sec.append(f'build bars {1 + bar // 2 * 2}-{2 + bar // 2 * 2}')
    d0 = lead + (32 * beat if mode != 'drop_only' else 0.0)
    for k in range(32):
        times.append(d0 + k * beat)
        cut.append(None)
        sec.append('drop first 2 s' if k * beat < 2 else 'drop after 2 s')
    n = int((times[-1] + 1.0) * SR)
    x = bed[:n].copy() * .5
    for t, c in zip(times, cut):
        s = shot if c is None else sosfilt(butter(4, c, btype='high', fs=SR, output='sos'), shot)
        i = int(round(t * SR)) - int(.005 * SR)
        x[i:i + len(s)] += s
    return x / max(1.0, np.abs(x).max() / .98), np.array(times), np.array(sec)


def caught(times, fire_s):
    from tools.audio_analysis.eval.detector_eval import nearest
    return nearest(times, fire_s) <= .07 if len(fire_s) else np.zeros(len(times), bool)


def main():
    from tools.audio_analysis.eval.kick_goal_labels import load
    os.environ['KICK_GOAL_NN_DEVICE'] = 'cpu'
    final = K.load_final()
    net = K.net_module(final)
    for song, (kick_path, bed_path) in SOURCES.items():
        stem = load(str(ABL / kick_path), SR)
        bed = load(str(ABL / bed_path), SR)[int(BED_START_S * SR):]
        for j, shot in enumerate(one_shot(stem)):
            for mode in MODES:
                x, times, sec = clip(shot, bed, mode)
                r = K.reference(x, final, net)
                shipped = r['emit_s'][r['fires']]
                blend = r['emit_s'][K.fires(r['blend_p'], r['avail_hop'], SR, r['hop'], final['cutoff'])]
                hit_s, hit_b, hit_c = caught(times, shipped), caught(times, blend), caught(times, r['onset_s'])
                rows = []
                for s in dict.fromkeys(sec):
                    m = sec == s
                    rows.append(f'{s}: {hit_s[m].sum()}/{m.sum()} (blend {hit_b[m].sum()}, candidates {hit_c[m].sum()})')
                print(f'{song} shot {j} {mode:10s} | ' + ' | '.join(rows), flush=True)


if __name__ == '__main__':
    main()
