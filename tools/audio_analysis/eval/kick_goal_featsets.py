"""Named feature sets for the kick goal runs, built from the per-song caches in GOAL.

features: the 15 frozen features. f21: + tail (3) + template (3). f30/f15n/f31:
song-normalised variants (kick_goal_songnorm). f53: f21 + rise profile (32).
f47: features + profile. f68: f53 + normalised 15. f69: f53 + low filter bank (16).
f85: f69 + sidechain fall profile (16). f31b: the 15 + low filter bank (16), the small detector.
f47r: f31b + causal percussive residual (16). f85r: f69 + residual. f31r: features + residual (residual in place of low16).
fx: everything measured so far, in one set: f85 + residual + causal pitch glide (3). Asks whether combinations hold kick information no single family showed.
"""
from __future__ import annotations

import numpy as np

from tools.audio_analysis.eval.kick_glide_features import glide_features
from tools.audio_analysis.eval.kick_goal_eval import GOAL, NEW_SONGS, TRACKS
from tools.audio_analysis.eval.kick_goal_lowbank import band_envelopes, lowbank_features
from tools.audio_analysis.eval.kick_goal_profile import fall_profile, rise_profile
from tools.audio_analysis.eval.kick_goal_residual import residual_features
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


def env_cache(g, t):
    path = GOAL / f'env_{t}.npy'
    if not path.exists():
        np.save(path, band_envelopes(mix_audio(g, t), g.records[t]['sample_rate']).astype(np.float32))
    return np.load(path)


def residual_cache(g, t):
    path = GOAL / f'res_{t}.npy'
    if not path.exists():
        r = g.records[t]
        np.save(path, residual_features(env_cache(g, t).astype(np.float64), r['onset_s'], r['emit_s']))
    return np.load(path)


def glide_cache(g, t):
    path = GOAL / f'glide_{t}.npy'
    if not path.exists():
        r = g.records[t]
        np.save(path, glide_features(mix_audio(g, t), r['sample_rate'], r['hop'], r['candidates'], r['available']))
    return np.load(path)


def build(g, feats):
    """Adds record[feats] (and its ingredients) to every record."""
    if feats == 'features':
        return
    add_normalised(g, TRACKS, NEW_SONGS, GOAL)
    for t, r in g.records.items():
        r['f21'] = np.hstack([r['features'], np.load(GOAL / f'tail_{t}.npy'), np.load(GOAL / f'tmpl_{t}.npy')])
        if feats in ('f53', 'f47', 'f68', 'f69', 'f85', 'f85r', 'fx'):
            prof = profile_cache(g, t)
            r['f53'] = np.hstack([r['f21'], prof])
            r['f47'] = np.hstack([r['features'], prof])
            r['f68'] = np.hstack([r['f53'], r['f15n']])
        if feats in ('f69', 'f85', 'f31b', 'f47r', 'f85r', 'f31r', 'fx'):
            r['low16'] = lowbank_cache(g, t)
            r['f31b'] = np.hstack([r['features'], r['low16']])
            if 'f53' in r:
                r['f69'] = np.hstack([r['f53'], r['low16']])
        if feats in ('f85', 'fx'):
            r['f85'] = np.hstack([r['f69'], fall_cache(g, t)])
        if feats == 'fx':
            r['fx'] = np.hstack([r['f85'], residual_cache(g, t), glide_cache(g, t)])
        if feats in ('f47r', 'f85r', 'f31r'):
            r['res16'] = residual_cache(g, t)
            r['f47r'] = np.hstack([r['f31b'], r['res16']])
            r['f31r'] = np.hstack([r['features'], r['res16']])
            if 'f69' in r:
                r['f85r'] = np.hstack([r['f69'], r['res16']])
