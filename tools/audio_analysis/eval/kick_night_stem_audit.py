#!/usr/bin/env python3
"""Stem evidence at each emitted trigger, plus model-independent drum-stem onsets.

For the original five (separated stems summed to the mix), report the 45-140 Hz
energy rise of each stem around every H18 emission, and list strong low-band
drum-stem onsets that no label covers. Detector output only selects where to
look; any label question must stand on the stem/waveform evidence alone.
"""
from __future__ import annotations

import json
import pickle
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[3]))

import numpy as np  # noqa: E402
from scipy.signal import butter, sosfilt  # noqa: E402

from tools.audio_analysis.eval.kick_attack_rejection import read_audio  # noqa: E402
from tools.audio_analysis.eval.kick_night_common import NIGHT, Data  # noqa: E402

AUDIO = Path('/Users/peterkiemann/MANIFOLD - Rust/tests/fixtures/audio')
STEMS = ('drums', 'bass', 'others', 'vocals')
FRAME = .005


def band_db(x, sr):
    sos = butter(4, (45, 140), btype='band', fs=sr, output='sos')
    y = sosfilt(sos, x)
    n = int(FRAME * sr)
    frames = len(y) // n
    e = np.sqrt(np.mean(y[:frames * n].reshape(frames, n) ** 2, axis=1))
    return 20 * np.log10(e + 1e-9)


def rise(db, t, pre=.05, post=.04):
    i = int(t / FRAME)
    a, b = max(0, i - int(pre / FRAME)), min(len(db), i + int(post / FRAME))
    if b <= i or i <= a:
        return float('nan'), float('nan')
    return float(db[i:b].max() - np.median(db[a:i])), float(db[i:b].max())


def onsets(db, min_rise=9., min_level=-45.):
    """Low-band onsets: 40 ms max exceeds the preceding 50 ms median by min_rise dB."""
    out, last = [], -1.
    for i in range(10, len(db) - 8):
        t = i * FRAME
        if t - last < .1:
            continue
        r, lvl = rise(db, t)
        if r >= min_rise and lvl >= min_level and db[i + 1] > db[i - 1]:
            out.append((round(t, 3), round(r, 1), round(lvl, 1)))
            last = t
    return out


def main():
    d = Data()
    with open(NIGHT / 'replay.pkl', 'rb') as f:
        replay = pickle.load(f)
    report = {}
    for track in ('bad_guy_128bpm', 'inhale_exhale_145bpm', 'tears_140bpm', 'apricots_128bpm', 'feel_the_vibration_174bpm'):
        r = d.records[track]
        stems = {}
        for s in STEMS:
            sr, x = read_audio(str(AUDIO / track / f'{s}.wav'))
            stems[s] = band_db(np.asarray(x, dtype=np.float64), sr)
        labels = sorted(r['source']['truth'])
        p = replay['h18'][1][track][0]['accuracy_by_tolerance_ms']['70']
        extras = p['extra_times_s']
        fires = [(i + 1) * r['hop'] / r['sample_rate'] for i in replay['h18'][2][track]]
        rows = []
        for t in fires:
            # Emission is ~45 ms after the attack; measure stems around t - 45 ms.
            a = t - .045
            near = min(labels, key=lambda x: abs(x - t)) if labels else None
            rows.append(dict(emit_s=round(t, 3), extra=any(abs(t - e) < 1e-6 for e in extras),
                             nearest_label_ms=round(1000 * (t - near), 1) if near is not None else None,
                             **{f'{s}_rise_db': round(rise(stems[s], a)[0], 1) for s in STEMS},
                             **{f'{s}_level_db': round(rise(stems[s], a)[1], 1) for s in STEMS}))
        drum_on = onsets(stems['drums'])
        unlabeled = [o for o in drum_on if not any(abs(o[0] - lab) <= .07 for lab in labels)]
        labeled = [o for o in drum_on if any(abs(o[0] - lab) <= .07 for lab in labels)]
        uncovered_labels = [lab for lab in labels if not any(abs(o[0] - lab) <= .07 for o in drum_on)]
        report[track] = dict(fires=rows, drum_low_onsets=len(drum_on), drum_onsets_matching_labels=len(labeled),
                             drum_onsets_unlabeled=unlabeled, labels=len(labels), labels_without_drum_onset=uncovered_labels,
                             regions=r['source']['regions'])
        ex = [x for x in rows if x['extra']]
        mt = [x for x in rows if not x['extra']]
        def med(rs, k):
            return round(float(np.nanmedian([x[k] for x in rs])), 1) if rs else None
        print(f"== {track}: fires {len(rows)} extras {len(ex)} | drum-low onsets {len(drum_on)}, on labels {len(labeled)}, "
              f"unlabeled {len(unlabeled)}, labels without drum onset {len(uncovered_labels)}")
        for k in ('drums_rise_db', 'bass_rise_db', 'others_rise_db', 'drums_level_db', 'bass_level_db'):
            print(f"   median {k}: matches {med(mt, k)} extras {med(ex, k)}")
        dom = dict(drums=0, bass=0, others=0, vocals=0)
        for x in ex:
            dom[max(STEMS, key=lambda s: x[f'{s}_level_db'] if np.isfinite(x[f'{s}_level_db']) else -1e9)] += 1
        print('   extras by loudest low-band stem:', dom)
        print('   unlabeled drum-low onsets (t, rise dB, level dB):', unlabeled[:40])
    (NIGHT / 'stem_audit.json').write_text(json.dumps(report, indent=1))


if __name__ == '__main__':
    main()
