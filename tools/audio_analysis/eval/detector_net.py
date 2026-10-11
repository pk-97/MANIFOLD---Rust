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
# DETECTOR_FOLDS: nets per run. 0 = one per held-out song (final numbers); N = N fixed song groups, each scored by a
# net trained on the others (iteration: a synth run takes minutes, not 40). DETECTOR_DRAW_CAP caps the draws per song
# and class each epoch (kick_goal_nn.DRAW_CAP). The family runner defaults to 4 groups and 300 draws.
FOLDS = int(os.environ.get('DETECTOR_FOLDS', '4'))
DRAW_CAP = os.environ.get('DETECTOR_DRAW_CAP', '300')
DEADLINE = 8  # hops from a candidate to its emission, every finder below
TAG = f'_p{PAST_MS}_a{round(AHEAD * 1000)}' + ('_flat' if FLAT else '') + (f'_f{FOLDS}' if FOLDS else '') + (f'_d{DRAW_CAP}' if DRAW_CAP != '0' else '')


def configure():
    """The kick net's input settings for a family run; must run before kick_goal_nn is imported."""
    os.environ['KICK_GOAL_NN_PAST_MS'] = PAST_MS
    os.environ.setdefault('KICK_GOAL_NN_AHEAD_MS', '0')
    os.environ['KICK_GOAL_NN_DRAW_CAP'] = DRAW_CAP
    if FLAT:
        os.environ['KICK_GOAL_NN_PERC'] = '1'  # the net's third channel slot carries noisiness


def family_candidates(spec_name, fam):
    """f(x, sr, spec) -> (candidate hops, available hops) for a CANDS argument."""
    from tools.audio_analysis.eval.detector_cands import band_hops, peak_candidates, rise_candidates, spectral_rise
    kind, *args = spec_name.split(':')
    a = [float(v) for v in args]

    def rise(x, sr, spec):
        return rise_candidates(band_hops(x, sr, *fam.band), a[0], 4, 6, DEADLINE)

    def change(x, sr, spec, thr=None):
        return peak_candidates(spectral_rise(spec, sr, *fam.band), a[-1] if thr is None else thr, 6, DEADLINE)

    def union(x, sr, spec):
        c = np.unique(np.concatenate([rise(x, sr, spec)[0], change(x, sr, spec, a[1])[0]]))
        keep, last = [], -100
        for h in c:
            if h - last >= 6:
                keep.append(h)
                last = h
        c = np.array(keep, int)
        return c, c + DEADLINE
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
        # Families with a tight window (labels' tol_s) are judged at the candidate; the fixed wait to emission is
        # added back as lateness (main: ahead).
        meta[t] = (s, onset if 'tol_s' in s else emit, avail, sr)
        log(f'{t}: {len(cand)} candidates, {int(mask.sum())} training rows ({int(y[mask].sum())} events, {int(hard.sum())} vouched negatives)')
    return data, meta


def groups(songs, folds):
    """Fixed song groups: names sorted, dealt round-robin, so the same song set always splits the same way. folds 0:
    one group per song in the labels' order (the seeds of the leave-one-song-out runs depend on it)."""
    if not folds:
        return [[t] for t in songs]
    names = sorted(songs)
    return [names[k::folds] for k in range(folds)]


def net_groups(songs, folds, untrained=()):
    """The song groups one run's nets hold out (groups). With one net per song, the untrained songs share one last
    net: every net would train on the same songs for them."""
    if folds or not untrained:
        return groups(songs, folds)
    return groups([t for t in songs if t not in untrained], 0) + [[t for t in songs if t in untrained]]


def held_out_nets(lab, data, seed, clips=(), log=print, folds=0, untrained=(), train_kw=None, record=None):
    """{song: held-out probabilities}: per song group (net_groups), a net trained on every other song (and the clips
    not blocked for any song in the group). Songs in untrained are scored but never trained on. train_kw goes to
    kick_goal_nn.train; record, when given, gets each net's group, trained songs and clips."""
    from tools.audio_analysis.eval.kick_goal_nn import predict, train
    held = {}
    for k, grp in enumerate(net_groups(list(lab), folds, untrained)):
        t0 = time.time()
        songs = [u for u in lab if u not in grp and u not in untrained]
        used = [c for c in clips if not set(grp) & c.blocked]
        net = train([data[u] for u in songs], 1000 * k + seed, used, **(train_kw or {}))
        for t in grp:
            held[t] = predict(net, data[t])
        if record is not None:
            record.append(dict(group=grp, trained=songs, clips=[c.name for c in used], seconds=round(time.time() - t0, 1)))
        log(f'net without {", ".join(grp)}: {time.time() - t0:.0f} s')
    return held


def main():
    configure()
    from tools.audio_analysis.eval.detector_cands import HOP
    from tools.audio_analysis.eval.detector_eval import held_out, write_results
    from tools.audio_analysis.eval.kick_goal_eval import GOAL, Goal
    from tools.audio_analysis.eval.kick_goal_nn import device
    family, cands_name = sys.argv[1], sys.argv[2]
    seed = int(sys.argv[3]) if len(sys.argv) > 3 else 0
    fam = importlib.import_module(f'tools.audio_analysis.eval.{family}_labels').FAMILY
    lab = {t: s for t, s in json.loads((GOAL / f'{family}_labels.json').read_text()).items() if s['positives']}

    def log(m):
        print(m, flush=True)
    data, meta = songs_data(Goal(mode='v3'), lab, family_candidates(cands_name, fam), device(), log)
    held = held_out_nets(lab, data, seed, log=log, folds=FOLDS)
    per_song = {}
    wait = {t: AHEAD + (DEADLINE * HOP / meta[t][3] if 'tol_s' in lab[t] else 0.0) for t in lab}
    m, n, f = held_out(lab, meta, held, log=log, per_song=per_song, ahead=wait)
    r, p = m / n, m / max(1, m + f)
    log(f'{family} net ({cands_name}{TAG}), held out: R {r:.3f} P {p:.3f} F1 {2 * r * p / max(1e-9, r + p):.3f} ({m}/{n}, {f} false)')
    name = f'{family}_net_s{seed}_{cands_name.replace(":", "_")}{TAG}'
    np.savez(GOAL / f'{name}.npz', **held)
    write_results(GOAL / f'{name}.json', per_song)


if __name__ == '__main__':
    main()
