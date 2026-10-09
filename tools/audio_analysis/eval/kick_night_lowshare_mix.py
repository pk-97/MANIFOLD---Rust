#!/usr/bin/env python3
"""Does the stem's kick-versus-impostor low-energy share survive in the full mix?

On the isolated drum stem, Bad Guy's backbeat claps put ~5% of their first-40 ms
energy below 140 Hz and its kicks ~38%. This measures the same share on the mix.

Declared before looking:
- Share: 30-140 Hz energy over 30-8000 Hz energy, window -5..+40 ms around the
  candidate onset (the energies() rule of kick_night_drum_onset_shape.py), causal
  filtering, inside the 42.6 ms evidence window.
- Kicks: each scored label's best-scoring timely H22 candidate. Impostors: (A) the
  real H22 nested false fires; (B) the oracle-90% extras by refined class.
- Separation: within-song AUC (kick share above impostor share), mix and, for the
  original five, the drum stem; songs with < 5 impostors in a set are not scored.
- Holds in the mix: mix AUC >= 0.75 and within 0.05 of the stem AUC wherever a
  stem exists. Adds information only if it beats the frozen body_low_log_balance
  feature on the same candidates.
"""
from __future__ import annotations

import json
import pickle
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[3]))

import numpy as np  # noqa: E402

from tools.audio_analysis.eval.kick_attack_rejection import read_audio  # noqa: E402
from tools.audio_analysis.eval.kick_night_common import NIGHT, TRACKS, Data  # noqa: E402
from tools.audio_analysis.eval.kick_night_diagnose import auc, emission_times, scored_labels  # noqa: E402
from tools.audio_analysis.eval.kick_night_drum_onset_shape import energies  # noqa: E402
from tools.audio_analysis.eval.kick_night_oracle_labels import ORIGINAL_FIVE, mono  # noqa: E402
from tools.audio_analysis.eval.run_kick_night_h22 import CONFIGS, load_inputs, make_h22  # noqa: E402

TOL = .070
BALANCE = 6  # body_low_log_balance: log(max 140-400 Hz / max 45-140 Hz); lower is more kick-like


def share(x, sr, t):
    e = energies(x, sr, t)
    return (e['sub'] + e['low']) / (sum(e.values()) + 1e-20)


def nearest_candidate(em, t):
    i = int(np.argmin(np.abs(em - t)))
    # Case times are stored rounded to 1 ms; hops are 5.3 ms apart.
    return i if abs(em[i] - t) < 6e-4 else None


def main():
    d = Data()
    glide, act = load_inputs()
    h22, _ = make_h22(d, glide, act, *CONFIGS['h18_gate_glide_lowfit'])
    with open(NIGHT / 'h22' / 'h18_gate_glide_lowfit.pkl', 'rb') as f:
        _, nested_by_track, _ = pickle.load(f)
    oracle = json.loads((NIGHT / 'oracle_labels.json').read_text())
    report = {}
    for t in TRACKS:
        r = d.records[t]
        hop, sr0 = r['hop'], r['sample_rate']
        em = emission_times(r)
        onset = np.asarray(r['candidates']) * hop / sr0
        lg = h22(t, t)
        feats = d.features(t)
        msr, mix = read_audio(r['source']['audio_path'])
        mix = mono(mix)
        stem = None
        if t in ORIGINAL_FIVE:
            ssr, stem = read_audio(str(Path('/Users/peterkiemann/MANIFOLD - Rust/tests/fixtures/audio') / t / 'drums.wav'))
            stem = mono(stem)
        kick_idx = []
        for _, lab in scored_labels(r['source'])[0]:
            near = np.flatnonzero(np.abs(em - lab) <= TOL)
            if len(near):
                kick_idx.append(int(near[np.argmax(lg[near])]))
        sets = {'A_nested_false_fires': [], }
        for p in nested_by_track[t]:
            for e in p['accuracy_by_tolerance_ms']['70']['extra_times_s']:
                i = nearest_candidate(em, e)
                if i is not None:
                    sets['A_nested_false_fires'].append(i)
        for c in oracle[t]['extra_cases']:
            i = nearest_candidate(em, c['time_s'])
            if i is not None:
                sets.setdefault(f"B_{c['cls_refined']}", []).append(i)

        def measure(idx):
            out = dict(mix=[share(mix, msr, onset[i]) for i in idx], balance=[float(feats[i, BALANCE]) for i in idx])
            if stem is not None:
                out['stem'] = [share(stem, ssr, onset[i]) for i in idx]
            return out
        kicks = measure(kick_idx)
        row = dict(kicks=len(kick_idx), kick_mix_share_median=round(float(np.median(kicks['mix'])), 3),
                   kick_stem_share_median=round(float(np.median(kicks['stem'])), 3) if stem is not None else None, sets={})
        for name, idx in sets.items():
            if len(idx) < 5:
                continue
            imp = measure(idx)
            row['sets'][name] = dict(
                n=len(idx), mix_share_median=round(float(np.median(imp['mix'])), 3),
                stem_share_median=round(float(np.median(imp['stem'])), 3) if stem is not None else None,
                auc_mix=round(auc(np.array(kicks['mix']), np.array(imp['mix'])), 3),
                auc_stem=round(auc(np.array(kicks['stem']), np.array(imp['stem'])), 3) if stem is not None else None,
                auc_balance=round(auc(-np.array(kicks['balance']), -np.array(imp['balance'])), 3))
        report[t] = row
        print(t, 'kicks', row['kicks'], 'mix share', row['kick_mix_share_median'], 'stem', row['kick_stem_share_median'], flush=True)
        for name, s in row['sets'].items():
            print(f"   {name:34s} n {s['n']:4d} share mix {s['mix_share_median']} stem {s['stem_share_median']} | "
                  f"AUC mix {s['auc_mix']} stem {s['auc_stem']} existing-balance {s['auc_balance']}", flush=True)
    (NIGHT / 'lowshare_mix.json').write_text(json.dumps(report, indent=1))


if __name__ == '__main__':
    main()
