#!/usr/bin/env python3
"""Build-up training clips for the kick network (BUG-qy8q7: high-passed build-up rolls are missed): kick_goal_synth's
kick-swap clips with a build-up in the middle.

For target song A and donor B (both with a whole-song kick stem), A's other stems are summed as the bed. A window of
CLIP_S starts before one of A's kick notes. Its first part keeps A's own kick pattern with B's hits (as the kick-swap
clips do); then BUILD_BARS bars of a roll on A's beat grid (beat = A's median note spacing, folded into 0.3-0.6 s):
quarters, eighths, then sixteenths, each hit high-passed (4th-order Butterworth) at a cutoff sweeping exponentially
from HP_LO to a random HP_HI in 800-2500 Hz, at a level that rises 6 dB; then A's pattern again, full range (the drop).
Labels: every pasted hit. Unscored: A's drum-stem kicks off the pasted notes, the clip edges.
Saved as GOAL/buildup/{A}__{B}.npz in kick_goal_synth's format plus 'roll' (candidate rows on a roll hit), so
run_kick_goal_fast.py (KICK_GOAL_NN_BUILDUP=1) trains nets on them and reads held-out roll recall.
Usage: KICK_GOAL_MORE=1 KICK_GOAL_TRIGGER=1 KICK_GOAL_TRUTH=v3 kick_goal_buildup.py   (KICK_GOAL_JOBS processes)
"""
from __future__ import annotations

import sys
from concurrent.futures import ProcessPoolExecutor
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[3]))

import numpy as np  # noqa: E402
from scipy.signal import butter, sosfilt  # noqa: E402

from tools.audio_analysis.eval.kick_fusion_bandwise import fusion_features  # noqa: E402
from tools.audio_analysis.eval.kick_goal_eval import GOAL, jobs  # noqa: E402
from tools.audio_analysis.eval.kick_goal_labels import drum_kicks, load  # noqa: E402
from tools.audio_analysis.eval.kick_goal_nn import spectrum  # noqa: E402
from tools.audio_analysis.eval.kick_goal_synth import FADE_S, HIT_S, hits, notes, rms, sources, stem_sum  # noqa: E402
from tools.audio_analysis.eval.run_kick_fusion_trial import training_labels  # noqa: E402

SR = 48000
CLIP_S = 40.0
BUILD_BARS = 8
HP_LO = 120.0
DONORS = 4
OUT = GOAL / 'buildup'


def paste(kick, h, p, stop, target, level, fade):
    h = h[:stop].copy()
    h[-fade:] *= np.linspace(1, 0, fade)
    h *= target / rms(h[:int(.05 * SR)]) * level
    kick[p:p + stop] += h


def clip(a, b, src, rng):
    kick_a, rest_a, drums_a = src[a]
    ka, rest = load(kick_a, SR), stem_sum(rest_a)
    n = min(len(ka), len(rest))
    ka, rest = ka[:n], rest[:n]
    na = notes(a)
    na = na[(na > .05) & (na < n / SR - HIT_S)]
    donor = [h for h in hits(load(src[b][0], SR), notes(b)) if rms(h[:int(.05 * SR)]) > 1e-4]
    if len(na) < 40 or len(donor) < 20:
        return None
    beat = float(np.median(np.diff(na)))
    while beat < .3:
        beat *= 2
    while beat > .6:
        beat /= 2
    build_s = BUILD_BARS * 4 * beat
    first = na[na < n / SR - CLIP_S]
    if not len(first):
        return None
    start = max(0.0, float(rng.choice(first)) - rng.uniform(1, 4))
    s0, s1 = int(start * SR), int(min(n / SR, start + CLIP_S) * SR)
    b0 = start + rng.uniform(6, 10)
    b1 = b0 + build_s
    if b1 + 6 > s1 / SR:
        return None
    level = 10 ** (rng.uniform(-3, 3) / 20)
    target = float(np.median([rms(ka[int(t * SR):int(t * SR) + int(.05 * SR)]) for t in na[:200]]))
    fade = int(FADE_S * SR)
    hp_hi = rng.uniform(800, 2500)
    kick = np.zeros(s1 - s0)
    times, roll = [], []
    outside = na[(na >= start) & (na < s1 / SR) & ((na < b0 - .05) | (na >= b1))]
    for i, t in enumerate(outside):
        h = donor[rng.integers(len(donor))]
        p = int((t - start) * SR)
        nxt = outside[i + 1] if i + 1 < len(outside) else t + HIT_S
        if t < b0:
            nxt = min(nxt, b0)  # a hit before the build stops where the build starts
        stop = min(len(h), int((nxt - t) * SR), len(kick) - p)
        if stop <= fade:
            continue
        paste(kick, h, p, stop, target, level * 10 ** (rng.uniform(-1, 1) / 20), fade)
        times.append(float(t - start))
        roll.append(False)
    h = donor[rng.integers(len(donor))]
    grid = []
    for bar in range(BUILD_BARS):
        div = 1 if bar < BUILD_BARS // 2 else 2 if bar < 3 * BUILD_BARS // 4 else 4
        grid += [b0 + (bar * 4 + k / div) * beat for k in range(4 * div)]
    for i, t in enumerate(grid):
        frac = (t - b0) / build_s
        sos = butter(4, HP_LO * (hp_hi / HP_LO) ** frac, btype='high', fs=SR, output='sos')
        nxt = grid[i + 1] if i + 1 < len(grid) else b1
        p = int((t - start) * SR)
        stop = min(len(h), int((nxt - t) * SR), len(kick) - p)
        if stop <= fade:
            continue
        hp = sosfilt(sos, h)
        g = target / rms(h[:int(.05 * SR)]) * level * 10 ** (6 * frac / 20)
        seg = hp[:stop].copy() * g
        seg[-fade:] *= np.linspace(1, 0, fade)
        kick[p:p + stop] += seg
        times.append(float(t - start))
        roll.append(True)
    mix = rest[s0:s1] + kick
    mix *= rms(rest[s0:s1] + ka[s0:s1]) / rms(mix)
    if np.max(np.abs(mix)) > .99:
        mix = .99 * np.tanh(mix / .99)
    keep = [(t, r) for t, r in zip(times, roll) if 1.0 <= t <= len(mix) / SR - 1.0]
    regions = [(0.0, 1.2), (len(mix) / SR - 1.0, len(mix) / SR + 1.0)]
    for d in drums_a:
        for u in drum_kicks(load(d, SR)[s0:s1], SR):
            if not any(abs(t - u) <= .07 for t in times):
                regions.append((u - .07, u + .2))
    return mix, keep, regions


def build(task):
    a, b, seed = task
    path = OUT / f'{a}__{b}.npz'
    if path.exists():
        return a, b, 'cached'
    made = clip(a, b, STATE['src'], np.random.default_rng(seed))
    if made is None:
        return a, b, 'skipped'
    mix, keep, regions = made
    truth = [t for t, _ in keep]
    rolls = np.array([t for t, r in keep if r])
    cand, avail, _, hop = fusion_features(mix, SR)
    src = dict(track=f'{a}__{b}', group='original_five', truth=truth, regions=[dict(start_s=x, end_s=y) for x, y in sorted(regions)])
    mask, y, _ = training_labels(src, None, cand, avail, SR, hop, len(mix) / SR)
    onset = (cand + 1) * hop / SR
    roll = (y == 1) & (np.min(np.abs(onset[:, None] - rolls[None, :]), axis=1) <= .035) if len(rolls) else np.zeros(len(y), bool)
    np.savez(path, spec=spectrum(mix, SR).astype(np.float16), onset_s=onset, emit_s=(avail + 1) * hop / SR, mask=mask, y=y,
             target=a, donor=b, roll=roll)
    return a, b, f'kicks {int(y[mask].sum())} of {len(truth)} labels ({int(roll.sum())} roll), candidates {int(mask.sum())}'


STATE = {}


def _init():
    STATE['src'] = sources()


def main():
    OUT.mkdir(exist_ok=True)
    _init()
    names = sorted(STATE['src'])
    rng = np.random.default_rng(1)
    tasks = []
    for i, a in enumerate(names):
        for b in rng.choice([x for x in names if x != a], min(DONORS, len(names) - 1), replace=False):
            tasks.append((a, str(b), 5000 + 1000 * i + len(tasks)))
    with ProcessPoolExecutor(max(1, jobs()), initializer=_init) as ex:
        for a, b, msg in ex.map(build, tasks):
            print(f'{a} <- {b}: {msg}', flush=True)


if __name__ == '__main__':
    main()
