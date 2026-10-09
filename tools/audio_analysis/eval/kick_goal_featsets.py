"""Named feature sets for the kick goal runs, built from the per-song caches in GOAL.

features: the 15 frozen features. f21: + tail (3) + template (3). f30/f15n/f31:
song-normalised variants (kick_goal_songnorm). f53: f21 + rise profile (32).
f47: features + profile. f68: f53 + normalised 15. f69: f53 + low filter bank (16).
f85: f69 + sidechain fall profile (16).
"""
from __future__ import annotations

import numpy as np

from tools.audio_analysis.eval.kick_goal_eval import GOAL, NEW_SONGS, TRACKS
from tools.audio_analysis.eval.kick_goal_lowbank import band_envelopes, lowbank_features
from tools.audio_analysis.eval.kick_goal_profile import fall_profile, rise_profile
from tools.audio_analysis.eval.kick_goal_songnorm import add_normalised
from tools.audio_analysis.eval.kick_goal_templates import band_spec
from tools.audio_analysis.eval.run_kick_goal_tail_component import mix_audio


def profile_cache(g, t):
    path = GOAL / f'prof_{t}.npy'
    if not path.exists():
        r = g.records[t]
        np.save(path, rise_profile(band_spec(mix_audio(g, t), r['sample_rate'], r['hop']), r['candidates'], r['available']))
    return np.load(path)


def lowbank_cache(g, t):
    path = GOAL / f'low_{t}.npy'
    if not path.exists():
        r = g.records[t]
        np.save(path, lowbank_features(band_envelopes(mix_audio(g, t), r['sample_rate']), r['onset_s'], r['emit_s']))
    return np.load(path)


def fall_cache(g, t):
    path = GOAL / f'fall_{t}.npy'
    if not path.exists():
        r = g.records[t]
        np.save(path, fall_profile(band_spec(mix_audio(g, t), r['sample_rate'], r['hop']), r['candidates'], r['available']))
    return np.load(path)


def build(g, feats):
    """Adds record[feats] (and its ingredients) to every record."""
    if feats == 'features':
        return
    add_normalised(g, TRACKS, NEW_SONGS, GOAL)
    for t, r in g.records.items():
        r['f21'] = np.hstack([r['features'], np.load(GOAL / f'tail_{t}.npy'), np.load(GOAL / f'tmpl_{t}.npy')])
        if feats in ('f53', 'f47', 'f68', 'f69', 'f85'):
            prof = profile_cache(g, t)
            r['f53'] = np.hstack([r['f21'], prof])
            r['f47'] = np.hstack([r['features'], prof])
            r['f68'] = np.hstack([r['f53'], r['f15n']])
        if feats in ('f69', 'f85'):
            r['low16'] = lowbank_cache(g, t)
            r['f69'] = np.hstack([r['f53'], r['low16']])
        if feats == 'f85':
            r['f85'] = np.hstack([r['f69'], fall_cache(g, t)])
