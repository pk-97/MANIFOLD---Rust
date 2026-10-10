#!/usr/bin/env python3
"""Export frozen H18 component logits for every outer and inner fold, once.

Night research (2026-10-10) reuses these arrays instead of refitting. Nothing is
fitted, no audio is read, and the evening caches are only read: the H17/H18
prediction pairs are copied into the night cache before use.
"""
from __future__ import annotations

import hashlib
import json
import shutil
import sys
from pathlib import Path
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT))

import numpy as np  # noqa: E402
from scipy.special import logit  # noqa: E402

from tools.audio_analysis.eval import kick_trajectory_features as cached  # noqa: E402
from tools.audio_analysis.eval import run_kick_trajectory_trial as runner  # noqa: E402
from tools.audio_analysis.eval import run_kick_fusion_trial as linear  # noqa: E402
from tools.audio_analysis.eval.kick_threeway_blend import FrozenFitter, PredictionCache  # noqa: E402
from tools.audio_analysis.eval.kick_interaction_score import basis  # noqa: E402
from tools.audio_analysis.eval.kick_training_coverage import add_training_coverage  # noqa: E402
from tools.audio_analysis.eval.run_kick_shape_trial import append_reviews  # noqa: E402

CACHE = Path.home() / '.cache/manifold'
EVENING = CACHE / 'kick-research-2026-10-09-evening'
NIGHT = CACHE / 'kick-research-2026-10-10-night'
AUDIO = Path('/Users/peterkiemann/MANIFOLD - Rust/tests/fixtures/audio')


def sha(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


REPO = Path(__file__).resolve().parents[3]


def checkout_path(path):
    """A frozen path inside some worktree, read from this checkout instead: worktree
    slots get reclaimed, and the sha256 check still pins the content."""
    parts = Path(path).parts
    if '.claude' in parts and parts[parts.index('.claude') + 1:][:1] == ('worktrees',):
        return REPO.joinpath(*parts[parts.index('.claude') + 3:])
    return Path(path)


def load_inputs():
    rule = json.loads((EVENING / 'h18/rule.json').read_text())
    inputs = {}
    for name, item in rule['inputs'].items():
        path = checkout_path(item['path'])
        if sha(path) != item['sha256']:
            raise ValueError(f'frozen input changed: {name}')
        inputs[name] = json.loads(path.read_text())
    return inputs


def load_records(inputs):
    def forbidden(*a, **k):
        raise RuntimeError('no audio or extraction allowed')
    with patch.object(cached, 'read_audio', side_effect=forbidden), \
            patch.object(cached, 'fusion_features', side_effect=forbidden):
        records = runner.load_records(AUDIO, inputs['baseline'], CACHE / 'kick-trajectory-2026-10-09/features')
    records, _ = append_reviews(records, inputs['expanded'])
    original = [dict(r, features=r['features'][:, :15]) for r in records]
    records, _ = add_training_coverage(original, inputs['additional'])
    return original, records


def main():
    NIGHT.mkdir(parents=True, exist_ok=True)
    pairs = NIGHT / 'h18_pairs_copy'
    if not pairs.exists():
        shutil.copytree(EVENING / 'h18/pairs', pairs)
    inputs = load_inputs()
    original, records = load_records(inputs)
    h10, h16 = inputs['h10'], inputs['h16_compact']
    baseline = next(v for v in inputs['previous']['variants'] if v['variant'] == 'linear_15')
    covered = next(v for v in h10['variants'] if v['variant'] == 'coverage_linear15')
    interaction = next(v for v in h10['variants'] if v['variant'] == 'coverage_interaction15')
    fitter = FrozenFitter(covered, h16['variants'][-1], interaction, EVENING / 'h16/models')
    predict = PredictionCache(pairs)

    def components(model, x):
        a, b = predict.pair.logits(model['base'], x)
        inter = model['interaction']
        c = basis(x, inter['mean'], inter['scale']) @ np.asarray(inter['weights']) + inter['intercept']
        return np.asarray(a), np.asarray(b), np.asarray(c)

    tracks = [r['track'] for r in records]
    base_rows = {t['track']: t for t in baseline['tracks']}
    arrays, meta = {}, dict(tracks=tracks, per_track={}, inner_pairs=[])
    for record in records:
        t = record['track']
        x = np.asarray(record['features'], dtype=np.float64)
        model = fitter.fit(records, t, .25)
        a, b, c = components(model, x)
        arrays[f'{t}|outer|linear'], arrays[f'{t}|outer|kernel'], arrays[f'{t}|outer|interaction'] = a, b, c
        row = base_rows[t]
        p0 = linear.predict_score(row['model'], x)
        arrays[f'{t}|outer|baseline'] = logit(np.clip(p0, 1e-15, 1 - 1e-15))
        arrays[f'{t}|features'] = x
        arrays[f'{t}|available'] = np.asarray(record['available'])
        arrays[f'{t}|candidates'] = np.asarray(record['candidates'])
        meta['per_track'][t] = dict(sample_rate=record['sample_rate'], hop=record['hop'],
                                    baseline_refined_threshold=row['refinement']['threshold'] if 'refinement' in row else None,
                                    baseline_calibrated_threshold=row['calibration']['threshold'])
        inner_records = [r for r in records if r['track'] != t]
        base_inner = {f['validation_track']: f['model'] for f in row['calibration']['inner_folds']}
        for inner in inner_records:
            u = inner['track']
            xi = np.asarray(inner['features'], dtype=np.float64)
            model = fitter.fit(inner_records, u, .25)
            a, b, c = components(model, xi)
            arrays[f'{t}|{u}|linear'], arrays[f'{t}|{u}|kernel'], arrays[f'{t}|{u}|interaction'] = a, b, c
            p0 = linear.predict_score(base_inner[u], xi)
            arrays[f'{t}|{u}|baseline'] = logit(np.clip(p0, 1e-15, 1 - 1e-15))
            meta['inner_pairs'].append([t, u])
        print('dumped', t, flush=True)
    np.savez(NIGHT / 'logits.npz', **arrays)
    meta['statistics'] = predict.statistics()
    meta['logits_sha256'] = sha(NIGHT / 'logits.npz')
    meta['source_sha256'] = sha(__file__)
    (NIGHT / 'logits_meta.json').write_text(json.dumps(meta, indent=1) + '\n')
    print(json.dumps(meta['statistics']))


if __name__ == '__main__':
    main()
