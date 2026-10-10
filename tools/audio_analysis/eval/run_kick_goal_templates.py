#!/usr/bin/env python3
"""Kick-template features: build, component-test, cache.

Component test, declared: within-song AUC of tmpl_sim for scored kick
candidates against scored non-kicks. Expected failure: mix patches carry bass
and pad increases that wash out the kick shape. Pass to combine: median
within-song AUC >= 0.75 on dev and on new songs.
"""
from __future__ import annotations

import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[3]))

import numpy as np  # noqa: E402

from tools.audio_analysis.eval.kick_attack_rejection import read_audio  # noqa: E402
from tools.audio_analysis.eval.kick_goal_eval import GOAL, NEW_SONGS, Goal  # noqa: E402
from tools.audio_analysis.eval.kick_goal_labels import DEV_STEMS, NEW, fresh_onsets, kick_env_db, load  # noqa: E402
from tools.audio_analysis.eval.kick_goal_templates import TEMPLATE_NAMES, band_spec, fit_templates, patches, template_features  # noqa: E402
from tools.audio_analysis.eval.kick_night_diagnose import auc  # noqa: E402
from tools.audio_analysis.eval.kick_night_stem_audit import AUDIO  # noqa: E402
from tools.audio_analysis.eval.run_kick_goal_tail_component import mix_audio  # noqa: E402

HOP_48K = 256


def stem_patches(x, sr, onsets_s):
    hop = max(1, round(sr * HOP_48K / 48000))
    spec = band_spec(x, sr, hop)
    cand = np.array([int(t * sr / hop) for t in onsets_s])
    span = int(np.ceil(.040 * sr / hop))
    return patches(spec, cand, cand + span)


def template_sets(g):
    new_p = []
    for name in NEW_SONGS:
        sr = g.records[name]['sample_rate']
        k = load(NEW[name]['kick'], sr)
        new_p.append(stem_patches(k, sr, fresh_onsets(kick_env_db(k, sr))))
    dev_p = []
    for t in g.records:
        if t in NEW_SONGS:
            continue
        r = g.records[t]
        sr = r['sample_rate']
        if t in DEV_STEMS:
            k = load(DEV_STEMS[t]['kick'], sr)
            dev_p.append(stem_patches(k, sr, fresh_onsets(kick_env_db(k, sr))))
        else:
            d = read_audio(str(AUDIO / t / 'drums.wav'), sr)[1]
            dev_p.append(stem_patches(d, sr, r['source']['truth']))
    return fit_templates(new_p), fit_templates(dev_p)


def main():
    g = Goal(mode='strict')
    t_new, t_dev = template_sets(g)
    np.save(GOAL / 'templates_from_new.npy', t_new)
    np.save(GOAL / 'templates_from_dev.npy', t_dev)
    report = {}
    for t, r in g.records.items():
        spec = band_spec(mix_audio(g, t), r['sample_rate'], r['hop'])
        f = template_features(patches(spec, r['candidates'], r['available']), t_dev if t in NEW_SONGS else t_new)
        np.save(GOAL / f'tmpl_{t}.npy', f)
        m, y = r['training_mask'], r['labels']
        report[t] = {n: round(auc(f[m & (y == 1), k], f[m & (y == 0), k]), 3) for k, n in enumerate(TEMPLATE_NAMES)}
        print(t, report[t], flush=True)
    for grp, names in (('dev', [t for t in g.records if t not in NEW_SONGS]), ('new', list(NEW_SONGS))):
        print(grp, 'median AUC', {n: round(float(np.median([report[t][n] for t in names])), 3) for n in TEMPLATE_NAMES})
    (GOAL / 'template_component.json').write_text(json.dumps(report, indent=1))


if __name__ == '__main__':
    main()
