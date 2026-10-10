#!/usr/bin/env python3
"""H23: an early firing path for confident kicks, H18 for everything else.

Declared before scoring (rule written to the night cache first):
- Why: emission is ~45 ms after the attack because every candidate waits for
  42.6 ms of evidence; 35 ms matching is impossible and visuals lag the kick.
- Fast scorer: the frozen 15 features over a shorter evidence window, a weighted
  logistic model (songs/classes equal, l2 0.01) fitted per fold on training
  songs only. Slow path: H18 .25 with its frozen nested cutoff per outer song.
- Policy: in availability order, a candidate fires at its fast availability if
  the fast probability passes T_fast, else at its slow availability if H18
  passes; one 60 ms refractory covers both paths, so a fast fire suppresses the
  same kick's slow decision. Never backdated.
- T_fast per outer song: chosen on its eight inner songs only, maximising 35 ms
  matches subject to wide extras and kick-free fires no higher than H18 alone
  on the same inner songs.
- Configurations: evidence 15 / 20 / 25 ms (3 / 4 / 5 hops at 48 kHz).
- Expected failure: one bass cycle or less of evidence makes fast scores weak,
  so few kicks qualify without new extras.
- Acceptance (timing): 70 ms within 2 matches and 0 extras of H18, cores clear,
  and 50 ms matches up >= 20 or median delay down >= 10 ms.
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
from scipy.special import expit  # noqa: E402

from tools.audio_analysis.eval.kick_attack_rejection import read_audio  # noqa: E402
from tools.audio_analysis.eval.kick_fast_features import fast_features  # noqa: E402
from tools.audio_analysis.eval.kick_fusion_calibration import REFRACTORY_SECONDS, calibration_counts  # noqa: E402
from tools.audio_analysis.eval.kick_night_common import NIGHT, TRACKS, Data, h18_logit, summarise  # noqa: E402
from tools.audio_analysis.eval.kick_night_stack import apply_stack, fit_stack  # noqa: E402
from tools.audio_analysis.eval.run_kick_dsp_experiments import evaluate  # noqa: E402

RULE = dict(hypothesis='H23', l2=.01, evidence_s=[.015, .020, .025],
            selection='T_fast maximises inner 35 ms matches with inner wide extras and kick-free fires <= H18 alone',
            acceptance='70 ms within 2 matches and 0 extras of H18, cores clear, 50 ms +20 or median delay -10 ms',
            expected_failure='weak fast scores; few kicks qualify without new extras')
T_GRID = tuple(np.round(np.concatenate([np.linspace(.5, .95, 10), np.linspace(.96, .999, 14), [1.01]]), 4))


def features(d, evidence):
    path = NIGHT / f'fast_{int(evidence * 1000)}ms.npz'
    if path.exists():
        with np.load(path) as z:
            return {k: z[k] for k in z.files}
    out = {}
    for t in TRACKS:
        r = d.records[t]
        sr, x = read_audio(r['source']['audio_path'])
        c0 = time.process_time()
        cand, avail, feats, hop = fast_features(x, sr, evidence)
        cpu = time.process_time() - c0
        idx = {int(c): i for i, c in enumerate(cand)}
        rows = np.array([idx[int(c)] for c in r['candidates']])
        out[f'{t}|features'] = feats[rows, :15]
        out[f'{t}|available'] = avail[rows]
        out[f'{t}|cpu'] = np.array([cpu, len(x) / sr])
        print('fast features', t, round(cpu, 2), 's', flush=True)
    np.savez(path, **out)
    return out


def combined(record, fast_p, fast_av, slow_p, slow_av, t_fast, t_slow):
    sr, hop = record['sample_rate'], record['hop']
    events = sorted([(int(a), 0, p) for a, p in zip(fast_av, fast_p)] + [(int(a), 1, p) for a, p in zip(slow_av, slow_p)])
    last, fires, kinds = -np.inf, [], []
    for a, kind, p in events:
        if p >= (t_fast if kind == 0 else t_slow) and (a - last) * hop / sr >= REFRACTORY_SECONDS - 1e-12:
            fires.append(a); kinds.append(kind); last = a
    times = [(a + 1) * hop / sr for a in fires]
    return evaluate(record['source'], times), kinds


def main():
    out = NIGHT / 'h23'
    out.mkdir(exist_ok=True)
    rule_path = out / 'rule.json'
    if not rule_path.exists():
        rule_path.write_text(json.dumps(RULE, indent=1))
    if json.loads(rule_path.read_text()) != RULE:
        raise ValueError('predeclared rule differs')
    d = Data()
    # Sanity: the shim at 40 ms must reproduce the cached frozen features.
    probe = d.records['apricots_128bpm']
    sr, x = read_audio(probe['source']['audio_path'])
    c, a, f, _ = fast_features(x, sr, .040)
    if not (np.array_equal(c, probe['candidates']) and np.allclose(f[:, :15], d.features('apricots_128bpm'), atol=1e-12)):
        raise ValueError('40 ms shim does not reproduce frozen features')
    with open(NIGHT / 'replay.pkl', 'rb') as fh:
        replay = pickle.load(fh)
    h18_cut = replay['h18'][0]['cutoffs']
    report = dict(rule=RULE, rule_sha256=hashlib.sha256(rule_path.read_bytes()).hexdigest(), variants=[])
    for ev in RULE['evidence_s']:
        fz = features(d, ev)
        models = {}

        def fast_model(o, t):
            key = (o, frozenset({o, t}))
            if key not in models:
                xs, ys, ss = [], [], []
                for u in TRACKS:
                    if u in (o, t):
                        continue
                    r = d.records[u]
                    m = np.array(r['training_mask'], dtype=bool)
                    xs.append(fz[f'{u}|features'][m]); ys.append(np.asarray(r['labels'])[m]); ss.append(np.full(m.sum(), TRACKS.index(u)))
                x_ = np.concatenate(xs)
                models[key] = fit_stack(np.zeros(len(x_)), x_, np.concatenate(ys).astype(float), np.concatenate(ss), RULE['l2'])
            return models[key]

        def fast_p(o, t):
            return expit(apply_stack(fast_model(o, t), 0.0, fz[f'{t}|features']))

        by_track, chosen, fast_share = {}, {}, {}
        for o in TRACKS:
            inner = [u for u in TRACKS if u != o]
            slow = {u: expit(h18_logit(d, o, u)) for u in inner}
            ref = calibration_counts([p for u in inner for p in combined(d.records[u], np.zeros(1), [0], slow[u],
                                                                         d.records[u]['available'], 2.0, h18_cut[o])[0]])
            best = None
            for tf in T_GRID:
                ps = [p for u in inner for p in combined(d.records[u], fast_p(o, u), fz[f'{u}|available'], slow[u],
                                                         d.records[u]['available'], tf, h18_cut[o])[0]]
                c = calibration_counts(ps)
                m35 = sum(p['accuracy_by_tolerance_ms']['35']['matched'] for p in ps)
                if c['wide_extras'] <= ref['wide_extras'] and c['kick_free_extras'] <= ref['kick_free_extras']:
                    if best is None or m35 > best[1]:
                        best = (tf, m35)
            chosen[o] = best[0] if best else 1.01
            r = d.records[o]
            passages, kinds = combined(r, fast_p(o, o), fz[f'{o}|available'], expit(h18_logit(d, o, o)), r['available'],
                                       chosen[o], h18_cut[o])
            by_track[o] = passages
            fast_share[o] = (sum(k == 0 for k in kinds), len(kinds))
        s = summarise(by_track)
        cpu = sum(float(fz[f'{t}|cpu'][0]) for t in TRACKS) / sum(float(fz[f'{t}|cpu'][1]) for t in TRACKS)
        h = replay['h18'][0]
        acc = dict(within_70=s['tol']['70']['matched'] >= h['tol']['70']['matched'] - 2 and s['tol']['70']['extra'] <= h['tol']['70']['extra'],
                   cores_clear=s['kick_free_extras'] == 0,
                   timing=s['tol']['50']['matched'] - h['tol']['50']['matched'] >= 20 or h['delay_p50'] - s['delay_p50'] >= 10)
        acc['passes'] = all(acc.values())
        report['variants'].append(dict(evidence_s=ev, summary=s, acceptance=acc, t_fast=chosen, fast_fires=fast_share,
                                       fast_feature_cpu_share=cpu))
        with open(out / f'ev{int(ev * 1000)}.pkl', 'wb') as fh:
            pickle.dump((s, by_track), fh)
        print(ev, {k: (v['matched'], v['extra']) for k, v in s['tol'].items()}, 'cores', s['kick_free_extras'],
              'delay', round(s['delay_p50'], 1), round(s['delay_p90'], 1), 'fast fires', sum(v[0] for v in fast_share.values()),
              'of', sum(v[1] for v in fast_share.values()), 'cpu', round(100 * cpu, 2), '%', acc, flush=True)
        print('   t_fast', chosen, flush=True)
    (out / 'trial.json').write_text(json.dumps(report, indent=1, default=str))


if __name__ == '__main__':
    main()
