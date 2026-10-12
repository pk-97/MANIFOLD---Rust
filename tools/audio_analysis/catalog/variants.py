#!/usr/bin/env python3
"""Do a song's other bounces carry its labels? Every local master, premaster, WIP and mix of a composition is aligned to
the lab mix and judged: lag, correlation, residual, and a verdict.

  same_arrangement     the variant plays the same song on the same timeline (mastered, a vocal added or dropped, a
                       WIP a few weeks later): the labels transfer at the lag. A usable real master variant.
  aligned_needs_check  one global lag fits but parts do not: a section edited, a drift, or a short file
  different_arrangement  no shared timeline: the labels do not transfer

Method: the lag is remix's alignment (attack cross-correlation, refined to the sample on the loudest 8 s). Then the whole
song is cut into SEG_S windows; a window matches when the attack (onset)
envelopes of the aligned files correlate (>= MATCH_ATTACK). seg_frac is the matching share of the windows where both files sound. drift_ms is the
largest local lag found at four anchors (a tempo change or a stretched section moves it). cover is the share of the lab
mix the variant spans. verdict() is a pure function of those numbers; thresholds below, tested in test_catalog.py.

The same function judges a stem set against a bounce (ref = 'stem_sum': does this bounce sound like the stems' sum).

Usage: catalog.py variants [SONG...] | stemsum [SONG...]   (the music folder is read only: sandbox-exec wrapper)"""
import hashlib
import json
import os
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT))

import numpy as np  # noqa: E402

VERSION = 'variant-2'
BAND = (100, 8000)
Z_MIN = 8.0                # attack cross-correlation peak over the other lags (remix.LAG_Z_MIN)
SEG_S = 8.0
PEAKS = 4                  # onset lags tried per candidate
PEAK_GAP_MS = 100
SLACK_MS = 4               # a window may sit this far off the global lag (a mastering chain's latency)
MATCH_ATTACK = 0.5
SEG_SAME = 0.9             # share of matching windows for same_arrangement
SEG_DIFF = 0.5             # under this share: different_arrangement
DRIFT_SAME_MS = 5.0
DRIFT_DIFF_MS = 40.0
COVER_SAME = 0.9
IDENTICAL_DB = -60.0       # raw residual under this: the same audio (a renamed or retagged copy)
MIN_DUR_S = 45.0           # a shorter file is a clip, not a bounce
CACHE = Path.home() / '.cache/manifold/detector-lab/catalog'


def verdict(m):
    """same_arrangement | aligned_needs_check | different_arrangement from the metrics of compare()."""
    if m['lag_z'] < Z_MIN or m['seg_frac'] < SEG_DIFF or m['drift_ms'] > DRIFT_DIFF_MS:
        return 'different_arrangement'
    if m['seg_frac'] >= SEG_SAME and m['drift_ms'] <= DRIFT_SAME_MS and m['cover'] >= COVER_SAME:
        return 'same_arrangement'
    return 'aligned_needs_check'


def _shift_view(x, k, a, b):
    """x delayed by k samples, over [a, b), zero outside x."""
    out = np.zeros(b - a, dtype=x.dtype)
    lo, hi = max(a - k, 0), min(b - k, len(x))
    if hi > lo:
        out[lo + k - a:hi + k - a] = x[lo:hi]
    return out


def refine(ref, x, k0, sr, span=96, win_s=8):
    """Sample-level lag near k0 on the loudest win_s of ref (remix.refine_lag, without copying whole files per try)."""
    w = int(win_s * sr)
    n = len(ref)
    if n <= w:
        a, b = 0, n
    else:
        hop = sr
        e = np.array([np.dot(ref[i:i + w], ref[i:i + w]) for i in range(0, n - w, hop)])
        a = int(np.argmax(e)) * hop
        b = a + w
    seg = ref[a:b].astype(np.float64)
    best, bk = -np.inf, k0
    for k in range(k0 - span, k0 + span + 1):
        v = float(np.dot(seg, _shift_view(x, k, a, b)))
        if v > best:
            best, bk = v, k
    # a peak on the edge of the search is a phase-rotated bass line (a mastering EQ), not a lag: keep the onset lag
    return k0 if abs(bk - k0) >= span else bk


def rises(x, sr):
    """Positive rises of the 100-8000 Hz band energy on a 1 ms grid (what detector_labels.stem_lag correlates)."""
    from tools.audio_analysis.eval.detector_labels import env_db
    e = env_db(x, sr, *BAND)
    return np.maximum(np.diff(e, prepend=e[0]), 0.0)


def smooth(r):
    from scipy.ndimage import uniform_filter1d
    return uniform_filter1d(r, 5)


def attack_env(x, sr):
    """The rises smoothed over 5 ms."""
    return smooth(rises(x, sr))


def _ncc(a, b):
    a = a - a.mean()
    b = b - b.mean()
    d = float(np.sqrt(np.dot(a, a) * np.dot(b, b)))
    return float(np.dot(a, b) / d) if d > 1e-12 else 0.0


def _local_lag_ms(ra, xa, lo, hi, search=300):
    """Lag (ms) of xa against ra in [lo, hi) ms within +-search, and the correlation there."""
    r = ra[lo:hi] - ra[lo:hi].mean()
    best, bl = -1.0, 0
    for lag in range(-search, search + 1, 2):
        a, b = lo - lag, hi - lag
        if a < 0 or b > len(xa):
            continue
        v = _ncc(r, xa[a:b])
        if v > best:
            best, bl = v, lag
    return bl, best


def lag_peaks(ra, xa, search_s=20.0, top=PEAKS):
    """The strongest lags (ms) of xa against ra from the cross-correlation of the two onset envelopes within
    +-search_s, as [(lag_ms, z)], z the peak over the other lags (detector_labels.stem_lag, but keeping several peaks:
    a bounce with a different count-in has a weak best peak that is not its timeline)."""
    n = 1 << int(np.ceil(np.log2(len(ra) + len(xa))))
    c = np.fft.irfft(np.fft.rfft(ra, n) * np.conj(np.fft.rfft(xa, n)), n)
    k = int(search_s * 1e3)
    lags = np.r_[np.arange(0, k), np.arange(-k, 0)]
    vals = np.r_[c[:k], c[-k:]]
    z = (vals - vals.mean()) / (vals.std() + 1e-12)
    out = []
    for _ in range(top):
        i = int(np.argmax(z))
        if z[i] <= 0:
            break
        out.append((int(lags[i]), float(z[i])))
        z = np.where(np.abs(lags - lags[i]) < PEAK_GAP_MS, -np.inf, z)
    return out


def _shift_env(xa, d, n):
    """xa delayed by d ms, over [0, n), zero outside."""
    return _shift_view(xa, d, 0, n)


def _win_pow(x, k, i, w):
    a, b = max(i - k, 0), min(i - k + w, len(x))
    return float(np.dot(x[a:b], x[a:b])) / w if b > a else 0.0


def segments(ref, x, ra, xa, k, sr):
    """(windows where both files sound, windows whose onset pattern matches) with x delayed by k samples."""
    n, w, hop_ms = len(ref), int(SEG_S * sr), int(SEG_S * 1000)
    starts = list(range(0, max(n - w, 1), w))
    rp = [float(np.dot(ref[i:i + w], ref[i:i + w])) / w for i in starts]
    floor = max(float(np.median(rp)) * 1e-3, 1e-12)
    xs = _shift_env(xa, int(round(k * 1000 / sr)), len(ra))
    ok = matched = 0
    for j, i in enumerate(starts):
        if rp[j] < floor or _win_pow(x, k, i, w) < floor:
            continue
        ok += 1
        # the labels are drum onsets: a window matches when its onset pattern does (a held bass or pad correlating
        # across two different drum patterns must not count)
        lo, hi = j * hop_ms, (j + 1) * hop_ms
        ca = max(_ncc(ra[lo:hi], xs[lo + d:hi + d]) for d in range(-SLACK_MS, SLACK_MS + 1) if lo + d >= 0 and hi + d <= len(xs))
        matched += ca >= MATCH_ATTACK
    return ok, matched, xs


def compare(ref, x, sr):
    """Metrics of x (a candidate) against ref on ref's timeline. Both mono float at sr. The best of the strongest onset
    lags wins (most matching windows, then the strongest peak)."""
    n = len(ref)
    rr, xr = rises(ref, sr), rises(x, sr)
    ra, xa0 = smooth(rr), smooth(xr)
    best = None
    for L, z in lag_peaks(rr, xr):
        k = int(round(L * sr / 1000))
        ok, matched, xs_env = segments(ref, x, ra, xa0, k, sr)
        key = (matched / ok if ok else 0.0, z)
        if best is None or key > best[0]:
            best = (key, L, z, k, ok, matched, xs_env)
    if best is None:
        raise ValueError('no onset lag (silent or too short)')
    _, L, z, k0, ok, matched, xa = best
    k = refine(ref, x, k0, sr)
    xs = _shift_view(x, k, 0, n)
    rx, xx, pm = float(np.dot(ref, xs)), float(np.dot(xs, xs)), float(np.dot(ref, ref))
    g = rx / max(xx, 1e-30)
    # |ref - a*xs|^2 / |ref|^2 in dB from the three dot products: no whole-file temporaries
    res = lambda a: float(10 * np.log10(max(pm - 2 * a * rx + a * a * xx, 1e-30 * pm) / max(pm, 1e-30)))  # noqa: E731
    mr, mx = float(ref.mean()), float(xs.mean())
    cov, vr, vx = rx - n * mr * mx, pm - n * mr * mr, xx - n * mx * mx
    corr = float(cov / np.sqrt(vr * vx)) if vr > 0 and vx > 0 else 0.0
    drifts = []
    total_ms = min(len(ra), len(xa))
    for frac in (.2, .4, .6, .8):
        lo = int(frac * total_ms)
        hi = min(lo + 20000, total_ms - 400)
        if hi - lo < 4000 or lo < 400:
            continue
        lag, c = _local_lag_ms(ra, xa, lo, hi)
        if c >= .3:
            drifts.append(abs(lag))
    over = max(0, min(n, k + len(x)) - max(0, k))
    m = dict(lag_s=k / sr, lag_samples=int(k), lag_z=round(float(z), 1), corr=round(corr, 5), residual_db=round(res(g), 2),
             raw_residual_db=round(res(1.0), 2), gain_db=round(float(20 * np.log10(abs(g) + 1e-30)), 2),
             seg_frac=round(matched / ok, 3) if ok else 0.0, windows=ok, drift_ms=float(max(drifts)) if drifts else 999.0,
             dur_s=round(len(x) / sr, 2), ref_dur_s=round(n / sr, 2), cover=round(over / n, 3))
    m['identical'] = m['raw_residual_db'] < IDENTICAL_DB
    m['verdict'] = verdict(m)
    return m


def key_of(*parts):
    return hashlib.sha256(json.dumps([VERSION, *parts], default=str).encode()).hexdigest()[:24]


def read_mono(path, sr):
    from tools.audio_analysis.lab import remix
    return remix.read_mono(path, sr).astype(np.float32).astype(np.float64)


def candidates(con, song_id):
    """Local full mixes, masters and premasters in the song's composition (not its lab mix, not references), as
    [(path, role, version, sha256, duration_s)]."""
    row = con.execute('SELECT lab_mix, group_override, group_id FROM songs WHERE id=?', (song_id,)).fetchone()
    comp = row[1] or row[2]
    lab = json.loads(row[0] or 'null') or {}
    lab_files = {lab.get('file')} | set(lab.get('parts') or [])
    out = []
    for p, role, ver, sha, dur, local in con.execute(
            "SELECT a.path, a.role, a.version, a.sha256, a.duration_s, a.local FROM comp_assets ca JOIN assets a USING(path) "
            "WHERE ca.comp_id=? AND a.role IN ('full_mix','master','premaster') AND a.local=1 AND ca.flags NOT LIKE '%reference%' "
            "ORDER BY a.path", (comp,)):
        if p in lab_files or (dur or 0) < MIN_DUR_S:
            continue
        out.append((p, role, ver, sha, dur))
    return out


def lab_variants(con, songs, ids, force=False):
    """Compare every candidate of the given lab songs against the lab mix; store in variants."""
    from tools.audio_analysis.catalog import catalog
    from tools.audio_analysis.lab import registry
    for sid in ids:
        s = songs[sid]
        todo = []
        for p, role, ver, sha, dur in candidates(con, sid):
            k = key_of(s.content_id, sha)
            have = con.execute("SELECT key FROM variants WHERE song_id=? AND ref='lab_mix' AND path=?", (sid, p)).fetchone()
            if force or not have or have[0] != k:
                todo.append((p, k))
        if not todo:
            continue
        print(f'{sid}: {len(todo)} candidates', flush=True)
        ref = registry.mix(s)
        for p, k in todo:
            try:
                m = compare(ref, read_mono(p, s.sample_rate), s.sample_rate)
            except Exception as e:  # an unreadable or silent file is recorded, not fatal
                m = dict(lag_s=None, lag_samples=None, lag_z=None, corr=None, residual_db=None, raw_residual_db=None, gain_db=None,
                         seg_frac=None, windows=0, drift_ms=None, dur_s=None, ref_dur_s=None, cover=None,
                         verdict='unreadable', error=f'{type(e).__name__}: {e}'[:120])
            print(f"  {Path(p).name[:70]:70s} {m['verdict']:22s} lag {m['lag_s'] if m['lag_s'] is None else round(m['lag_s'], 4)}  "
                  f"z {m['lag_z']}  seg {m['seg_frac']}  drift {m['drift_ms']}  res {m['residual_db']}", flush=True)
            detail = {x: m[x] for x in ('windows', 'cover', 'gain_db', 'error', 'identical') if x in m}
            with con:
                catalog.upsert(con, 'variants', ('song_id', 'ref', 'path'), dict(
                    song_id=sid, ref='lab_mix', path=p, lag_s=m['lag_s'], lag_z=m['lag_z'], corr=m['corr'],
                    residual_db=m['residual_db'], raw_residual_db=m['raw_residual_db'], seg_frac=m['seg_frac'],
                    drift_ms=m['drift_ms'], dur_s=m['dur_s'], ref_dur_s=m['ref_dur_s'], verdict=m['verdict'],
                    detail=catalog.j(detail), key=k))
        del ref


MAX_DUR_S = 900.0          # a 15+ minute file is a set recording, not a song bounce
SUMS_DB = -20.0            # residual at the best gain under this: the stems sum to the bounce (remix.FLAG_DB)


def stem_groups(con, comp):
    """The comp's stem sets as [(label, [paths], sample rate)]: a lab song's catalogue stem set, else each folder holding
    2+ local stem files of one length."""
    from tools.audio_analysis.catalog import usable
    out = []
    sid = con.execute("SELECT id, sample_rate, stem_set FROM songs WHERE COALESCE(group_override, group_id)=? AND kind='project' "
                      "AND stem_set IS NOT NULL", (comp,)).fetchall()
    for s, sr, ss in sid:
        ps = (json.loads(ss) or {}).get('paths') or []
        if len(ps) >= 2:
            out.append((f'catalogue:{s}', ps, sr))
    for folder, files in usable.stem_folders(con, comp):
        local = [f for f in files if f[3] and f[2]]
        if len(local) < 2:
            continue
        by = {}
        for p, role, dur, _ in local:
            by.setdefault(round(dur), []).append(p)
        group = max(by.values(), key=len)
        if len(group) >= 2 and sorted(group) not in [sorted(g[1]) for g in out]:
            out.append((f'folder:{os.path.basename(folder)}', group, 48000))
    return out


def stem_sum(paths, sr, key):
    """The plain sum of the stems, mono float64 at sr, streamed one file at a time; cached as float32."""
    CACHE.mkdir(parents=True, exist_ok=True)
    f = CACHE / f'stemsum_{key}.npy'
    if f.exists():
        return np.load(f).astype(np.float64)
    total = None
    for p in paths:
        x = read_mono(p, sr)
        if total is None:
            total = np.zeros(len(x))
        if len(x) > len(total):
            total = np.concatenate([total, np.zeros(len(x) - len(total))])
        total[:len(x)] += x
    np.save(f, total.astype(np.float32))
    return total


def stem_variants(con, comps, force=False):
    """Does each stem set sum to one of the composition's bounces? Stored in variants as ref='stem_sum:<set>'."""
    from tools.audio_analysis.catalog import catalog
    for comp in comps:
        bounces = [r for r in con.execute(
            "SELECT a.path, a.sha256, a.duration_s FROM comp_assets ca JOIN assets a USING(path) WHERE ca.comp_id=? AND a.local=1 "
            "AND a.role IN ('full_mix','master','premaster') AND ca.flags NOT LIKE '%reference%' AND a.duration_s BETWEEN ? AND ? "
            "ORDER BY a.path", (comp, MIN_DUR_S, MAX_DUR_S))]
        for label, paths, sr in stem_groups(con, comp):
            shas = [catalog.stem_sha(p) for p in paths]
            skey = key_of(shas, sr)
            x = None
            dur = None
            for p, sha, bdur in bounces:
                k = key_of(skey, sha)
                have = con.execute('SELECT key FROM variants WHERE song_id=? AND ref=? AND path=?', (comp, 'stem_sum:' + label, p)).fetchone()
                if have and have[0] == k and not force:
                    continue
                if x is None:
                    x = stem_sum(paths, sr, skey)
                    dur = len(x) / sr
                if abs(bdur - dur) > .12 * dur:
                    continue                      # a bounce of another length is not this stem set's sum
                try:
                    m = compare(read_mono(p, sr), x, sr)
                except Exception as e:  # unreadable or silent audio is recorded, not fatal
                    print(f'  {comp} {os.path.basename(p)}: {type(e).__name__}: {e}'[:160])
                    continue
                m['sums'] = m['residual_db'] <= SUMS_DB and m['verdict'] != 'different_arrangement'
                print(f"  {comp:20s} {label[:28]:28s} {os.path.basename(p)[:50]:50s} {m['verdict']:21s} res {m['residual_db']} corr {m['corr']} "
                      f"seg {m['seg_frac']} lag {round(m['lag_s'], 4)}  sums={m['sums']}", flush=True)
                with con:
                    catalog.upsert(con, 'variants', ('song_id', 'ref', 'path'), dict(
                        song_id=comp, ref='stem_sum:' + label, path=p, lag_s=m['lag_s'], lag_z=m['lag_z'], corr=m['corr'],
                        residual_db=m['residual_db'], raw_residual_db=m['raw_residual_db'], seg_frac=m['seg_frac'],
                        drift_ms=m['drift_ms'], dur_s=m['dur_s'], ref_dur_s=m['ref_dur_s'], verdict=m['verdict'],
                        detail=catalog.j({'sums': m['sums'], 'cover': m['cover'], 'windows': m['windows'], 'gain_db': m['gain_db'], 'identical': m['identical']}),
                        key=k))
            del x


def run(con, cmd, wanted, force=False):
    """catalog.py variants / stemsum."""
    from tools.audio_analysis.lab import registry
    if cmd == 'variants':
        songs = registry.load()
        ids = [t for t, s in songs.items() if s.kind == 'project' and (not wanted or t in wanted)]
        ids = [t for t in ids if con.execute('SELECT licence FROM songs WHERE id=?', (t,)).fetchone()]
        lab_variants(con, songs, ids, force)
    else:
        comps = [r[0] for r in con.execute("SELECT id FROM comps WHERE kind IN ('lab','original','remix') ORDER BY id")
                 if not wanted or r[0] in wanted]
        stem_variants(con, comps, force)
