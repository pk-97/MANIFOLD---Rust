#!/usr/bin/env python3
"""Snare-mix training clips: real snare/clap one-shots pasted into real song mixes, with exact labels.

Sample pool: the one-shot snare/clap files Peter's projects use (snare_samples.json; reverb impulse files dropped)
plus loud hits cut from the snare/clap stems of the stem songs. Each pool entry carries the songs that use it.
For every song in the goal set, CLIPS clips of CLIP_S seconds, each with one pool sample:
- positions: every second kick (a clap layered on the beat) and midpoints between kicks at least 350 ms apart (a
  backbeat between kicks), at least 250 ms apart, 70% kept; random times when the clip has too few kicks.
- level: per clip a random offset of 6-16 dB over the mix's own 1-8 kHz level just before the hit, +-2 dB per hit.
- labels: a pasted hit is a positive only when the mix shows the prominent 1-8 kHz attack (snare_labels.on_attack);
  the rest are unscored. In a labelled snare song the song's own labels carry over (positions avoid its snares and
  unscored windows; vouched non-snares within 70 ms of a pasted hit are dropped). In any other song only the pasted
  hits count (positives_only): its own snares are unknown, so no candidate there is called a non-snare.
A clip may train a net only when its song is not held out and the held-out song's project does not use its sample.
Saves GOAL/snare_mix/{song}__{n}.npz: 64-band net spectrum (float16), candidates, rows, measures, song, sample, users.
Usage: snare_mix.py  (KICK_GOAL_JOBS processes)"""
import json
import os
import re
import zlib
import sys
from concurrent.futures import ProcessPoolExecutor
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parents[3]))
import numpy as np  # noqa: E402

CLIPS, CLIP_S, HOP = 3, 60.0, 256
STATE = {}


def pool(g):
    from tools.audio_analysis.eval.kick_goal_eval import GOAL
    from tools.audio_analysis.eval.detector_labels import merge_layers, stem_hits
    from tools.audio_analysis.eval.snare_labels import FAMILY
    from tools.audio_analysis.eval.kick_goal_eval import STEM_CFG, load
    out = []
    for name, e in json.loads((GOAL / 'snare_samples.json').read_text()).items():
        if re.search(r'snare|clap', name, re.I) and not name.startswith('Hybrid_'):
            # The 808 kit is AIFF-C compressed (unreadable here, even by afconvert): skipped.
            if not name.lower().endswith(('.aif', '.aiff')):
                out.append(dict(name=name, audio=load(e['path'], 48000), users=e['users']))
    for t, stems in FAMILY.stem_songs.items():
        info = g.labels['new'].get(t) or {}
        dirs = {Path(p).parent for p in info.get('mix_parts') or []}
        if t in STEM_CFG:
            k = STEM_CFG[t]['kick']
            dirs |= {Path(p).parent for p in (k if isinstance(k, list) else [k])}
        for s in stems:
            x = load(str(next(d / s for d in dirs if (d / s).exists())), 48000)
            loud = merge_layers(stem_hits(x, 48000)[0])
            for n, h in enumerate(loud[np.linspace(0, len(loud) - 1, min(6, len(loud))).astype(int)] if len(loud) else []):
                nxt = loud[loud > h + .05]
                end = min(h + .35, (nxt[0] - .005) if len(nxt) else h + .35)
                a = int((h - .005) * 48000)
                cut = x[a:int(end * 48000)].copy()
                cut[-240:] *= np.linspace(1, 0, min(240, len(cut)))
                out.append(dict(name=f'{t}:{s}#{n}', audio=cut, users=[t]))
    return out


def positions(kicks, rng, length):
    kicks = kicks[(kicks > .3) & (kicks < length - .5)]
    p = list(kicks[rng.integers(2)::2])
    gaps = np.diff(kicks)
    p += list((kicks[:-1] + gaps / 2)[gaps >= .35])
    if len(p) < length / 4:
        p += list(rng.uniform(.3, length - .5, int(length / 2)))
    p = np.sort(np.array(p))
    keep, last = [], -1.0
    for t in p:
        if t - last >= .25 and rng.random() < .7:
            keep.append(t)
            last = t
    return np.array(keep)


def build(task):
    t, n = task
    from tools.audio_analysis.eval.snare_cands import candidates
    from tools.audio_analysis.eval.snare_stack import measures
    from tools.audio_analysis.eval.detector_labels import env_db, on_attack
    from tools.audio_analysis.eval.detector_eval import inside, nearest, rows
    from tools.audio_analysis.eval.kick_goal_nn import spectrum
    from tools.audio_analysis.eval.run_kick_goal_tail_component import mix_audio
    from tools.audio_analysis.eval.kick_goal_eval import GOAL
    path = GOAL / 'snare_mix' / f'{t}__{n}.npz'
    if path.exists():
        return f'{path.stem}: cached'
    g, lab, smp = STATE['g'], STATE['lab'], STATE['pool']
    rng = np.random.default_rng(zlib.crc32(f"{t}:{n}".encode()))
    r = g.records[t]
    sr = r['sample_rate']
    x = mix_audio(g, t)
    if len(x) / sr < CLIP_S + 2:
        return f'{path.stem}: too short'
    a = rng.uniform(1, len(x) / sr - CLIP_S - 1)
    seg = x[int(a * sr):int((a + CLIP_S) * sr)].copy()
    kicks = np.asarray(r.get('stem_kicks') if r.get('stem_kicks') is not None else r.get('all_labels', r.get('truth', [])), float) - a
    pos = positions(kicks, rng, CLIP_S)
    own = None
    if t in lab:
        s = lab[t]
        own = {k: [[u - a for u in w] for w in s[k]] for k in ('unscored', 'loop_spans')}
        own['positives'] = [u - a for u in s['positives']]
        own['hard_neg'] = [u - a for u in s['hard_neg']]
        pos = pos[(nearest(pos, own['positives']) > .15) & ~inside(pos, own['unscored'])]
    sm = smp[rng.integers(len(smp))]
    hit = sm['audio'] if sr == 48000 else np.interp(np.arange(0, len(sm['audio']) * sr / 48000) * 48000 / sr, np.arange(len(sm['audio'])), sm['audio'])
    hit_db = env_db(hit, sr, 1000, 8000).max()
    env = env_db(seg, sr, 1000, 8000)
    off = rng.uniform(6, 16)
    for p in pos:
        j = int(p * 1000)
        local = np.median(env[max(0, j - 60):max(1, j - 20)])
        gain = 10 ** ((local + off + rng.uniform(-2, 2) - hit_db) / 20)
        i = int(p * sr)
        m = min(len(hit), len(seg) - i)
        seg[i:i + m] += gain * hit[:m]
    peak = np.abs(seg).max()
    if peak > .99:
        seg *= .99 / peak
    ok = on_attack(env_db(seg, sr, 1000, 8000), pos)
    unscored = [[u - .07, u + .07] for u in pos[~ok]]
    if own is None:
        clip = dict(positives=list(pos[ok]), unscored=unscored, loop_spans=[], hard_neg=[], positives_only=True)
    else:
        hard = np.array(own['hard_neg'])
        hard = hard[nearest(hard, pos) > .07] if len(hard) else hard
        clip = dict(positives=sorted(own['positives'] + list(pos[ok])), unscored=own['unscored'] + unscored,
                    loop_spans=own['loop_spans'], hard_neg=sorted(hard), positives_only=False)
    cand, avail = candidates(seg, sr, 4.5)
    onset, emit = (cand + 1) * HOP / sr, (avail + 1) * HOP / sr
    mask, y, hard = rows(clip, onset)
    np.savez(path, spec=spectrum(seg, sr).astype(np.float16), onset_s=onset, emit_s=emit, avail=avail, sr=sr, mask=mask, y=y,
             hard=hard, X=measures(seg, sr, cand, avail), song=t, sample=sm['name'], users=json.dumps(sm['users']),
             labels=json.dumps(clip))
    return f'{path.stem}: {sm["name"][:40]} {int(ok.sum())}/{len(pos)} pasted prominent, rows {int(mask.sum())} ({int(y[mask].sum())} snares)'


def _init():
    from tools.audio_analysis.eval.kick_goal_eval import GOAL, Goal
    STATE['g'] = Goal(mode='v3')
    STATE['lab'] = json.loads((GOAL / 'snare_labels.json').read_text())
    STATE['pool'] = pool(STATE['g'])


def main():
    from tools.audio_analysis.eval.kick_goal_eval import GOAL, Goal, jobs
    (GOAL / 'snare_mix').mkdir(exist_ok=True)
    songs = list(Goal(mode='v3').records)
    tasks = [(t, n) for t in songs for n in range(CLIPS)]
    with ProcessPoolExecutor(jobs(), initializer=_init) as ex:
        for line in ex.map(build, tasks):
            print(line, flush=True)


if __name__ == '__main__':
    main()
