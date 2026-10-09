#!/usr/bin/env python3
"""Kick truth v2 under Peter's rule, for the nine dev songs and five new songs.

Peter, 2026-10-10: a fresh kick attack in the kick stem or in the drum bus is a
kick; a fire on a ringing kick tail (a retrigger over the kick's own sustain) is
a false trigger. Declared before any rescoring; stems and masters only, no
detector output is read.

- Kick-stem envelope: analytic-signal magnitude, 5 ms moving average, 1 ms
  frames, dB. A 1 ms RMS envelope ripples inside a 40-60 Hz kick body and fakes
  10 dB "attacks" on ringing tails (seen on the first label-check renders); the
  analytic envelope does not.
- Fresh attack: the envelope reaches 10 dB above the median of the preceding
  20 ms, above -60 dBFS, with its next-60 ms peak within 30 dB of the stem's
  99.9th-percentile level; one per 60 ms.
- Drum-bus kick: a 45-140 Hz drum-bus onset (stem-audit rule: 9 dB rise, -45 dB
  level) whose first 40 ms put at least 25% of 30-8000 Hz energy below 140 Hz.
  Bad Guy's labelled kicks sit near 38% on its drum stem, its claps at 5% or less.
- Dev own-stem songs: a label stays when a fresh kick-stem attack or a drum-bus
  kick lies within [-40, +30] ms of it (the snap window). Otherwise it is a
  ringing kick tail (kick stem within 30 dB of its peak level), bass only, or
  no attack. Unlabelled fresh kick-stem attacks inside scored passages are
  flagged for visual review, never added here.
- Original five: reviewed labels stand; unlabelled kick-shaped drum-stem onsets
  are flagged for review.
- New songs: labels are fresh kick-stem attacks plus drum-bus kicks with no
  kick-stem attack within 70 ms (drum-only songs: drum-bus kicks). The mix is
  the real master when the label-free envelope lag to the kick stem is clear
  (z >= 8), else the sum of the export's stems, where every stem has lag 0.
  Scored: the whole song after the first and before the last second.
"""
from __future__ import annotations

import hashlib
import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[3]))

import numpy as np  # noqa: E402
from scipy.signal import hilbert  # noqa: E402

from tools.audio_analysis.eval.kick_attack_rejection import read_audio  # noqa: E402
from tools.audio_analysis.eval.kick_night_common import NIGHT, TRACKS, Data  # noqa: E402
from tools.audio_analysis.eval.kick_night_diagnose import scored_labels  # noqa: E402
from tools.audio_analysis.eval.kick_night_drum_onset_shape import energies  # noqa: E402
from tools.audio_analysis.eval.kick_night_snap_labels import envelope  # noqa: E402
from tools.audio_analysis.eval.kick_night_stem_audit import AUDIO, band_db, onsets  # noqa: E402

GOAL = Path.home() / '.cache/manifold/kick-goal-2026-10-10'
DROP = Path('/Users/peterkiemann/Library/CloudStorage/Dropbox/Music Production')
ABL = DROP / 'Ableton Projects'
STEMS = ABL / 'STEMS'
DEV_STEMS = {
    'late_night': dict(kick=STEMS / 'LATE NIGHT STEMS/Late Night - Kicks Stem.wav', drums=[STEMS / 'LATE NIGHT STEMS/Late Night - Drums Stem.wav'],
                       bass=[STEMS / 'LATE NIGHT STEMS/Late Night - Bass and Sub Stem.wav']),
    'midnight_patience': dict(kick=STEMS / 'MIDNIGHT PATIENCE STEMS/Kick.wav', drums=[STEMS / 'MIDNIGHT PATIENCE STEMS/Drums.wav'],
                              bass=[STEMS / 'MIDNIGHT PATIENCE STEMS/Bass and Sub.wav']),
    'miracle': dict(kick=STEMS / 'MIRACLE STEMS/Kick.wav', drums=[STEMS / 'MIRACLE STEMS/Drums.wav'], bass=[STEMS / 'MIRACLE STEMS/Subs.wav']),
    'heavy_on_mind': dict(kick=STEMS / 'HEAVY ON MIND STEMS/KICK.wav', drums=[STEMS / 'HEAVY ON MIND STEMS/DRUMS.wav'],
                          bass=[STEMS / 'HEAVY ON MIND STEMS/BASS AND SUB.wav']),
}
PREMASTERS = ABL / '2025/cool drums Project/FINALS/PREMASTERS T2'
NEW = {
    'pattern': dict(mix=ABL / '2025/Pattern Project/MASTERS/32Bit/Pattern - Master (Premaster Fix) - V1.wav',
                    kick=ABL / '2025/Pattern Project/MASTERS/32Bit/Pattern - KICK STEM - 32Bit.wav', drums=[], bass=[],
                    parts=[ABL / '2025/Pattern Project/MASTERS/32Bit/Pattern - KICK STEM - 32Bit.wav',
                           ABL / '2025/Pattern Project/MASTERS/32Bit/Pattern - NO KICK - 32Bit.wav']),
    'back_to_you': dict(mix=ABL / '2024/States Project/Alex Master/Latent Space - Back To You T2 (mastered) 48k 24-bit - RELEASE.wav',
                        kick=ABL / '2024/States Project/STEMS/PRE MASTER/KICK.wav',
                        drums=[ABL / '2024/States Project/STEMS/PRE MASTER/ALL DRUMS.wav'],
                        bass=[ABL / '2024/States Project/STEMS/PRE MASTER/BASS.wav'],
                        parts_dir=ABL / '2024/States Project/STEMS/PRE MASTER'),
    'burn_stems': dict(mix=None, kick=ABL / '2025/Fifty Project/STEMS 3-08-25/KICK.wav',
                       drums=[ABL / '2025/Fifty Project/STEMS 3-08-25/DRUMS.wav'],
                       bass=[ABL / '2025/Fifty Project/STEMS 3-08-25/BASS AND SUBS.wav'],
                       parts_dir=ABL / '2025/Fifty Project/STEMS 3-08-25'),
    'mirage': dict(mix=None, kick=None, drums=[PREMASTERS / '02 Mirage - PREMASTER - DRUMS.wav'], bass=[],
                   parts=[PREMASTERS / '02 Mirage - PREMASTER - DRUMS.wav', PREMASTERS / '02 Mirage - PREMASTER - NO DRUMS.wav']),
    'cold_remix': dict(mix=None, kick=ABL / '2024/Eleonora - Cold - Remix Project/STEMS - Cold/_ KICK.wav',
                       drums=[ABL / '2024/Eleonora - Cold - Remix Project/STEMS - Cold/_ PERCUSSION.wav'],
                       bass=[ABL / '2024/Eleonora - Cold - Remix Project/STEMS - Cold/_ BASS GUITAR.wav'],
                       parts_dir=ABL / '2024/Eleonora - Cold - Remix Project/STEMS - Cold'),
}
MS = .001
DRUM_KICK_SHARE = .25
LAG_Z_MIN = 8.0
SNAP = (-.04, .03)


def sha(p):
    return hashlib.sha256(Path(p).read_bytes()).hexdigest()


def kick_env_db(x, sr):
    """Analytic envelope, 5 ms smoothing, sampled on an exact 1 ms grid, dB.

    The grid is exact at any sample rate: whole-sample frames drift by 0.2% at
    44.1 kHz, about 130 ms by one minute in.
    """
    a = np.abs(hilbert(x))
    k = max(1, int(.005 * sr))
    a = np.convolve(a, np.ones(k) / k, mode='same')
    idx = np.round(np.arange(int(len(a) / sr / MS)) * MS * sr).astype(int)
    return 20 * np.log10(a[np.minimum(idx, len(a) - 1)] + 1e-9)


def fresh_onsets(e):
    """Fresh attacks (seconds, stem time) on a kick_env_db envelope; one per 60 ms."""
    floor = np.percentile(e, 99.9) - 30
    med = np.full(len(e), np.inf)
    med[20:] = np.median(np.lib.stride_tricks.sliding_window_view(e, 20)[:-1], axis=1)
    out, last = [], -10 ** 9
    for i in np.flatnonzero((e >= med + 10) & (e >= -60)):
        if i - last < 60 or e[i:i + 60].max() < floor:
            continue
        out.append(i * MS)
        last = i
    return np.array(out)


def ringing(e, t):
    """Kick stem sounding within 30 dB of its peak level at stem time t."""
    i = int(t / MS)
    return 0 <= i < len(e) and e[i] >= np.percentile(e, 99.9) - 30


def low_share(x, sr, t):
    e = energies(x, sr, t)
    return (e['sub'] + e['low']) / (sum(e.values()) + 1e-20)


def lag(stem, mix, sr):
    """Label-free lag L: a stem event at t sits at t + L in the mix; (L, peak z)."""
    s, m = envelope(stem, sr), envelope(mix, sr)
    n = 1 << int(np.ceil(np.log2(len(s) + len(m))))
    c = np.fft.irfft(np.fft.rfft(m, n) * np.conj(np.fft.rfft(s, n)), n)
    lags = np.concatenate([np.arange(0, 20000), np.arange(-20000, 0)])
    vals = np.concatenate([c[:20000], c[-20000:]])
    k = int(np.argmax(vals))
    return lags[k] * MS, float((vals[k] - vals.mean()) / (vals.std() + 1e-12))


def load(path, sr):
    return read_audio(str(path), sr)[1]


def stem_sum(cfg, sr):
    parts = cfg.get('parts') or sorted(Path(cfg['parts_dir']).glob('*.wav'))
    xs = [load(p, sr) for p in parts]
    out = np.zeros(max(len(x) for x in xs))
    for x in xs:
        out[:len(x)] += x
    return out, [str(p) for p in parts]


def drum_kicks(drum, sr):
    """Kick-shaped drum-bus onsets (stem time)."""
    return np.array([t for t, r_db, lvl in onsets(band_db(drum, sr)) if low_share(drum, sr, t) >= DRUM_KICK_SHARE])


def near(times, t, lo, hi):
    return len(times) > 0 and bool(np.any((times - t >= lo) & (times - t <= hi)))


def attack_at(e, s):
    """A reviewed label's own fresh attack: inside the snap window the envelope
    clears the median of the preceding 20 ms by 10 dB, or of 20-40 ms back by
    10 dB for slow attacks, above -60 dBFS. No level floor: quiet kicks count."""
    i0, i1 = int((s + SNAP[0]) / MS), int((s + SNAP[1]) / MS)
    for i in range(max(40, i0), min(len(e), i1 + 1)):
        if e[i] >= -60 and (e[i] >= np.median(e[i - 20:i]) + 10 or e[i] >= np.median(e[i - 40:i - 20]) + 10):
            return True
    return False


def dev_labels(d):
    lags = json.loads((NIGHT / 'snapped_labels_frozen.json').read_text())['tracks']
    out = {}
    for t in TRACKS:
        src = d.records[t]['source']
        labels, cores, excl, ignored = scored_labels(src)
        row = dict(group=src['group'], scored_v1=len(labels), kept=[], removed=[], flagged_missing=[])
        every = np.array([x for _, x in labels] + ignored)
        if t in DEV_STEMS:
            sr = d.records[t]['sample_rate']
            cfg = DEV_STEMS[t]
            shift = lags[t]['song_lag_ms'] / 1000
            ke = kick_env_db(load(cfg['kick'], sr), sr)
            fresh = fresh_onsets(ke)
            dk = drum_kicks(load(cfg['drums'][0], sr), sr)
            bass_on = np.array([o[0] for o in onsets(band_db(load(cfg['bass'][0], sr), sr))]) if cfg['bass'] else np.array([])
            for pid, x in labels:
                s = x - shift
                if near(fresh, s, *SNAP) or attack_at(ke, s):
                    c = 'kick_stem'
                elif near(dk, s, *SNAP):
                    c = 'drums_bus_kick'
                elif ringing(ke, s):
                    c = 'kick_tail'
                elif near(bass_on, s, *SNAP):
                    c = 'bass_only'
                else:
                    c = 'no_attack'
                (row['kept'] if c in ('kick_stem', 'drums_bus_kick') else row['removed']).append(dict(passage=pid, t=round(x, 4), cls=c))
            for a, b in cores:
                for f in fresh + shift:
                    if a + .05 <= f < b - .05 and not np.any(np.abs(every - f) <= .07) and not any(s0 - .07 <= f <= e0 + .2 for s0, e0 in excl):
                        row['flagged_missing'].append(round(float(f), 4))
        else:
            sr, drums = read_audio(str(AUDIO / t / 'drums.wav'))
            row['kept'] = [dict(passage=pid, t=round(x, 4), cls='reviewed') for pid, x in labels]
            for f in drum_kicks(drums, sr):
                if not np.any(np.abs(every - f) <= .07) and not any(s0 - .07 <= f <= e0 + .2 for s0, e0 in excl):
                    row['flagged_missing'].append(round(float(f), 4))
            # Unlabelled low drum-stem onsets are uncertain, not non-kicks: Bad Guy's
            # backbeat carries a low body 7-12 dB under its kicks (BUG-sa3n3), and the
            # visual audit read some as kicks. Excluded from scoring until Peter rules.
            # Tears 13.07 is a floor-to-hump drum-stem kick with a low burst in the mix;
            # 12.755 is a weaker pre-hit (both from the 2026-10-10 visual audit).
            row['uncertain'] = ([round(float(o[0]), 4) for o in onsets(band_db(drums, sr))
                                 if not np.any(np.abs(every - o[0]) <= .07)] if t == 'bad_guy_128bpm' else
                                [f for f in row['flagged_missing'] if t == 'tears_140bpm' and abs(f - 12.755) < .01])
            row['added'] = [f for f in row['flagged_missing'] if t == 'tears_140bpm' and abs(f - 13.07) < .01]
        out[t] = row
        cls = {}
        for r in row['removed']:
            cls[r['cls']] = cls.get(r['cls'], 0) + 1
        print(t, 'v1', row['scored_v1'], 'kept', len(row['kept']), 'removed', len(row['removed']), cls,
              'flagged missing', len(row['flagged_missing']), flush=True)
    return out


def new_labels():
    out = {}
    for name, cfg in NEW.items():
        sr = read_audio(str(cfg['mix']))[0] if cfg['mix'] is not None else 48000
        kick = load(cfg['kick'], sr) if cfg['kick'] is not None else None
        mix_kind, mix_path, parts, shift, z = 'stem_sum', None, None, 0.0, None
        if cfg['mix'] is not None:
            mix = load(cfg['mix'], sr)
            shift, z = lag(kick, mix, sr)
            if z >= LAG_Z_MIN:
                mix_kind, mix_path = 'master', cfg['mix']
            else:
                mix_kind, shift = f'stem_sum (master lag z {z:.1f} < {LAG_Z_MIN})', 0.0
        if mix_path is None:
            mix, parts = stem_sum(cfg, sr)
        fresh = fresh_onsets(kick_env_db(kick, sr)) + shift if kick is not None else np.array([])
        extra = []
        for p in cfg['drums']:
            for f in drum_kicks(load(p, sr), sr) + shift:
                if not near(fresh, f, -.07, .07):
                    extra.append(f)
        dur = len(mix) / sr
        times = sorted(set(round(float(t), 4) for t in np.concatenate([fresh, np.array(extra)]) if 1.0 <= t <= dur - 1.0))
        out[name] = dict(mix_kind=mix_kind, mix_path=str(mix_path) if mix_path else None, mix_parts=parts,
                         mix_sha256=sha(mix_path) if mix_path else None, sample_rate=sr, duration_s=round(dur, 3),
                         kick_stem=str(cfg['kick']) if cfg['kick'] else None, kick_lag_ms=round(1000 * shift, 1),
                         kick_lag_z=round(z, 2) if z is not None else None,
                         kick_stem_onsets=int(np.sum((fresh >= 1.0) & (fresh <= dur - 1.0))), drum_bus_only_kicks=len(extra),
                         labels=times)
        print(name, mix_kind, 'lag', out[name]['kick_lag_ms'], 'z', out[name]['kick_lag_z'], 'dur', round(dur, 1),
              'labels', len(times), 'kick-stem', out[name]['kick_stem_onsets'], 'drum-bus-only', len(extra), flush=True)
        if mix_path is None:
            np.save(GOAL / f'{name}_mix.npy', mix.astype(np.float32))
    return out


def main():
    GOAL.mkdir(parents=True, exist_ok=True)
    d = Data()
    report = dict(method=__doc__, dev=dev_labels(d), new=new_labels())
    path = GOAL / 'labels_v2.json'
    path.write_text(json.dumps(report, indent=1))
    print(path, sha(path))


if __name__ == '__main__':
    main()
