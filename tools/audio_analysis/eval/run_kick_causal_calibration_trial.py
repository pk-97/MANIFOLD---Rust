"""Predeclared past-only score calibration on unchanged acoustic evidence."""
from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
from unittest.mock import patch

import numpy as np

from . import kick_trajectory_features as cached
from . import run_kick_trajectory_trial as runner
from .kick_attack_rejection import sha
from .kick_fusion_bandwise import FEATURE_NAMES
from .kick_causal_calibration import FoldFitter, STRENGTHS, predict_score
from .run_kick_boosted_trial import replay_reference, score_diagnostics
from .run_kick_dsp_experiments import ROOT
from .run_kick_fusion_calibration import comparisons
from .run_kick_shape_trial import append_reviews, add_cohort_results


def run(audio_root, baseline_path, cache, expanded_path, previous_path, rule_path, out):
    for name in ('OPENBLAS_NUM_THREADS', 'VECLIB_MAXIMUM_THREADS', 'OMP_NUM_THREADS'):
        if os.environ.get(name) != '1':
            raise ValueError(f'{name}=1 required')
    baseline, expanded, previous, rule = [json.loads(p.read_text()) for p in
        (baseline_path, expanded_path, previous_path, rule_path)]
    hypothesis = next(h for h in rule['hypotheses'] if h['id']=='H12')
    if [c['strength'] for c in hypothesis['configurations']] != list(STRENGTHS):
        raise ValueError('predeclaration differs')
    for name, digest in previous['source_sha256'].items():
        if sha(ROOT/'tools/audio_analysis/eval'/name) != digest:
            raise ValueError(f'frozen upstream source changed: {name}')
    if sha(expanded_path) != previous['expanded_review_sha256']:
        raise ValueError('expanded labels changed')
    def forbidden(*args, **kwargs):
        raise RuntimeError('cache-only experiment cannot decode audio')
    with patch.object(cached, 'read_audio', side_effect=forbidden), patch.object(cached, 'fusion_features', side_effect=forbidden):
        records = runner.load_records(audio_root, baseline, cache)
    records, coverage = append_reviews(records, expanded)
    records = [dict(r, features=r['features'][:, :15]) for r in records]
    reference = next(v for v in previous['variants'] if v['variant']=='linear_15')
    replay_reference(records, reference)
    records = [dict(r,features=np.column_stack((r["features"],(r["available"]+1)*r["hop"]/r["sample_rate"]))) for r in records]
    before = {r['track']:r for r in reference['tracks']}
    out.parent.mkdir(parents=True, exist_ok=True)
    fitter = FoldFitter(reference)
    report = dict(hypothesis=hypothesis, rule_sha256=sha(rule_path), baseline_sha256=sha(baseline_path),
        previous_sha256=sha(previous_path), expanded_sha256=sha(expanded_path),
        original_label_sha256=baseline['label_sha256'], upstream_source_sha256=previous['source_sha256'],
        source_sha256={n:sha(ROOT/'tools/audio_analysis/eval'/n) for n in
            ('kick_causal_calibration.py','run_kick_causal_calibration_trial.py','test_kick_causal_calibration.py')},
        feature_names=list(FEATURE_NAMES)+['emission_time_seconds_not_learned'], baseline_replay_exact=True, coverage=coverage, variants=[],
        limitations='Development songs only; provisional unchanged truth. Offsets use only prior candidate scores; training reference excludes the evaluated family. Scores are not probabilities. Eight seconds of bounded history adds no candidate observation delay; no reserved evaluation or live integration.')
    for strength in STRENGTHS:
        variant = runner.run_variant(records, f'causal_calibration_{strength:g}',
            lambda rows, held:fitter.fit(rows,held,strength), predict_score)
        add_cohort_results(variant, records)
        for row, record in zip(variant['tracks'], records):
            compared = comparisons(before[row['track']]['scores']['refined'], row['scores']['refined'])
            row['comparison_to_linear15'] = compared
            row['lost_linear15_labels70'] = sum(len(p['metrics']['70']['lost_labels_s']) for p in compared)
            scores = predict_score(row['model'], record['features'])
            row['score_distribution'] = score_diagnostics(record, scores, row)
            horizon = (record['available']-record['candidates'])*record['hop']/record['sample_rate']*1000
            row['timing'] = dict(candidate_observation_min_ms=float(horizon.min()),
                candidate_observation_max_ms=float(horizon.max()), emitted_at_completed_availability_hop=True)
            for model in [row['model']]+[f['model'] for f in row['calibration']['inner_folds']]:
                if row['track'] in model['training_tracks']:
                    raise ValueError('outer song leaked into fitting')
        counts = variant['totals']['refined']['tolerance_ms']['70']
        variant['acceptance'] = dict(count_target_met=(counts['matched']>=223 and counts['extra']<=70)
            or (counts['matched']>=250 and counts['extra']<=89), nine_kick_free_cores_zero=variant['kick_free_extras']==0,
            at_most_one_lost_label_per_track=all(r['lost_linear15_labels70']<=1 for r in variant['tracks']))
        variant['acceptance']['intermediate_target_met'] = all(variant['acceptance'].values())
        report['variants'].append(variant); report['fit_cache'] = fitter.statistics()
        report['complete'] = len(report['variants']) == len(STRENGTHS)
        out.write_text(json.dumps(report, indent=2)+'\n')
        print('TOTAL', variant['variant'], counts, variant['acceptance'], fitter.statistics(), flush=True)
    return report


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('audio-root','baseline','cache','expanded','previous','rule','out'):
        parser.add_argument('--'+name, type=Path, required=True)
    a = parser.parse_args()
    run(a.audio_root,a.baseline,a.cache,a.expanded,a.previous,a.rule,a.out)
