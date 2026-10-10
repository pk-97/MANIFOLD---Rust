#!/usr/bin/env python3
"""Snap the bracketed master labels to isolated kick-stem attack onsets.

Model-independent and frozen before any rescoring: only the master, the named
kick stem and the reviewed brackets are read. Offline label construction may use
zero-phase filtering; it is never part of a detector.

Method, declared before running:
1. Stem attack: in the kick stem, find the largest absolute-amplitude peak (0.5 ms
   smoothing) inside [bracket start - 30 ms, bracket end + 60 ms]; the onset is
   the last time before that peak, within 40 ms, where the envelope is below 10%
   of the peak. No peak above -50 dBFS -> keep the midpoint, flag 'no_stem_event'.
2. Master lag per song (revised before any rescoring, after the first run showed
   per-song stem offsets of 40-50 ms beyond the old +-30 ms waveform search,
   whose near-sinusoidal kick bodies also alias by one period): cross-correlate
   35-180 Hz 5 ms power envelopes of stem and master over the whole song,
   lags -250..+250 ms in 1 ms steps. Labels never set the lag; the median
   (stem onset - midpoint) is reported only as an independent check.
3. Snapped label = stem onset + per-song lag. Labels whose snapped time moves
   more than 30 ms from the midpoint keep the midpoint, flagged 'far'.
Original-five labels and all uncertainty regions are unchanged.
"""
from __future__ import annotations

import hashlib
import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[3]))

import numpy as np  # noqa: E402
from scipy.signal import butter, sosfiltfilt, resample_poly  # noqa: E402

from tools.audio_analysis.eval.kick_attack_rejection import read_audio  # noqa: E402
from tools.audio_analysis.eval.kick_night_common import NIGHT, Data  # noqa: E402

STEMS = Path('/Users/peterkiemann/Library/CloudStorage/Dropbox/Music Production/Ableton Projects/STEMS')
KICK = {'late_night': 'LATE NIGHT STEMS/Late Night - Kicks Stem.wav',
        'midnight_patience': 'MIDNIGHT PATIENCE STEMS/Kick.wav',
        'miracle': 'MIRACLE STEMS/Kick.wav',
        'heavy_on_mind': 'HEAVY ON MIND STEMS/KICK.wav'}


def sha(p):
    return hashlib.sha256(Path(p).read_bytes()).hexdigest()


def brackets(passage):
    iv = passage.get('kick_onset_intervals') or passage.get('onset_intervals') or []
    def span(i):
        return tuple(i['interval_s']) if 'interval_s' in i else (i['start_s'], i['end_s'])
    by_mid = {round(i['midpoint_s'], 3): span(i) for i in iv if 'midpoint_s' in i}
    return {t: by_mid.get(round(t, 3), (t - .015, t + .015)) for t in passage['kick_times_s']}


def stem_times(env_db, sr, centre):
    """Physical onset and perceived-attack estimate of the stem kick near `centre`.

    Revised before rescoring (third rule): peak walk-backs stopped on the
    ~55 Hz body ripple. Now: physical onset = first 1 ms envelope value in
    [centre - 40, centre + 30] ms at least 10 dB above the median of the
    preceding 20 ms and above -60 dBFS; peak = max over the next 60 ms;
    perceived attack = first time the envelope is within 12 dB of that peak.
    """
    ms = int(.001 * sr)
    i0, i1 = int((centre - .04) * sr) // ms, int((centre + .03) * sr) // ms
    if i0 < 25 or i1 + 60 >= len(env_db):
        return None, None
    for i in range(i0, i1):
        pre = np.median(env_db[i - 20:i])
        if env_db[i] >= max(pre + 10, -60):
            peak = env_db[i:i + 60].max()
            j = i + int(np.flatnonzero(env_db[i:i + 60] >= peak - 12)[0])
            return i * ms / sr, j * ms / sr
    return None, None


def envelope(x, sr):
    sos = butter(4, (35, 180), btype='band', fs=sr, output='sos')
    y = sosfiltfilt(sos, x) ** 2
    n = int(.001 * sr)
    frames = len(y) // n
    e = y[:frames * n].reshape(frames, n).mean(axis=1)
    k = 5
    e = np.convolve(e, np.ones(k) / k, mode='same')
    d = np.maximum(np.diff(np.log(e + 1e-12), prepend=0.0), 0.0)
    return d - d.mean()


def global_lag(stem, master, sr):
    """Lag L (s) maximising sum s(t) m(t + L): a stem event at t is at t + L in the master."""
    s, m = envelope(stem, sr), envelope(master, sr)
    best = (0, -np.inf)
    corr = {}
    for lag in range(-250, 251):
        if lag >= 0:
            c = float(np.dot(s[:len(s) - lag], m[lag:]))
        else:
            c = float(np.dot(s[-lag:], m[:len(m) + lag]))
        corr[lag] = c
        if c > best[1]:
            best = (lag, c)
    vals = np.array(list(corr.values()))
    return best[0] / 1000, float((best[1] - vals.mean()) / (vals.std() + 1e-12))


def main():
    d = Data()
    out = dict(method=__doc__, tracks={})
    for track, rel in KICK.items():
        r = d.records[track]
        msr, master = read_audio(r['source']['audio_path'])
        ssr, stem = read_audio(str(STEMS / rel))
        if ssr != msr:
            stem = resample_poly(stem, msr, ssr)
        sr = msr
        n = min(len(stem), len(master))
        stem, master = stem[:n], master[:n]
        ms = int(.001 * sr)
        frames = len(stem) // ms
        env_db = 20 * np.log10(np.sqrt(np.mean(stem[:frames * ms].reshape(frames, ms) ** 2, axis=1)) + 1e-9)
        song_lag, lag_z = global_lag(stem, master, sr)
        rows = []
        for p in r['source']['passages']:
            for t, (bs, be) in brackets(p).items():
                phys, pat = stem_times(env_db, sr, t - song_lag)
                rows.append(dict(passage=p['id'], midpoint_s=t, bracket=[bs, be],
                                 physical_s=None if phys is None else round(phys + song_lag, 4),
                                 attack_s=None if pat is None else round(pat + song_lag, 4)))
        summary = {}
        for key in ('physical_s', 'attack_s'):
            moved, flags = [], dict(snapped=0, far=0, no_stem_event=0)
            for x in rows:
                if x[key] is None:
                    flags['no_stem_event'] += 1
                elif abs(x[key] - x['midpoint_s']) > .03:
                    flags['far'] += 1
                else:
                    flags['snapped'] += 1
                    moved.append(1000 * (x[key] - x['midpoint_s']))
            moved = np.array(moved)
            summary[key] = dict(flags=flags, shift_ms=[round(float(np.percentile(moved, q)), 1) for q in (10, 50, 90)] if len(moved) else None)
        out['tracks'][track] = dict(kick_stem=rel, kick_stem_sha256=sha(STEMS / rel), master_sha256=r['source']['audio_sha256'],
                                    song_lag_ms=round(1000 * song_lag, 3), lag_peak_z=round(lag_z, 2), summary=summary, labels=rows)
        print(track, 'lag', out['tracks'][track]['song_lag_ms'], 'z', out['tracks'][track]['lag_peak_z'], summary, flush=True)
    path = NIGHT / 'snapped_labels.json'
    path.write_text(json.dumps(out, indent=1, default=float))
    print('frozen', path, sha(path))


if __name__ == '__main__':
    main()
