#!/usr/bin/env python3
"""Kick-swap training clips for the kick network: a real song with its own kick replaced by another song's kicks.

Usage: KICK_GOAL_MORE=1 KICK_GOAL_TRIGGER=1 KICK_GOAL_TRUTH=v3 kick_goal_synth.py   (KICK_GOAL_JOBS processes)

For target song A and donor B, both with a whole-song kick stem: A's other stems
are summed, and at each of A's kick notes one of B's kick hits is pasted,
level-matched to A's own kick at that note (+-1 dB per hit, one +-3 dB offset
per clip). A's groove, arrangement and sidechain ducking stay real; only the
kick sound changes. A hit runs from its note to the next B note, at most
HIT_S, cut short before A's next note, with a 5 ms fade. The sum is scaled to
A's own stem-sum level and only soft-limited when it would clip.
Labels: A's kick notes. Unscored: A's kick-shaped drum-stem hits with no kick
note within 70 ms (kicks the swap does not replace), and the clip edges.
Clips are CLIP_S long, from a random point where A has kicks. A clip may train
a net only when neither A nor B is the held-out song.
Each clip is saved as GOAL/synth/{A}__{B}.npz: the network spectrum (float16),
the onset picker's candidates as onset and emission seconds, and the
training mask and labels, all built by the same code as real songs.
"""
from __future__ import annotations

import sys
from concurrent.futures import ProcessPoolExecutor
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[3]))

import numpy as np  # noqa: E402

from tools.audio_analysis.eval.kick_fusion_bandwise import fusion_features  # noqa: E402
from tools.audio_analysis.eval.kick_goal_eval import GOAL, jobs  # noqa: E402
from tools.audio_analysis.eval.kick_goal_labels import DEV_STEMS, MORE, NEW, drum_kicks, load  # noqa: E402
from tools.audio_analysis.eval.kick_goal_nn import spectrum  # noqa: E402
from tools.audio_analysis.eval.run_kick_fusion_trial import training_labels  # noqa: E402

SR = 48000
CLIP_S = 60.0
HIT_S = .5
FADE_S = .005
DONORS = 8
SYNTH = GOAL / 'synth'
# Stem folders also hold full or partial mixes that carry the original kick.
NOT_STEMS = ('LATE NIGHT - NO VOX', 'Second Verse No Pianos')


def sources():
    """{song: (kick paths, other stem paths, drum stem paths)} for every song with a whole-song kick stem."""
    out = {}
    for name, cfg in {**DEV_STEMS, **NEW, **MORE}.items():
        if cfg.get('kick') is None:
            continue
        kick = cfg['kick'] if isinstance(cfg['kick'], (list, tuple)) else [cfg['kick']]
        if cfg.get('parts'):
            parts = list(cfg['parts'])
        else:
            folder = Path(cfg['parts_dir']) if cfg.get('parts_dir') else Path(kick[0]).parent
            parts = sorted(p for p in folder.glob('*') if p.suffix.lower() == '.wav')
        rest = [p for p in parts if Path(p) not in {Path(k) for k in kick} and not any(s in Path(p).stem for s in NOT_STEMS)]
        out[name] = (kick, rest, list(cfg.get('drums') or []))
    return out


def notes(t):
    return np.load(GOAL / f'notes_{t}.npy')


def stem_sum(paths):
    xs = [load(p, SR) for p in paths]
    out = np.zeros(max(len(x) for x in xs))
    for x in xs:
        out[:len(x)] += x
    return out


def rms(x):
    return float(np.sqrt(np.mean(x ** 2) + 1e-20))


def hits(kick, times):
    """Each note's hit: from the note to the next note, at most HIT_S."""
    out = []
    for i, t in enumerate(times):
        a = int(t * SR)
        b = int(min(times[i + 1] if i + 1 < len(times) else t + HIT_S, t + HIT_S) * SR)
        if 0 <= a < b <= len(kick):
            out.append(kick[a:b])
    return out


def clip(a, b, src, rng):
    kick_a, rest_a, drums_a = src[a]
    ka, rest = load(kick_a, SR), stem_sum(rest_a)
    n = min(len(ka), len(rest))
    ka, rest = ka[:n], rest[:n]
    na = notes(a)
    na = na[(na > .05) & (na < n / SR - HIT_S)]
    donor = hits(load(src[b][0], SR), notes(b))
    donor = [h for h in donor if rms(h[:int(.05 * SR)]) > 1e-4]
    if len(na) < 20 or len(donor) < 20:
        return None
    # A clip window where A has kicks: start a little before a random note in the first part of the song.
    first = na[na < n / SR - CLIP_S]
    if not len(first):
        return None
    start = max(0.0, float(rng.choice(first)) - rng.uniform(1, 4))
    s0, s1 = int(start * SR), int(min(n / SR, start + CLIP_S) * SR)
    kick = np.zeros(s1 - s0)
    level = 10 ** (rng.uniform(-3, 3) / 20)
    inside = na[(na >= start) & (na < s1 / SR)]
    fade = int(FADE_S * SR)
    for i, t in enumerate(inside):
        h = donor[rng.integers(len(donor))]
        p = int((t - start) * SR)
        stop = min(len(h), (int((inside[i + 1] - t) * SR) if i + 1 < len(inside) else len(h)), len(kick) - p)
        if stop <= fade:
            continue
        h = h[:stop].copy()
        h[-fade:] *= np.linspace(1, 0, fade)
        target = rms(ka[int(t * SR):int(t * SR) + int(.05 * SR)])
        h *= target / rms(h[:int(.05 * SR)]) * level * 10 ** (rng.uniform(-1, 1) / 20)
        kick[p:p + stop] += h
    mix = rest[s0:s1] + kick
    mix *= rms(rest[s0:s1] + ka[s0:s1]) / rms(mix)
    peak = np.max(np.abs(mix))
    if peak > .99:
        mix = .99 * np.tanh(mix / .99)
    truth = [float(t - start) for t in inside if 1.0 <= t - start <= CLIP_S - 1.0]
    regions = [(0.0, 1.2), (len(mix) / SR - 1.0, len(mix) / SR + 1.0)]
    for d in drums_a:
        for u in drum_kicks(load(d, SR)[s0:s1], SR):
            if not np.any(np.abs(inside - start - u) <= .07):
                regions.append((u - .07, u + .2))
    return mix, truth, regions


def build(task):
    a, b, seed = task
    path = SYNTH / f'{a}__{b}.npz'
    if path.exists():
        return a, b, 'cached'
    made = clip(a, b, STATE['src'], np.random.default_rng(seed))
    if made is None:
        return a, b, 'skipped'
    mix, truth, regions = made
    cand, avail, _, hop = fusion_features(mix, SR)
    src = dict(track=f'{a}__{b}', group='original_five', truth=truth,
               regions=[dict(start_s=x, end_s=y) for x, y in sorted(regions)])
    mask, y, _ = training_labels(src, None, cand, avail, SR, hop, len(mix) / SR)
    np.savez(path, spec=spectrum(mix, SR).astype(np.float16), onset_s=(cand + 1) * hop / SR,
             emit_s=(avail + 1) * hop / SR, mask=mask, y=y, target=a, donor=b)
    return a, b, f'kicks {int(y[mask].sum())} of {len(truth)} labels, candidates {int(mask.sum())}'


STATE = {}


def _init():
    STATE['src'] = sources()


def main():
    SYNTH.mkdir(exist_ok=True)
    _init()
    src = STATE['src']
    rng = np.random.default_rng(0)
    names = sorted(src)
    tasks = []
    for i, a in enumerate(names):
        for b in rng.choice([x for x in names if x != a], min(DONORS, len(names) - 1), replace=False):
            tasks.append((a, str(b), 1000 * i + len(tasks)))
    with ProcessPoolExecutor(max(1, jobs()), initializer=_init) as ex:
        for a, b, msg in ex.map(build, tasks):
            print(f'{a} <- {b}: {msg}', flush=True)


if __name__ == '__main__':
    main()
