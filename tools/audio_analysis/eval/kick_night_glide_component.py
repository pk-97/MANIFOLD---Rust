#!/usr/bin/env python3
"""Component test of the glide measurement before it enters any scorer.

Declared before running: glide is worth a scorer hypothesis only if, among the
candidates the frozen H18 score finds hard (outer logit within 1.0 of its nested
cutoff or above it), at least one glide value separates labelled kicks from
non-kicks with song-balanced AUC >= 0.65 and does so in Bad Guy or Midnight,
where the existing 15 values fail. Expected failure: kick-over-bass mixtures
blur zero crossings, so glide may only work where the kick dominates the band.
Extracts and caches glide values for all 43,585 candidates.
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

from tools.audio_analysis.eval.kick_attack_rejection import read_audio  # noqa: E402
from tools.audio_analysis.eval.kick_glide_features import GLIDE_NAMES, glide_features  # noqa: E402
from tools.audio_analysis.eval.kick_night_common import NIGHT, TRACKS, Data, h18_logit  # noqa: E402
from tools.audio_analysis.eval.kick_night_diagnose import auc  # noqa: E402


def sha(p):
    return hashlib.sha256(Path(p).read_bytes()).hexdigest()


def extract(d):
    path = NIGHT / 'glide.npz'
    src = sha(Path(__file__).parent / 'kick_glide_features.py')
    meta_path = NIGHT / 'glide_meta.json'
    if path.exists() and json.loads(meta_path.read_text())['source_sha256'] == src:
        with np.load(path) as z:
            return {k: z[k] for k in z.files}, json.loads(meta_path.read_text())
    arrays, meta = {}, dict(source_sha256=src, cpu_s={}, audio_s={})
    for t in TRACKS:
        r = d.records[t]
        audio = r['source']['audio_path']
        if sha(audio) != r['source']['audio_sha256']:
            raise ValueError(f'audio differs: {t}')
        sr, x = read_audio(audio)
        if sr != r['sample_rate']:
            raise ValueError('sample rate differs')
        c0 = time.process_time()
        arrays[t] = glide_features(x, sr, r['hop'], np.asarray(r['candidates']), np.asarray(r['available']))
        meta['cpu_s'][t] = time.process_time() - c0
        meta['audio_s'][t] = len(x) / sr
        print('glide', t, round(meta['cpu_s'][t], 2), 's for', round(len(x) / sr, 1), 's audio', flush=True)
    np.savez(path, **arrays)
    meta['data_sha256'] = sha(path)
    meta_path.write_text(json.dumps(meta, indent=1))
    return arrays, meta


def main():
    d = Data()
    g, meta = extract(d)
    with open(NIGHT / 'replay.pkl', 'rb') as f:
        replay = pickle.load(f)
    cuts = replay['h18'][0]['cutoffs']
    names = list(GLIDE_NAMES) + ['centroid_drop', 'low_centroid_drop', 'body_log_rise']
    cols = {'centroid_drop': 4, 'low_centroid_drop': 10, 'body_log_rise': 1}
    report = {}
    for t in TRACKS:
        r = d.records[t]
        y, m = np.asarray(r['labels']), np.asarray(r['training_mask'], dtype=bool)
        lg = h18_logit(d, t, t)
        cut = np.log(cuts[t] / (1 - cuts[t]))
        hard = m & (lg >= cut - 1.0)
        vals = {n: g[t][:, i] for i, n in enumerate(GLIDE_NAMES)}
        vals.update({n: d.features(t)[:, c] for n, c in cols.items()})
        row = dict(pos_all=int(np.sum(y[m] == 1)), neg_all=int(np.sum(y[m] == 0)),
                   pos_hard=int(np.sum(y[hard] == 1)), neg_hard=int(np.sum(y[hard] == 0)))
        for n in names:
            v = vals[n]
            row[f'{n}_auc_all'] = round(auc(v[m & (y == 1)], v[m & (y == 0)]), 3)
            row[f'{n}_auc_hard'] = round(auc(v[hard & (y == 1)], v[hard & (y == 0)]), 3)
            row[f'{n}_pos_med'] = round(float(np.median(v[m & (y == 1)])), 3) if row['pos_all'] else None
            row[f'{n}_neg_hard_med'] = round(float(np.median(v[hard & (y == 0)])), 3) if row['neg_hard'] else None
        report[t] = row
        print(f"{t[:18]:18s} pos {row['pos_hard']:3d}/{row['pos_all']:3d} neg_hard {row['neg_hard']:4d} | "
              + ' '.join(f"{n}:{row[f'{n}_auc_hard']}" for n in names))
    bal = {n: round(float(np.nanmean([report[t][f'{n}_auc_hard'] for t in TRACKS])), 3) for n in names}
    print('song-balanced hard AUC:', bal)
    for n in GLIDE_NAMES:
        print(n, 'pos median by song', {t[:6]: report[t][f'{n}_pos_med'] for t in TRACKS})
        print(n, 'hard-neg median   ', {t[:6]: report[t][f'{n}_neg_hard_med'] for t in TRACKS})
    (NIGHT / 'glide_component.json').write_text(json.dumps(dict(meta=meta, report=report, balanced_hard_auc=bal), indent=1))


if __name__ == '__main__':
    main()
