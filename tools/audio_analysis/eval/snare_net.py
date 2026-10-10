#!/usr/bin/env python3
"""Snare detector, step 2: the kick's spectrum CNN alone on snare candidates, leave-one-song-out.

Candidates: snare_cands (4.5 dB). Each song's candidates get y = 1 within 35 ms of a labelled snare. Training rows:
everything outside unscored windows; inside loop spans only positives and vouched non-snares (hard_neg within 35 ms);
recall-only songs only positives. Net input: kick_goal_nn's 64-band spectrum, slice ending 40 ms after emission.
Each song is scored by a net that never saw it; its cutoff is chosen on the other songs' held-out predictions
(pooled F1). Scoring at +-70 ms: a fire is false only outside unscored windows and, inside loop spans, only when it
sits on a vouched non-snare. With --mix, the snare-mix clips (snare_mix.py) join training: a clip trains the net for held-out song t only when
it was not cut from t and t's project does not use its sample; together the clips get as many draws as the real songs.
Usage: snare_net.py [SEED] [--mix]"""
import json
import os
import sys
from pathlib import Path
import time
sys.path.insert(0, str(Path(__file__).resolve().parents[3]))
import numpy as np  # noqa: E402

from tools.audio_analysis.eval.detector_eval import HOP, dense_recall, held_out, rows  # noqa: E402
# Look-ahead past emission (SNARE_AHEAD_MS, default 40) and slice length (SNARE_PAST_MS, default 150); SNARE_FLAT=1 adds
# the noisiness channel (detector_channels) as a third input channel.
AHEAD = float(os.environ.get('SNARE_AHEAD_MS', '40')) / 1000
FLAT = os.environ.get('SNARE_FLAT') == '1'
# SNARE_DENSE=1: the prominent snares inside fast runs train as positives (BUG-9ngk8.2.1); scoring is unchanged.
DENSE = os.environ.get('SNARE_DENSE') == '1'
TAG = ''.join(f'_{k.lower()}{os.environ[k]}' for k in ('SNARE_AHEAD_MS', 'SNARE_PAST_MS', 'SNARE_FLAT', 'SNARE_DENSE') if k in os.environ)
os.environ['KICK_GOAL_NN_PAST_MS'] = os.environ.get('SNARE_PAST_MS', '150')
if FLAT:
    os.environ['KICK_GOAL_NN_PERC'] = '1'  # the net's third channel slot carries noisiness instead


def main():
    os.environ.setdefault('KICK_GOAL_NN_AHEAD_MS', '0')
    from tools.audio_analysis.eval.snare_cands import candidates
    from tools.audio_analysis.eval.kick_goal_eval import GOAL, Goal
    from tools.audio_analysis.eval.detector_net import held_out_nets
    from tools.audio_analysis.eval.kick_goal_nn import Song, device
    from tools.audio_analysis.eval.detector_channels import flat_cache
    from tools.audio_analysis.eval.detector_songs import audio, rate, spec
    args = [a for a in sys.argv[1:] if a != '--mix']
    seed = int(args[0]) if args else 0
    mix = '--mix' in sys.argv
    g = Goal(mode='v3')
    lab = json.loads((GOAL / 'snare_labels.json').read_text())
    dev = device()
    data, meta = {}, {}
    for t, s in lab.items():
        sr = rate(g, t)
        cand, avail = candidates(audio(g, t), sr, 4.5)
        onset, emit = (cand + 1) * HOP / sr, (avail + 1) * HOP / sr
        mask, y, hard = rows(s, onset, DENSE)
        extra = (flat_cache(g, t),) if FLAT else ()
        data[t] = Song(spec(g, t), onset, emit + AHEAD, mask, y, t, hard, extra).to(dev)
        meta[t] = (s, emit, avail, sr)
        print(f'{t}: {len(cand)} candidates, {int(mask.sum())} training rows ({int(y[mask].sum())} snares, {int(hard.sum())} vouched non-snares)', flush=True)
    songs = list(lab)
    clips = []
    if mix:
        for p in sorted((GOAL / 'snare_mix').glob('*.npz')):
            with np.load(p) as z:
                c = Song(z['spec'].astype(np.float32), z['onset_s'], z['emit_s'] + AHEAD, z['mask'], z['y'], p.stem, z['hard']).to(dev)
                c.blocked = {str(z['song'])} | set(json.loads(str(z['users'])))
            clips.append(c)
        print(f'{len(clips)} mix clips, {sum(int(c.y[c.mask].sum()) for c in clips)} pasted or own snares', flush=True)
    held = held_out_nets(lab, data, seed, clips, log=lambda m: print(m, flush=True))
    cut = {}
    tot_m, tot_n, tot_f = held_out(lab, meta, held, log=lambda m: print(m, flush=True), cutoffs=cut)
    print(f'net only, held out: R {tot_m / tot_n:.3f} P {tot_m / max(1, tot_m + tot_f):.3f} ({tot_m}/{tot_n}, {tot_f} false)')
    dm, dn = dense_recall(lab, meta, held, cut)
    print(f'fast runs (unscored above): {dm}/{dn} prominent snares caught')
    np.savez(GOAL / f'snare_net_s{seed}{"_mix" if mix else ""}{TAG}.npz', **held)


if __name__ == '__main__':
    main()
