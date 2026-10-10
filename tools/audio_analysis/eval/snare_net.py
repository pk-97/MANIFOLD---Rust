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

HOP, TOL, LAB_TOL, REFR = 256, .070, .035, .060
# Look-ahead past emission (SNARE_AHEAD_MS, default 40) and slice length (SNARE_PAST_MS, default 150); SNARE_FLAT=1 adds
# the noisiness channel (detector_channels) as a third input channel.
AHEAD = float(os.environ.get('SNARE_AHEAD_MS', '40')) / 1000
FLAT = os.environ.get('SNARE_FLAT') == '1'
TAG = ''.join(f'_{k.lower()}{os.environ[k]}' for k in ('SNARE_AHEAD_MS', 'SNARE_PAST_MS', 'SNARE_FLAT') if k in os.environ)
os.environ['KICK_GOAL_NN_PAST_MS'] = os.environ.get('SNARE_PAST_MS', '150')
if FLAT:
    os.environ['KICK_GOAL_NN_PERC'] = '1'  # the net's third channel slot carries noisiness instead


def inside(t, spans):
    t = np.asarray(t)
    out = np.zeros(len(t), bool)
    for a, b in spans:
        out |= (t >= a) & (t <= b)
    return out


def nearest(a, b):
    """For each a, distance to the nearest b."""
    b = np.sort(np.asarray(b))
    if not len(b):
        return np.full(len(a), np.inf)
    j = np.clip(np.searchsorted(b, a), 1, max(1, len(b) - 1))
    return np.minimum(np.abs(a - b[j - 1]), np.abs(a - b[np.minimum(j, len(b) - 1)]))


def rows(s, onset):
    pos, hard = np.array(s['positives']), np.array(s['hard_neg'])
    y = (nearest(onset, pos) <= LAB_TOL).astype(float)
    hard_near = nearest(onset, hard) <= LAB_TOL
    mask = ~inside(onset, s['unscored'])
    in_loop = inside(onset, s['loop_spans'])
    mask &= ~in_loop | (y == 1) | hard_near
    if s['positives_only']:
        mask &= y == 1
    return mask, y, hard_near & (y == 0)


def fires(p, avail, th, sr):
    out, last = [], -1e9
    for i in np.flatnonzero(p >= th):
        if (avail[i] - last) * HOP / sr >= REFR - 1e-12:
            out.append(i)
            last = avail[i]
    return np.array(out, int)


def score(s, emit, p, avail, th, sr):
    f = emit[fires(p, avail, th, sr)]
    pos = np.array(s['positives'])
    matched = int(np.sum(nearest(pos, f) <= TOL)) if len(f) else 0
    if s['positives_only']:
        return matched, len(pos), 0
    cand_false = f[nearest(f, pos) > TOL]
    cand_false = cand_false[~inside(cand_false, s['unscored'])]
    loop = inside(cand_false, s['loop_spans'])
    false = int(np.sum(~loop) + np.sum(loop & (nearest(cand_false, s['hard_neg']) <= LAB_TOL)))
    return matched, len(pos), false


def choose(items):
    """Cutoff maximising pooled F1 over (song labels, emit, p, avail, sr) items."""
    best = (-1, .5)
    for th in np.linspace(.05, .99, 95):
        m = n = f = 0
        for s, emit, p, avail, sr in items:
            a, b, c = score(s, emit, p, avail, th, sr)
            m, n, f = m + a, n + b, f + c
        r, prec = m / max(1, n), m / max(1, m + f)
        f1 = 2 * r * prec / max(1e-9, r + prec)
        best = max(best, (f1, th))
    return best[1]


def main():
    os.environ.setdefault('KICK_GOAL_NN_AHEAD_MS', '0')
    from tools.audio_analysis.eval.snare_cands import candidates
    from tools.audio_analysis.eval.kick_goal_eval import GOAL, Goal
    from tools.audio_analysis.eval.kick_goal_nn import Song, device, predict, spectrum_cache, train
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
        mask, y, hard = rows(s, onset)
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
    held = {}
    for t in songs:
        t0 = time.time()
        net = train([data[u] for u in songs if u != t], 1000 * songs.index(t) + seed, [c for c in clips if t not in c.blocked])
        held[t] = predict(net, data[t])
        print(f'net without {t}: {time.time() - t0:.0f} s', flush=True)
    tot_m = tot_n = tot_f = 0
    for t in songs:
        th = choose([(meta[u][0], meta[u][1], held[u], meta[u][2], meta[u][3]) for u in songs if u != t and not lab[u]['positives_only']])
        s, emit, avail, sr = meta[t]
        m, n, f = score(s, emit, held[t], avail, th, sr)
        tot_m, tot_n, tot_f = tot_m + m, tot_n + n, tot_f + f
        print(f'  {t:14s} cutoff {th:.2f}: {m}/{n} caught, {f} false fires', flush=True)
    print(f'net only, held out: R {tot_m / tot_n:.3f} P {tot_m / max(1, tot_m + tot_f):.3f} ({tot_m}/{tot_n}, {tot_f} false)')
    np.savez(GOAL / f'snare_net_s{seed}{"_mix" if mix else ""}{TAG}.npz', **held)


if __name__ == '__main__':
    main()
