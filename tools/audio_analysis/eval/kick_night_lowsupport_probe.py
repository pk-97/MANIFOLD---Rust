#!/usr/bin/env python3
"""Diagnostic: is glide's direction consistent inside the kernel's low-support zone?

Per song, AUC of each glide value for labelled kicks versus non-kicks among
candidates whose outer-fold kernel activity is below 5 and whose H18 logit is
within 1.0 of its cutoff or above. AUC < 0.5 for glide_slope means kicks sweep
downward (the physical expectation).
"""
from __future__ import annotations

import pickle
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[3]))

import numpy as np  # noqa: E402

from tools.audio_analysis.eval.kick_glide_features import GLIDE_NAMES  # noqa: E402
from tools.audio_analysis.eval.kick_night_common import NIGHT, TRACKS, Data, h18_logit  # noqa: E402
from tools.audio_analysis.eval.kick_night_diagnose import auc  # noqa: E402


def main():
    d = Data()
    with np.load(NIGHT / 'glide.npz') as z:
        glide = {k: z[k] for k in z.files}
    with np.load(NIGHT / 'kernel_activity.npz') as z:
        act = {k: z[k] for k in z.files}
    with open(NIGHT / 'replay.pkl', 'rb') as f:
        cuts = pickle.load(f)['h18'][0]['cutoffs']
    for zone in ('low', 'high'):
        print('== zone', zone)
        for t in TRACKS:
            r = d.records[t]
            y, m = np.asarray(r['labels']), np.asarray(r['training_mask'], bool)
            lg = h18_logit(d, t, t)
            cut = np.log(cuts[t] / (1 - cuts[t]))
            a = act[f'{t}|{t}']
            sel = m & (lg >= cut - 1) & ((a < 5) if zone == 'low' else (a >= 5))
            pos, neg = sel & (y == 1), sel & (y == 0)
            row = [f"{t[:12]:12s} pos {pos.sum():3d} neg {neg.sum():4d}"]
            for i, n in enumerate(GLIDE_NAMES):
                row.append(f"{n} {auc(glide[t][pos, i], glide[t][neg, i]):.2f}")
            row.append(f"linear {auc(d.logits(t, t, 'linear')[pos], d.logits(t, t, 'linear')[neg]):.2f}")
            print('  ', ' | '.join(row))


if __name__ == '__main__':
    main()
