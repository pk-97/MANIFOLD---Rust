"""Shared held-out scoring for every event detector family (snare, and the bass and synth families next).

A family's labels file (detector_labels) gives each song: positives, unscored windows, loop_spans (other parts whose
own events are baked in: only positives and vouched negatives count there), hard_neg (vouched non-events) and
positives_only (recall-only songs). Candidates carry onset and emission times; a fire is stamped at its emission.

Rules every family shares:
- Training rows: candidates within LAB_TOL of a positive are y = 1; rows inside unscored windows are dropped; inside
  loop spans only positives and vouched negatives train; recall-only songs train only on positives.
- Fires: probability >= cutoff, at least REFR after the previous fire's emission.
- Scoring at TOL: a positive is caught by any fire within TOL; a fire is false only outside unscored windows and, in
  loop spans, only when it sits on a vouched negative.
- The cutoff for a held-out song is chosen on the other songs' held-out predictions; never on the song itself.
"""
import numpy as np

HOP, TOL, LAB_TOL, REFR = 256, .070, .035, .060
THRESHOLDS = np.linspace(.05, .99, 95)


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
    """(training mask, y, vouched-negative flag) per candidate onset (s) for one song's labels s."""
    pos, hard = np.array(s['positives']), np.array(s['hard_neg'])
    y = (nearest(onset, pos) <= LAB_TOL).astype(float)
    hard_near = nearest(onset, hard) <= LAB_TOL
    mask = ~inside(onset, s['unscored'])
    in_loop = inside(onset, s['loop_spans'])
    mask &= ~in_loop | (y == 1) | hard_near
    if s['positives_only']:
        mask &= y == 1
    return mask, y, hard_near & (y == 0)


def fires(p, avail, th, sr, refr=REFR):
    """Indices of firing candidates: p >= th, emissions at least refr apart."""
    out, last = [], -1e9
    for i in np.flatnonzero(p >= th):
        if (avail[i] - last) * HOP / sr >= refr - 1e-12:
            out.append(i)
            last = avail[i]
    return np.array(out, int)


def score(s, emit, p, avail, th, sr):
    """(caught, positives, false fires) for one song at cutoff th."""
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


def pooled(items, th):
    """Summed (caught, positives, false) over (labels, emit, p, avail, sr) items."""
    m = n = f = 0
    for s, emit, p, avail, sr in items:
        a, b, c = score(s, emit, p, avail, th, sr)
        m, n, f = m + a, n + b, f + c
    return m, n, f


def prf(m, n, f):
    r, p = m / max(1, n), m / max(1, m + f)
    return r, p, 2 * r * p / max(1e-9, r + p)


def choose(items, min_precision=None):
    """Cutoff over (labels, emit, p, avail, sr) items: the best pooled F1, or with min_precision the lowest cutoff
    whose pooled precision reaches it (the most recall at that precision; .99 when none does)."""
    if min_precision is not None:
        return next((th for th in THRESHOLDS if prf(*pooled(items, th))[1] >= min_precision), .99)
    best = (-1, .5)
    for th in THRESHOLDS:
        best = max(best, (prf(*pooled(items, th))[2], th))
    return best[1]


def held_out(lab, meta, held, min_precision=None, log=print):
    """Score every song with a cutoff chosen on the others. meta[t] = (labels, emit, avail, sr); held[t] = held-out
    probabilities. Recall-only songs never choose a cutoff. Returns (caught, positives, false)."""
    tm = tn = tf = 0
    songs = list(held)
    for t in songs:
        th = choose([(meta[u][0], meta[u][1], held[u], meta[u][2], meta[u][3]) for u in songs
                     if u != t and not lab[u]['positives_only']], min_precision)
        s, emit, avail, sr = meta[t]
        m, n, f = score(s, emit, held[t], avail, th, sr)
        tm, tn, tf = tm + m, tn + n, tf + f
        if log:
            log(f'  {t:14s} cutoff {th:.2f}: {m}/{n} caught, {f} false fires')
    return tm, tn, tf
