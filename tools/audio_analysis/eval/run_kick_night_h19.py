#!/usr/bin/env python3
"""H19: does causal pitch glide add information on top of existing scores?

Declared before scoring (rule written to the night cache first):
- Hypothesis: the 15 frozen values cannot see a 40 ms low-band pitch sweep
  (FFT frames too long); glide_slope/glide_drop/pre_jump add it.
- Configurations (exactly three, l2 = 0.01 on standardised glide values):
  C1 baseline linear15 + glide (component test in the simplest scorer),
  C2 H18 .25 blend + glide, C3 H16 kernel + glide.
- Fit: logistic stack with the frozen score as offset; fitted only on training
  songs' out-of-fold logits; inner cutoff selection refits without the inner song.
- Expected failure: glide's relation reverses across songs (Late Night, Feel),
  so one transferred weight helps Bad Guy/Inhale but hurts others; net gain small.
- Acceptance: intermediate target (>=223 with <=70 extras OR >=250 with <=89 at
  70 ms; zero fires in the nine kick-free cores; <=1 original-baseline label lost
  per track by identity). Also reported at 35/50 ms and against its own base.
"""
from __future__ import annotations

import hashlib
import json
import pickle
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[3]))

import numpy as np  # noqa: E402

from tools.audio_analysis.eval.kick_night_common import (  # noqa: E402
    NIGHT, Data, acceptance, baseline_logit, evaluate_scorer, h16_logit, h18_logit, retention)
from tools.audio_analysis.eval.kick_night_glide_component import extract  # noqa: E402
from tools.audio_analysis.eval.kick_night_stack import Stacker  # noqa: E402

RULE = dict(hypothesis='H19', l2=.01, features=['glide_slope', 'glide_drop', 'pre_jump'],
            configurations=['baseline_linear15+glide', 'h18_.25+glide', 'h16_kernel+glide'],
            acceptance='>=223 with <=70 extras OR >=250 with <=89 at 70ms; zero nine cores; <=1 baseline label lost per track',
            expected_failure='glide relation reverses across songs; transferred weight helps Bad Guy/Inhale, hurts Late Night/Feel')


def main():
    out = NIGHT / 'h19'
    out.mkdir(exist_ok=True)
    rule_path = out / 'rule.json'
    if not rule_path.exists():
        rule_path.write_text(json.dumps(RULE, indent=1))
    if json.loads(rule_path.read_text()) != RULE:
        raise ValueError('predeclared rule differs')
    d = Data()
    glide, meta = extract(d)
    with open(NIGHT / 'replay.pkl', 'rb') as f:
        replay = pickle.load(f)
    design = lambda o, t: glide[t]  # noqa: E731
    bases = {'baseline_linear15+glide': (baseline_logit, 'baseline'),
             'h18_.25+glide': (lambda dd, o, t: h18_logit(dd, o, t), 'h18'),
             'h16_kernel+glide': (h16_logit, 'h16')}
    report = dict(rule=RULE, rule_sha256=hashlib.sha256(rule_path.read_bytes()).hexdigest(),
                  glide_meta=meta, variants=[])
    for name in RULE['configurations']:
        fn, ref = bases[name]
        st = Stacker(d, lambda o, t, fn=fn: fn(d, o, t), design, RULE['l2'])
        c0 = time.process_time()
        s, by_track, fires = evaluate_scorer(d, st.prob, name=name)
        ret_base = retention(replay['baseline'][1], by_track)
        ret_self = retention(replay[ref][1], by_track)
        acc = acceptance(s, ret_base)
        betas = {o: st.model(o, o)['beta'].round(3).tolist() for o in ('bad_guy_128bpm', 'late_night', 'inhale_exhale_145bpm')}
        report['variants'].append(dict(name=name, summary=s, acceptance=acc, cpu_s=time.process_time() - c0,
                                       lost_vs_baseline={t: v['lost'] for t, v in ret_base.items() if v['lost']},
                                       recovered_vs_baseline=sum(len(v['recovered']) for v in ret_base.values()),
                                       lost_vs_own_base={t: v['lost'] for t, v in ret_self.items() if v['lost']},
                                       recovered_vs_own_base={t: v['recovered'] for t, v in ret_self.items() if v['recovered']},
                                       outer_betas=betas))
        with open(out / f'{name}.pkl', 'wb') as f:
            pickle.dump((s, by_track, fires), f)
        print(name, s['tol'], 'cores', s['kick_free_extras'], 'per', {t[:6]: v[:2] for t, v in s['per_track'].items()}, flush=True)
        print('   acceptance', acc, 'lost vs baseline', report['variants'][-1]['lost_vs_baseline'], flush=True)
        print('   vs own base: lost', report['variants'][-1]['lost_vs_own_base'], 'recovered',
              {t: len(v) for t, v in report['variants'][-1]['recovered_vs_own_base'].items()}, 'betas', betas, flush=True)
    (out / 'trial.json').write_text(json.dumps(report, indent=1, default=str))


if __name__ == '__main__':
    main()
