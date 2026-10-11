#!/usr/bin/env python3
"""Export-offset proofs for project songs (docs/DETECTOR_LAB_DESIGN.md D5), by detector_wip's method on the song's
own mix: the project's kick notes against the mix's low onset strength, its snare notes against the 1-8 kHz
rises, each fitted to whole bars plus a lag. Proven when both patterns pick the same bar, or when only one pattern
exists and it wins clearly (r >= 0.3, margin >= 1 sd). A proof counts only when its offset equals the one the labels
use (within 1 ms). Writes lab/offset_proofs.json; registry.py reads it.

prove_seconds is the same fit on any tempo map (note times through als_extract's tempo integration), plus the kick
attack timing; it proves a mix against a set, for songs not yet in the lab.

Usage: offsets.py SONG..."""
import json
from functools import cached_property
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT))
from tools.audio_analysis.lab import recipe  # noqa: E402

recipe.apply()

PROOFS = Path(__file__).with_name('offset_proofs.json')
CLEAR_R, CLEAR_SD = .3, 1.0
# Attack timing, generated_sets.timing_proof's rule on the mix (the evidence behind Peter's got_it, renew and
# overwhelming_force rulings): at kick notes with no other kick in the 200 ms before, the lag from the note's export
# time to where the mix's kick_env_db first clears its level 15-40 ms earlier by 10 dB, searched -15..+25 ms. On time
# when at least 16 attacks are found and their median is within 8 ms (the ruled songs: -6, -2, 0 ms); off when at
# least 16 are found and the median is further out (a pairing 30 ms early piles them at +16..+24 ms). Dense mixes
# often find fewer than 16: not measured, which leaves the pattern proof standing.
ATTACK_MIN, ATTACK_MAX_MS = 16, 8


def prove(song, als, used):
    import numpy as np  # noqa: F401
    from scipy.ndimage import maximum_filter1d
    from scipy.signal import resample_poly
    from tools.audio_analysis.eval.als_extract import extract
    from tools.audio_analysis.eval.detector_wip import kick_beats, prove as fit, snare_beats
    from tools.audio_analysis.eval.kick_goal_wip_sections import ENV_BIAS, band_amp_rise, onset_env
    from tools.audio_analysis.lab.registry import mix
    res = extract(als)
    if len(res['tempo_points']) != 1:
        return dict(status='not proven (tempo automation)')
    x = mix(song)
    if song.sample_rate != 48000:
        x = resample_poly(x, 48000, song.sample_rate)
    spb, dur = 60 / res['tempo_points'][0]['bpm'], len(x) / 48000
    kick = fit(onset_env(x), kick_beats(res), dur, spb)
    snare = fit(100 * maximum_filter1d(band_amp_rise(x, 1000, 8000), 7), snare_beats(res), dur, spb)
    if kick and snare and kick['k'] == snare['k']:
        status, use = 'proven (kick and snare agree)', kick
    elif len([p for p in (kick, snare) if p]) == 1 and any(p and p['r'] >= CLEAR_R and p['margin_sd'] >= CLEAR_SD for p in (kick, snare)):
        status, use = 'proven (one pattern, clear win)', kick or snare
    else:
        return dict(status='not proven', kick=kick, snare=snare)
    off = -4 * use['k'] * spb + use['lag'] - (ENV_BIAS if use is kick else 0.0)
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
    """detector_wip.prove on any tempo map: export start = a whole bar's time on the map plus one lag (+-80 ms),
    chosen by the Pearson r between the note pattern on the 16th grid and the onset strength at every grid point."""
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
                lag=lag, start_s=round(float(sec[16 * k0]), 4))


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
    from tools.audio_analysis.eval.kick_goal_wip_sections import ENV_BIAS
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
            off = -top['start_s'] + top['lag'] - (ENV_BIAS if top is kick else 0.0)
            row.update(top_offset_s=round(off, 4), attack=attacks_at(x, res, kick_b, off))
        return dict(row, status='not proven (patterns)')
    off = -use['start_s'] + use['lag'] - (ENV_BIAS if use is kick else 0.0)
    att = attacks_at(x, res, kick_b, off)
    row.update(offset_s=round(off, 4), attack=att)
    if att['verdict'] == 'off':
        return dict(row, status=f'not proven ({status}; attacks off)')
    return dict(row, status=f"proven ({status}; attacks {att['verdict']})")


def main():
    from tools.audio_analysis.eval.kick_goal_melodic import _source
    from tools.audio_analysis.lab.registry import load
    if len(sys.argv) < 2:
        sys.exit(__doc__)
    songs = load()
    proofs = json.loads(PROOFS.read_text()) if PROOFS.exists() else {}
    for t in sys.argv[1:]:
        als, used = _source(t)
        proofs[t] = dict(prove(songs[t], als, used), als=str(als))
        print(f"{t:18s} {proofs[t]['status']}")
    PROOFS.write_text(json.dumps(proofs, indent=1, sort_keys=True) + '\n')


if __name__ == '__main__':
    main()
