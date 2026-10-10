#!/usr/bin/env python3
"""H22: pitch glide as the fallback evidence where the kernel lacks support.

Declared before scoring (rule written to the night cache first):
- Diagnosis (kick_night_lowsupport_probe.py, all-song, development): among
  hard candidates with kernel activity < 5, glide points the physical way in
  every song with data (slope AUC Bad Guy .04, Inhale .06, Midnight .11, Feel
  .25, Heavy .33, Tears .36; kicks sweep down), while the linear score is weak
  there (Bad Guy .49, Inhale .59). In dense support glide flips by song, which
  is why H19's unconditional stack failed. Low-support candidates are the tonal
  low-band events: pitched kicks and synth bass notes.
- Score: g * S + (1 - g) * F, g = act/(act + 5) as H21; F = L + glide @ beta + b,
  a logistic stack with the frozen covered-linear logit L as offset.
- beta is fitted per fold on training songs only (out-of-fold logits/activity),
  l2 = 0.01, songs and classes weighted equally; inner cutoffs refit without
  the inner song.
- Configurations: C1 S = H18, beta fitted on low-support (act < 5) training
  candidates; C2 S = H16 kernel, same; C3 control: S = H18, beta fitted on all
  training candidates (tests whether the support conditioning matters).
- Expected failure: few low-support training candidates per fold (tens), so beta
  is noisy; Bad Guy's own fold must learn from Inhale/Tears/Midnight/Feel only.
- Acceptance: intermediate target.
"""
from __future__ import annotations

import hashlib
import json
import pickle
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[3]))

import numpy as np  # noqa: E402
from scipy.special import expit  # noqa: E402

from tools.audio_analysis.eval.kick_night_common import (  # noqa: E402
    NIGHT, TRACKS, Data, acceptance, evaluate_scorer, h18_logit, retention)
from tools.audio_analysis.eval.kick_night_stack import apply_stack, fit_stack  # noqa: E402

RULE = dict(hypothesis='H22', kappa=5.0, low_support_act=5.0, l2=.01,
            configurations=['h18_gate_glide_lowfit', 'h16_gate_glide_lowfit', 'h18_gate_glide_allfit_control'],
            acceptance='intermediate target', expected_failure='few low-support training candidates; noisy beta')


def load_inputs():
    with np.load(NIGHT / 'glide.npz') as z:
        glide = {k: z[k] for k in z.files}
    with np.load(NIGHT / 'kernel_activity.npz') as z:
        act = {k: z[k] for k in z.files}
    return glide, act


def make_h22(d, glide, act, expert, low_only):
    """Return (logit(outer, target), fallback_model(outer, target)) for one configuration."""
    lin = lambda o, t: d.logits(o, t, 'linear')  # noqa: E731
    experts = {'h18': lambda o, t: h18_logit(d, o, t), 'h16': lambda o, t: d.logits(o, t, 'kernel')}
    models = {}

    def fallback_model(o, t):
        key = (o, frozenset({o, t}))
        if key not in models:
            offs, xs, ys, ss = [], [], [], []
            for u in TRACKS:
                if u in (o, t):
                    continue
                r = d.records[u]
                # A copy: an in-place mask edit once leaked between folds and configs.
                m = np.array(r['training_mask'], dtype=bool)
                if low_only:
                    m = m & (act[f'{o}|{u}'] < RULE['low_support_act'])
                offs.append(lin(o, u)[m]); xs.append(glide[u][m])
                ys.append(np.asarray(r['labels'])[m]); ss.append(np.full(m.sum(), TRACKS.index(u)))
            models[key] = fit_stack(np.concatenate(offs), np.concatenate(xs), np.concatenate(ys).astype(float),
                                    np.concatenate(ss), RULE['l2'])
        return models[key]

    def logit(o, t):
        a = act[f'{o}|{t}']
        g = a / (a + RULE['kappa'])
        f = apply_stack(fallback_model(o, t), lin(o, t), glide[t])
        return g * experts[expert](o, t) + (1 - g) * f
    return logit, fallback_model


CONFIGS = {'h18_gate_glide_lowfit': ('h18', True), 'h16_gate_glide_lowfit': ('h16', True),
           'h18_gate_glide_allfit_control': ('h18', False)}


def main():
    out = NIGHT / 'h22'
    out.mkdir(exist_ok=True)
    rule_path = out / 'rule.json'
    if not rule_path.exists():
        rule_path.write_text(json.dumps(RULE, indent=1))
    if json.loads(rule_path.read_text()) != RULE:
        raise ValueError('predeclared rule differs')
    d = Data()
    glide, act = load_inputs()
    with open(NIGHT / 'replay.pkl', 'rb') as f:
        replay = pickle.load(f)
    report = dict(rule=RULE, rule_sha256=hashlib.sha256(rule_path.read_bytes()).hexdigest(), variants=[])
    for name in RULE['configurations']:
        logit, fm = make_h22(d, glide, act, *CONFIGS[name])
        s, by_track, fires = evaluate_scorer(d, lambda o, t, lg=logit: expit(lg(o, t)), name=name)
        ret = retention(replay['baseline'][1], by_track)
        ret18 = retention(replay['h18'][1], by_track)
        acc = acceptance(s, ret)
        betas = {t[:6]: fm(t, t)['beta'].round(3).tolist() for t in ('bad_guy_128bpm', 'inhale_exhale_145bpm', 'heavy_on_mind')}
        report['variants'].append(dict(name=name, summary=s, acceptance=acc, betas=betas,
                                       lost_vs_baseline={t: v['lost'] for t, v in ret.items() if v['lost']},
                                       recovered_vs_baseline=sum(len(v['recovered']) for v in ret.values()),
                                       lost_vs_h18=sum(len(v['lost']) for v in ret18.values()),
                                       recovered_vs_h18=sum(len(v['recovered']) for v in ret18.values())))
        with open(out / f'{name}.pkl', 'wb') as f:
            pickle.dump((s, by_track, fires), f)
        v = report['variants'][-1]
        print(name, {k: (x['matched'], x['extra']) for k, x in s['tol'].items()}, 'cores', s['kick_free_extras'],
              'per', {t[:6]: x[:2] for t, x in s['per_track'].items()}, flush=True)
        print('   acceptance', acc, 'lost', v['lost_vs_baseline'], 'recovered', v['recovered_vs_baseline'],
              'vs H18 lost/recovered', v['lost_vs_h18'], v['recovered_vs_h18'], 'betas', betas, flush=True)
    (out / 'trial.json').write_text(json.dumps(report, indent=1, default=str))


if __name__ == '__main__':
    main()
