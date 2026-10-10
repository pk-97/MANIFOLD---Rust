#!/usr/bin/env python3
"""Held-out net for any detector family: the kick net (kick_goal_nn) on the family's candidates, one net per held-out
song trained on the others, each song scored at a cutoff chosen on the other songs (detector_eval).

Net input (env, read before kick_goal_nn loads): DETECTOR_PAST_MS slice (default 190: four time segments),
DETECTOR_AHEAD_MS past emission (default 40), DETECTOR_FLAT=1 (default) adds the noisiness channel (detector_channels).
Candidates: 'rise:DB' (the family band's energy rises DB over the last 4 hops' minimum) or 'change:DB' (net bands in
the family range rise DB over their own last-20 ms mean; detector_cands.spectral_rise), or 'union:RISE:CHANGE'.
Saves GOAL/{family}_net_s{seed}_{cands}{TAG}.npz (held-out probabilities per song).
Usage: detector_net.py FAMILY CANDS [SEED]   (FAMILY bass or synth: GOAL/{FAMILY}_labels.json, {FAMILY}_labels.FAMILY)"""
import importlib
import json
import os
import sys
import time
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parents[3]))
import numpy as np  # noqa: E402

PAST_MS = os.environ.get('DETECTOR_PAST_MS', '190')
AHEAD = float(os.environ.get('DETECTOR_AHEAD_MS', '40')) / 1000
FLAT = os.environ.get('DETECTOR_FLAT', '1') == '1'
TAG = f'_p{PAST_MS}_a{round(AHEAD * 1000)}' + ('_flat' if FLAT else '')


def configure():
    """The kick net's input settings for a family run; must run before kick_goal_nn is imported."""
    os.environ['KICK_GOAL_NN_PAST_MS'] = PAST_MS
    os.environ.setdefault('KICK_GOAL_NN_AHEAD_MS', '0')
    if FLAT:
        os.environ['KICK_GOAL_NN_PERC'] = '1'  # the net's third channel slot carries noisiness


def family_candidates(spec_name, fam):
    """f(x, sr, spec) -> (candidate hops, available hops) for a CANDS argument."""
    from tools.audio_analysis.eval.detector_cands import band_hops, peak_candidates, rise_candidates, spectral_rise
    kind, *args = spec_name.split(':')
    a = [float(v) for v in args]

    def rise(x, sr, spec):
        return rise_candidates(band_hops(x, sr, *fam.band), a[0], 4, 6, 8)

    def change(x, sr, spec, thr=None):
        return peak_candidates(spectral_rise(spec, sr, *fam.band), a[-1] if thr is None else thr, 6, 8)

    def union(x, sr, spec):
        c = np.unique(np.concatenate([rise(x, sr, spec)[0], change(x, sr, spec, a[1])[0]]))
        keep, last = [], -100
        for h in c:
            if h - last >= 6:
                keep.append(h)
                last = h
        c = np.array(keep, int)
        return c, c + 8
    return {'rise': rise, 'change': change, 'union': union}[kind]


def songs_data(g, lab, cands, dev, log):
    """(Song per labelled song, scoring metadata per song)."""
    from tools.audio_analysis.eval.detector_cands import HOP
    from tools.audio_analysis.eval.detector_channels import flat_cache
    from tools.audio_analysis.eval.detector_eval import rows
    from tools.audio_analysis.eval.detector_songs import audio, rate, spec
    from tools.audio_analysis.eval.kick_goal_nn import Song
    data, meta = {}, {}
    for t, s in lab.items():
        sr = rate(g, t)
        sp = spec(g, t)
        cand, avail = cands(audio(g, t), sr, sp)
        onset, emit = (cand + 1) * HOP / sr, (avail + 1) * HOP / sr
        mask, y, hard = rows(s, onset)
        data[t] = Song(sp, onset, emit + AHEAD, mask, y, t, hard, (flat_cache(g, t),) if FLAT else ()).to(dev)
        meta[t] = (s, emit, avail, sr)
        log(f'{t}: {len(cand)} candidates, {int(mask.sum())} training rows ({int(y[mask].sum())} events, {int(hard.sum())} vouched negatives)')
    return data, meta


def held_out_nets(lab, data, seed, clips=(), log=print):
    """{song: held-out probabilities}: per song, a net trained on every other song (and the clips not blocked for it)."""
    from tools.audio_analysis.eval.kick_goal_nn import predict, train
    songs = list(lab)
    held = {}
    for t in songs:
        t0 = time.time()
        net = train([data[u] for u in songs if u != t], 1000 * songs.index(t) + seed, [c for c in clips if t not in c.blocked])
        held[t] = predict(net, data[t])
        log(f'net without {t}: {time.time() - t0:.0f} s')
    return held


def main():
    configure()
    from tools.audio_analysis.eval.detector_eval import held_out
    from tools.audio_analysis.eval.kick_goal_eval import GOAL, Goal
    from tools.audio_analysis.eval.kick_goal_nn import device
    family, cands_name = sys.argv[1], sys.argv[2]
    seed = int(sys.argv[3]) if len(sys.argv) > 3 else 0
    fam = importlib.import_module(f'tools.audio_analysis.eval.{family}_labels').FAMILY
    lab = {t: s for t, s in json.loads((GOAL / f'{family}_labels.json').read_text()).items() if s['positives']}

    def log(m):
        print(m, flush=True)
    data, meta = songs_data(Goal(mode='v3'), lab, family_candidates(cands_name, fam), device(), log)
    held = held_out_nets(lab, data, seed, log=log)
    m, n, f = held_out(lab, meta, held, log=log)
    r, p = m / n, m / max(1, m + f)
    log(f'{family} net ({cands_name}{TAG}), held out: R {r:.3f} P {p:.3f} F1 {2 * r * p / max(1e-9, r + p):.3f} ({m}/{n}, {f} false)')
    np.savez(GOAL / f'{family}_net_s{seed}_{cands_name.replace(":", "_")}{TAG}.npz', **held)


if __name__ == '__main__':
    main()
