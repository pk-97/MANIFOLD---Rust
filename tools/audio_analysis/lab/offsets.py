#!/usr/bin/env python3
"""Export-offset proofs for project songs (docs/DETECTOR_LAB_DESIGN.md D5), by detector_wip's method on the song's
own mix: the project's kick notes against the mix's low onset strength, its snare notes against the 1-8 kHz
rises, each fitted to whole bars plus a lag. Proven when both patterns pick the same bar, or when only one pattern
exists and it wins clearly (r >= 0.3, margin >= 1 sd). A proof counts only when its offset equals the one the labels
use (within 1 ms). Writes lab/offset_proofs.json; registry.py reads it.

Usage: offsets.py SONG..."""
import json
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT))
from tools.audio_analysis.lab import recipe  # noqa: E402

recipe.apply()

PROOFS = Path(__file__).with_name('offset_proofs.json')
CLEAR_R, CLEAR_SD = .3, 1.0


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
