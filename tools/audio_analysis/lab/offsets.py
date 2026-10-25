#!/usr/bin/env python3
"""Export-offset proofs for project songs, by detector_wip's method on the song's
own mix: the project's kick notes against the mix's low onset strength, its snare notes against the 1-8 kHz
rises, each fitted to a whole bar's time on the set's tempo map (als_extract's integration, so tempo automation fits
too) plus a lag. Proven when both patterns pick the same bar, or when only one pattern
exists and it wins clearly (r >= 0.3, margin >= 1 sd). A proof counts only when its offset equals the one the labels
use (within 1 ms). A WIP song (detector_wip) is checked against its recorded set and offset, on the audio the lab
reads. Writes lab/offset_proofs.json; registry.py reads it.

--variants proves the mix and master variants the catalogue judged same_arrangement against a song's lab mix
(catalog.py variants): each by the same fit on the variant's own audio, against the offset its labels would use (the
song's offset less the catalogue's lag); failing that, relatively: one pattern fitted on both the song's mix and the
variant moves by the catalogue's lag within VARIANT_TOL_S, so the song's proven offset carries over. Variants train and
are never scored, and the catalogue's whole-file lag is the alignment; the pattern only confirms it, and mastering
moves a fitted kick attack by up to 10 ms. Any audio the catalogue reads (mp3, aiff, flac) is proven. Identical audio and
byte-identical copies of the lab mix or of another variant are listed, not proven. Writes lab/variant_proofs.json, which
sampling.mix_variants reads (registry.variants_of).

prove_seconds is the same fit plus the kick attack timing: detector_wip's proof of a new song's set and bounce.

Usage: offsets.py SONG...
       offsets.py --variants [SONG...]"""
import json
from functools import cached_property
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT))
from tools.audio_analysis.lab import recipe  # noqa: E402

recipe.apply()

import numpy as np  # noqa: E402

PROOFS = Path(__file__).with_name('offset_proofs.json')
CLEAR_R, CLEAR_SD = .3, 1.0
VARIANT_TOL_S = .010   # how far mastering legitimately moves a fitted kick attack; variants only, never the exam's 1 ms
# Attack timing, generated_sets.timing_proof's rule on the mix (the evidence behind Peter's got_it, renew and
# overwhelming_force rulings): at kick notes with no other kick in the 200 ms before, the lag from the note's export
# time to where the mix's kick_env_db first clears its level 15-40 ms earlier by 10 dB, searched -15..+25 ms. On time
# when at least 16 attacks are found and their median is within 8 ms (the ruled songs: -6, -2, 0 ms); off when at
# least 16 are found and the median is further out (a pairing 30 ms early piles them at +16..+24 ms). Dense mixes
# often find fewer than 16: not measured, which leaves the pattern proof standing.
ATTACK_MIN, ATTACK_MAX_MS = 16, 8


def fits(song, als):
    """(kick fit, snare fit, the set's beat clock) of the song's mix against its set's notes, on any tempo map. A fit
    is None when the set has no such notes."""
    from scipy.signal import resample_poly
    from tools.audio_analysis.eval.als_extract import extract
    from tools.audio_analysis.eval.detector_wip import kick_beats, snare_beats
    from tools.audio_analysis.lab.registry import mix
    res = extract(als)
    x = mix(song)
    if song.sample_rate != 48000:
        x = resample_poly(x, 48000, song.sample_rate)
    envs, clock = MixEnvs(x), beat_clock(res['tempo_points'])
    return fit_map(envs.kick, kick_beats(res), envs.dur, clock), fit_map(envs.snare, snare_beats(res), envs.dur, clock), clock


def bar_start(clock, k):
    """Seconds at the start of bar k on the set's tempo map; on one tempo exactly detector_wip's 4 * k * spb."""
    return float(clock(np.array([4.0 * k]))[0])


def pattern_offset(p, clock, is_kick):
    """The export offset a fit implies: its bar's start on the map, less its lag (and the kick envelope's bias)."""
    from tools.audio_analysis.eval.kick_goal_wip_sections import ENV_BIAS
    return -bar_start(clock, p['k']) + p['lag'] - (ENV_BIAS if is_kick else 0.0)


def fit_offsets(kick, snare, clock):
    """{pattern: the export offset its fit implies}."""
    return {name: pattern_offset(p, clock, name == 'kick') for name, p in (('kick', kick), ('snare', snare)) if p}


def prove(song, als, used, got=False):
    """got: fits(song, als) when already computed."""
    got = fits(song, als) if got is False else got
    kick, snare, clock = got
    if kick and snare and kick['k'] == snare['k']:
        status, use = 'proven (kick and snare agree)', kick
    elif len([p for p in (kick, snare) if p]) == 1 and any(p and p['r'] >= CLEAR_R and p['margin_sd'] >= CLEAR_SD for p in (kick, snare)):
        status, use = 'proven (one pattern, clear win)', kick or snare
    else:
        return dict(status='not proven', kick=kick, snare=snare)
    off = pattern_offset(use, clock, use is kick)
    if abs(off - used) > .001:
        return dict(status=f'not proven (fit {off:.4f} s, labels use {used:.4f} s)', kick=kick, snare=snare)
    return dict(status=status, offset_s=round(off, 4), kick=kick, snare=snare)


def beat_clock(tempo_points):
    """Seconds at arrangement beats on the set's tempo map (als_extract's integration). One tempo keeps
    detector_wip's arithmetic exactly, so a constant-tempo song fits as before to the bit."""
    import numpy as np
    from tools.audio_analysis.eval.als_extract import beat_seconds
    pts = [(p['beat'], p['bpm']) for p in tempo_points]
    if len(pts) == 1:
        spb = 60 / pts[0][1]
        return lambda b: np.asarray(b, float) * spb
    return lambda b: np.array([beat_seconds(pts, float(v)) for v in np.atleast_1d(b)])


def fit_map(env, beats, dur, clock):
    """The grid fit (kick survey's wip_sections method) on any tempo map: export start = a whole bar's time on the map
    plus one lag (+-80 ms), chosen by the Pearson r between the note pattern on the 16th grid and the onset strength at
    every grid point. {k: the bar, r, margin_sd: r over the runner-up in sd of every bar's r, lag}; None under 16
    notes or fewer than two candidate bars. On one tempo it is the bar-grid fit number for number (test_lab)."""
    import numpy as np
    from scipy.ndimage import maximum_filter1d
    from tools.audio_analysis.eval.kick_goal_wip_sections import LAGS, vals
    if len(beats) < 16:
        return None
    wide = maximum_filter1d(env, 41)
    pat_all = set(np.round(beats * 4).astype(int).tolist())
    kmax = int(beats.max() / 4) + 2
    lag_ms = np.round(LAGS * 1000).astype(int)
    q = np.arange(16 * kmax + 64)
    sec = clock(q / 4)
    while sec[-1] < sec[16 * kmax] + dur:
        q = np.arange(len(q) * 2)
        sec = clock(q / 4)
    best = {}
    for k in range(0, kmax + 1):
        g = q[16 * k:]
        t = sec[16 * k:] - sec[16 * k]
        m = (t >= .6) & (t <= dur - .6)
        g, t = g[m], t[m]
        p = np.array([1.0 if v in pat_all else 0.0 for v in g])
        if p.sum() < 8:
            continue
        v = wide[np.clip(np.round(t * 1000).astype(int)[None, :] + lag_ms[:, None], 0, len(wide) - 1)]
        v = v - v.mean(axis=1, keepdims=True)
        pc = p - p.mean()
        r = (v @ pc) / (np.linalg.norm(v, axis=1) * np.linalg.norm(pc) + 1e-12)
        best[k] = float(r[int(np.argmax(r))])
    order = sorted(best, key=lambda k: -best[k])
    if len(order) < 2:
        return None
    k0 = order[0]
    rs = np.array([best[k] for k in order])
    t = clock(beats) - sec[16 * k0]
    t = t[(t >= .6) & (t <= dur - .6)]
    lag = float(LAGS[int(np.argmax([vals(env, t + lg).mean() for lg in LAGS]))])
    return dict(k=int(k0), r=round(best[k0], 3), margin_sd=round(float((best[k0] - best[order[1]]) / (rs.std() + 1e-9)), 2),
                lag=lag)


def attack_lags(env_db, times):
    """Attack lag (ms) at each isolated time (no other time in the 200 ms before) on a 1 ms dB envelope; NaN where
    no 10 dB rise sits in -15..+25 ms. Returns (lags, isolated count)."""
    import numpy as np
    times = np.sort(np.asarray(times, float))
    iso = times[np.concatenate([[True], np.diff(times) > .2])] if len(times) else times
    out = []
    for s in iso:
        i = int(round(s * 1000))
        if i < 45 or i + 30 >= len(env_db):
            continue
        up = np.flatnonzero(env_db[i - 15:i + 25] >= np.median(env_db[i - 40:i - 15]) + 10)
        out.append(float(up[0] - 15) if len(up) else np.nan)
    return np.array(out), len(out)


def attack_check(env_db, times):
    import numpy as np
    lags, n = attack_lags(env_db, times)
    v = lags[~np.isnan(lags)]
    if len(v) < ATTACK_MIN:
        return dict(isolated=n, hits=len(v), verdict='not measured')
    med = float(np.median(v))
    return dict(isolated=n, hits=len(v), median_ms=med, p10_ms=float(np.percentile(v, 10)),
                p90_ms=float(np.percentile(v, 90)), verdict='on time' if abs(med) <= ATTACK_MAX_MS else 'off')


class MixEnvs:
    """A 48 kHz mono mix's envelopes for the proof, each built once on first use (one mix, many sets)."""

    def __init__(self, x):
        self.x, self.dur = x, len(x) / 48000

    @cached_property
    def kick(self):
        from tools.audio_analysis.eval.kick_goal_wip_sections import onset_env
        return onset_env(self.x)

    @cached_property
    def snare(self):
        from scipy.ndimage import maximum_filter1d
        from tools.audio_analysis.eval.kick_goal_wip_sections import band_amp_rise
        return 100 * maximum_filter1d(band_amp_rise(self.x, 1000, 8000), 7)

    @cached_property
    def attack(self):
        from tools.audio_analysis.eval.kick_goal_labels import kick_env_db
        return kick_env_db(self.x, 48000)


def attacks_at(envs, res, kick_b, off):
    """Attack timing of an offset on the kick notes."""
    envs = envs if isinstance(envs, MixEnvs) else MixEnvs(envs)
    return attack_check(envs.attack, beat_clock(res['tempo_points'])(kick_b) + off)


def prove_seconds(res, x, kick_b=None, snare_b=None):
    """Export offset of a 48 kHz mono mix (an array or its MixEnvs) against its set, on any tempo map: export time =
    note seconds + offset_s. The pattern rule is detector_wip's (kick and snare pick the same bar, or the only
    pattern wins clearly); the kick attack timing must then not be off. A failed pattern rule still reports the
    attack timing at the best pattern's bar (top_offset_s), as evidence for Peter, never as a proof."""
    from tools.audio_analysis.eval.detector_wip import kick_beats, snare_beats
    x = x if isinstance(x, MixEnvs) else MixEnvs(x)
    kick_b = kick_beats(res) if kick_b is None else kick_b
    snare_b = snare_beats(res) if snare_b is None else snare_b
    clock = beat_clock(res['tempo_points'])
    kick = fit_map(x.kick, kick_b, x.dur, clock)
    snare = fit_map(x.snare, snare_b, x.dur, clock)
    row = dict(kick=kick, snare=snare, tempo_points=len(res['tempo_points']))
    if kick and snare and kick['k'] == snare['k']:
        status, use = 'kick and snare agree', kick
    elif len([p for p in (kick, snare) if p]) == 1 and any(p and p['r'] >= CLEAR_R and p['margin_sd'] >= CLEAR_SD for p in (kick, snare)):
        status, use = 'one pattern, clear win', kick or snare
    else:
        top = kick or snare
        if top:
            off = pattern_offset(top, clock, top is kick)
            row.update(top_offset_s=round(off, 4), attack=attacks_at(x, res, kick_b, off))
        return dict(row, status='not proven (patterns)')
    off = pattern_offset(use, clock, use is kick)
    att = attacks_at(x, res, kick_b, off)
    row.update(offset_s=round(off, 4), attack=att)
    if att['verdict'] == 'off':
        return dict(row, status=f'not proven ({status}; attacks off)')
    return dict(row, status=f"proven ({status}; attacks {att['verdict']})")


def catalogue_variants(con, song_id):
    """[(path, sha256, lag_s, identical)] of the song's variants the catalogue judged same_arrangement against its lab
    mix; aligned_needs_check and different_arrangement never transfer labels, however close."""
    import json as _json
    rows = con.execute("SELECT v.path, a.sha256, v.lag_s, v.detail FROM variants v LEFT JOIN assets a USING(path) "
                       "WHERE v.song_id=? AND v.ref='lab_mix' AND v.verdict='same_arrangement' ORDER BY v.path",
                       (song_id,)).fetchall()
    return [(p, sha, lag, bool(_json.loads(d or '{}').get('identical'))) for p, sha, lag, d in rows]


def moves(parent_fits, variant_fits, lag_s):
    """{pattern: seconds its fit misses moving by the catalogue's lag} for every pattern fitted on both the song's mix
    and the variant. The fit's bias on a mix is the song's, so it cancels."""
    if parent_fits is None or variant_fits is None:
        return {}
    a, b = fit_offsets(*parent_fits), fit_offsets(*variant_fits)
    return {k: (b[k] - a[k]) + lag_s for k in sorted(set(a) & set(b))}


def relative(moved):
    """The relative proof from moves(): (pattern, residual s) of the pattern nearest the lag when it is within
    VARIANT_TOL_S, else None. The song's own offset is proven apart (its audit)."""
    if not moved:
        return None
    k = min(moved, key=lambda p: (abs(moved[p]), p))
    return (k, moved[k]) if abs(moved[k]) <= VARIANT_TOL_S else None


def prove_variant(song, path, sha, lag_s, als, seen, parent_fits=None):
    """One proof row for a variant of song. seen: {decoded audio sha256: variant id} of the song's audio so far;
    parent_fits: fits(song, als). Proven absolutely (prove: the fit lands on the offset its labels use) or relatively
    (relative: the fit moves with the catalogue's lag)."""
    import hashlib
    from tools.audio_analysis.lab.registry import mix, sha256_file, variant_copy, variant_id
    vid = variant_id(song.id, sha)
    row = dict(parent=song.id, path=path, sha256=sha, lag_s=lag_s, als=str(als) if als else None)
    if sha in song.source_sha256:
        return vid, dict(row, status='excluded (a byte-identical copy of the lab mix)')
    if song.offset_s is None or als is None:
        return vid, dict(row, status='not proven (the song has no export offset or Ableton set)')
    from tools.audio_analysis.catalog.catalog import is_dataless
    if is_dataless(path):
        return vid, dict(row, status='not proven (online-only file: never downloaded by a proof)')
    if sha256_file(path) != sha:
        return vid, dict(row, status='not proven (the file changed since the catalogue hashed it)')
    used = song.offset_s - lag_s
    v = variant_copy(song, dict(path=path, sha256=sha, offset_s=used, status='pending'))
    try:
        got = fits(v, als)
        res = prove(v, als, used, got)
        moved = moves(parent_fits, got, lag_s)
        rel = None if res['status'].startswith('proven') else relative(moved)
        if rel:
            res = dict(res, status=f'proven (relative: the {rel[0]} fit moves with the catalogue lag, '
                                   f'{1e3 * rel[1]:+.2f} ms)')
        x = mix(v)
    except Exception as e:  # an unreadable variant is listed, never fatal
        return vid, dict(row, status=f'not proven (unreadable: {type(e).__name__}: {e})'[:200])
    audio = hashlib.sha256(np.ascontiguousarray(x).tobytes()).hexdigest()
    row = dict(row, audio_sha256=audio, duration_s=round(len(x) / v.sample_rate, 4), fit=res.get('offset_s'),
               moves_ms={k: round(1e3 * r, 3) for k, r in moved.items()}, status=res['status'])
    if audio in seen:
        return vid, dict(row, status=f'excluded (same audio as {seen[audio]})')
    seen[audio] = vid
    if rel:
        row['relative'] = dict(pattern=rel[0], residual_ms=round(1e3 * rel[1], 3))
    return vid, dict(row, offset_s=used) if res['status'].startswith('proven') else row


def variants(ids):
    """Proves every same_arrangement variant of the given project songs (all when none) into VARIANT_PROOFS."""
    import hashlib
    from tools.audio_analysis.catalog.catalog import connect
    from tools.audio_analysis.eval.detector_songs import wip
    from tools.audio_analysis.eval.kick_goal_melodic import _source
    from tools.audio_analysis.lab.registry import VARIANT_PROOFS, load, mix, variant_id
    songs = load()
    W = wip()
    con = connect(readonly=True)
    proofs = json.loads(VARIANT_PROOFS.read_text()) if VARIANT_PROOFS.exists() else {}
    for t, s in songs.items():
        if s.kind != 'project' or (ids and t not in ids):
            continue
        found = catalogue_variants(con, t)
        proofs = {k: v for k, v in proofs.items() if v['parent'] != t}
        if not found:
            continue
        als = W[t]['als'] if t in W else (_source(t) or (None,))[0]
        seen = {hashlib.sha256(np.ascontiguousarray(mix(s)).tobytes()).hexdigest(): f'{t} (the lab mix)'}
        parent_fits = fits(s, als) if als else None
        for path, sha, lag, identical in found:
            if variant_id(t, sha) in proofs:
                continue   # a byte-identical copy of a file already judged: one variant, its first path
            if identical:
                proofs[variant_id(t, sha)] = dict(parent=t, path=path, sha256=sha, lag_s=lag,
                                                  status='excluded (identical audio to the lab mix)')
                continue
            vid, row = prove_variant(s, path, sha, lag, als, seen, parent_fits)
            proofs[vid] = row
            print(f"{vid:28s} {row['status']}", flush=True)
        n = sum(1 for v in proofs.values() if v['parent'] == t and v['status'].startswith('proven'))
        print(f'{t}: {n} of {len(found)} same_arrangement variants proven', flush=True)
    VARIANT_PROOFS.write_text(json.dumps(proofs, indent=1, sort_keys=True) + '\n')


def source(t):
    """(set, export offset the labels use) for a goal song with a project or a WIP song."""
    from tools.audio_analysis.eval.detector_songs import wip
    from tools.audio_analysis.eval.kick_goal_melodic import _source
    w = wip().get(t)
    return _source(t) or (w and (w['als'], w['offset_s']))


def main():
    from tools.audio_analysis.lab.registry import load
    if len(sys.argv) < 2:
        sys.exit(__doc__)
    if sys.argv[1] == '--variants':
        return variants(set(sys.argv[2:]))
    songs = load()
    proofs = json.loads(PROOFS.read_text()) if PROOFS.exists() else {}
    for t in sys.argv[1:]:
        als, used = source(t)
        proofs[t] = dict(prove(songs[t], als, used), als=str(als))
        print(f"{t:18s} {proofs[t]['status']}")
    PROOFS.write_text(json.dumps(proofs, indent=1, sort_keys=True) + '\n')


if __name__ == '__main__':
    main()
