"""Shared held-out scoring for every event detector family (snare, and the bass and synth families next).

A family's labels file (detector_labels) gives each song: positives, unscored windows, loop_spans (other parts whose
own events are baked in: only positives and vouched negatives count there), hard_neg (vouched non-events) and
positives_only (recall-only songs). Candidates carry onset and emission times; a fire is stamped at its emission.

Rules every family shares:
- Training rows: candidates within LAB_TOL of a positive are y = 1; rows inside unscored windows are dropped; inside
  loop spans only positives and vouched negatives train; recall-only songs train only on positives.
- Fires: probability >= cutoff, at least REFR after the previous fire's emission.
- Scoring at TOL (or the labels' tol_s, or their one-sided window_s [lo, hi]: fire minus label): a positive is caught
  by any fire within it; a fire is false only outside unscored windows and, in loop spans, only when it sits on a
  vouched negative. Labels with window_s also train their positives from candidates inside that window.
- The cutoff for a held-out song is chosen on the other songs' held-out predictions; never on the song itself.
"""
import numpy as np

HOP, TOL, LAB_TOL, REFR = 256, .070, .035, .060
THRESHOLDS = np.linspace(.05, .99, 95)


def inside(t, spans):
    """Whether each t lies in any closed span [a, b]. Spans may overlap: t is inside when the latest end among
    spans starting at or before t reaches it."""
    t = np.asarray(t, float)
    if not len(spans) or not len(t):
        return np.zeros(len(t), bool)
    sp = np.asarray(spans, float)
    sp = sp[np.argsort(sp[:, 0], kind='stable')]
    reach = np.maximum.accumulate(sp[:, 1])
    j = np.searchsorted(sp[:, 0], t, side='right') - 1
    return (j >= 0) & (t <= reach[np.maximum(j, 0)])


def nearest(a, b):
    """For each a, distance to the nearest b."""
    b = np.sort(np.asarray(b))
    if not len(b):
        return np.full(len(a), np.inf)
    j = np.clip(np.searchsorted(b, a), 1, max(1, len(b) - 1))
    return np.minimum(np.abs(a - b[j - 1]), np.abs(a - b[np.minimum(j, len(b) - 1)]))


def within(a, b, lo, hi):
    """For each a, whether some b lies in [a + lo, a + hi]."""
    b = np.sort(np.asarray(b, float))
    a = np.asarray(a, float)
    if not len(b):
        return np.zeros(len(a), bool)
    j = np.searchsorted(b, a + lo, 'left')
    return (j < len(b)) & (b[np.minimum(j, len(b) - 1)] <= a + hi)


def rows(s, onset, dense=False):
    """(training mask, y, vouched-negative flag) per candidate onset (s) for one song's labels s. With dense, the
    prominent events inside fast runs (s['dense_pos']) also train as positives; nothing else in a run trains."""
    pos, hard = np.array(s['positives']), np.array(s['hard_neg'])
    if 'window_s' in s:
        lo, hi = s['window_s']
        y = within(onset, pos, -hi, -lo).astype(float)
    else:
        y = (nearest(onset, pos) <= LAB_TOL).astype(float)
    hard_near = nearest(onset, hard) <= LAB_TOL
    mask = ~inside(onset, s['unscored'])
    in_loop = inside(onset, s['loop_spans'])
    mask &= ~in_loop | (y == 1) | hard_near
    if dense and s.get('dense_pos'):
        run = nearest(onset, np.array(s['dense_pos'])) <= LAB_TOL
        y[run] = 1.0
        mask |= run
    if s['positives_only']:
        mask &= y == 1
    return mask, y, hard_near & (y == 0)


def fires(p, avail, th, sr, refr=REFR, hop=HOP):
    """Indices of firing candidates: p >= th, emissions at least refr apart (avail in hops of hop samples)."""
    out, last = [], -1e9
    for i in np.flatnonzero(p >= th):
        if (avail[i] - last) * hop / sr >= refr - 1e-12:
            out.append(i)
            last = avail[i]
    return np.array(out, int)


def tol(s):
    """A song's hit window: its labels' tol_s (tight for dense families, matched at the candidate) or TOL."""
    return s.get('tol_s', TOL)


def judged(s, stamp, p, avail, th, sr, hop=HOP):
    """(caught, positives, false fire times, fire times) for one song at cutoff th; fires at stamp."""
    f = stamp[fires(p, avail, th, sr, hop=hop)]
    pos = np.array(s['positives'])
    if 'window_s' in s:
        lo, hi = s['window_s']
        matched = int(np.sum(within(pos, f, lo, hi))) if len(f) else 0
        false = f[~within(f, pos, -hi, -lo)]
    else:
        w = tol(s)
        matched = int(np.sum(nearest(pos, f) <= w)) if len(f) else 0
        false = f[nearest(f, pos) > w] if len(pos) else f
    if s['positives_only']:
        return matched, len(pos), f[:0], f
    false = false[~inside(false, s['unscored'])]
    false = false[~inside(false, s['loop_spans']) | (nearest(false, s['hard_neg']) <= LAB_TOL)]
    return matched, len(pos), false, f


def score(s, emit, p, avail, th, sr, hop=HOP):
    """(caught, positives, false fires) for one song at cutoff th."""
    m, n, false, _ = judged(s, emit, p, avail, th, sr, hop)
    return m, n, len(false)


def song_result(s, emit, p, avail, th, sr, ahead=0.0, hop=HOP):
    """One song's scored result for comparisons (detector_compare): caught, positives, false fires (and their times),
    cutoff, scored minutes (duration less unscored windows) and the lateness of each caught event's trigger in ms
    (the decision, stamp + ahead, minus the label)."""
    m, n, false, fired = judged(s, emit, p, avail, th, sr, hop)
    late = []
    for t in s['positives']:
        if not len(fired):
            continue
        if 'window_s' in s:
            lo, hi = s['window_s']
            near = fired[(fired - t >= lo) & (fired - t <= hi)]
            if len(near):
                late.append(round(1000 * (near[0] + ahead - t), 1))
            continue
        e = fired[np.argmin(np.abs(fired - t))]
        if abs(e - t) <= tol(s):
            late.append(round(1000 * (e + ahead - t), 1))
    gone = sum(b - a for a, b in s['unscored'])
    return dict(matched=m, labels=n, extra=len(false), cutoff=float(th), minutes=max(0.0, s['duration_s'] - gone) / 60,
                late_ms=late, false_s=[round(float(v), 3) for v in false])


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
    return choose_from([pooled(items, th) for th in THRESHOLDS], min_precision)


def choose_from(sums, min_precision=None):
    """choose() over pooled (caught, positives, false) sums, one per THRESHOLDS entry."""
    if min_precision is not None:
        return next((th for th, mnf in zip(THRESHOLDS, sums) if prf(*mnf)[1] >= min_precision), .99)
    best = (-1, .5)
    for th, mnf in zip(THRESHOLDS, sums):
        best = max(best, (prf(*mnf)[2], th))
    return best[1]


def held_out(lab, meta, held, min_precision=None, log=print, cutoffs=None, per_song=None, ahead=0.0, pool=None):
    """Score every song with a cutoff chosen on the others. meta[t] = (labels, emit, avail, sr[, hop]); held[t] = held-out
    probabilities. Recall-only and generated songs never choose a cutoff: cutoffs are set on real music. pool[t], when
    given, holds the other songs' probabilities to choose t's cutoff on (from models that never saw t); one dict
    object per song group. Returns (caught, positives, false); fills cutoffs[t] and per_song[t] (song_result,
    decisions at stamp + ahead; ahead a number or one per song) when given."""
    tm = tn = tf = 0
    songs = list(held)
    hop = {u: meta[u][4] if len(meta[u]) > 4 else HOP for u in songs}
    eligible = [u for u in songs if not lab[u]['positives_only'] and lab[u].get('kind') != 'generated']

    def table(probs, users):
        return {u: np.array([score(meta[u][0], meta[u][1], probs[u], meta[u][2], th, meta[u][3], hop[u]) for th in THRESHOLDS])
                for u in users}
    # Without a pool each eligible song is scored once per threshold and a cutoff is a sum over the other songs.
    own = table(held, eligible) if pool is None else None
    total = sum(own.values(), np.zeros((len(THRESHOLDS), 3), int)) if own is not None else None
    pooled_sums = {}
    for t in songs:
        if pool is None:
            sums = total - own.get(t, 0)
        else:
            key = id(pool[t])
            if key not in pooled_sums:
                pooled_sums[key] = sum(table(pool[t], [u for u in eligible if u in pool[t]]).values(),
                                       np.zeros((len(THRESHOLDS), 3), int))
            sums = pooled_sums[key]
        th = choose_from([tuple(int(v) for v in row) for row in sums], min_precision)
        if cutoffs is not None:
            cutoffs[t] = th
        s, emit, avail, sr = meta[t][:4]
        m, n, f = score(s, emit, held[t], avail, th, sr, hop[t])
        if per_song is not None:
            per_song[t] = song_result(s, emit, held[t], avail, th, sr, ahead[t] if isinstance(ahead, dict) else ahead, hop[t])
        tm, tn, tf = tm + m, tn + n, tf + f
        if log:
            log(f'  {t:14s} cutoff {th:.2f}: {m}/{n} caught, {f} false fires')
    return tm, tn, tf


def dense_recall(lab, meta, held, cutoffs, key='dense_pos'):
    """(caught, total) of the events inside fast runs (unscored in the main score), each song at its held-out cutoff."""
    m = n = 0
    for t in held:
        d = np.array(lab[t].get(key) or [])
        if not len(d):
            continue
        s, emit, avail, sr = meta[t][:4]
        f = emit[fires(held[t], avail, cutoffs[t], sr, hop=meta[t][4] if len(meta[t]) > 4 else HOP)]
        if 'window_s' in s:
            m += int(np.sum(within(d, f, *s['window_s'])))
        else:
            m += int(np.sum(nearest(d, f) <= tol(s))) if len(f) else 0
        n += len(d)
    return m, n


def write_results(path, per_song):
    """Per-song results in the layout detector_compare reads (the kick fast loop's, under the key 'net')."""
    import json
    path.write_text(json.dumps(dict(results=dict(net=dict(per_song=per_song))), indent=1, default=float))
