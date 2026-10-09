"""Evaluate three fixed blends of cached nested linear15/depth3 models; no new fit."""
from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import time
from unittest.mock import patch

from . import kick_trajectory_features as cached
from . import run_kick_trajectory_trial as runner
from .kick_anchored_score import FrozenFitter, TREE_WEIGHTS, predict_score
from .kick_attack_rejection import sha
from .kick_fusion_bandwise import FEATURE_NAMES
from .run_kick_boosted_trial import replay_reference, score_diagnostics
from .run_kick_dsp_experiments import ROOT
from .run_kick_fusion_calibration import comparisons
from .run_kick_shape_trial import add_cohort_results, append_reviews


def add_diagnostics(variant, records, linear):
    before = {r['track']:r for r in linear['tracks']}
    cores, deltas = [], []
    for row, record in zip(variant['tracks'], records):
        row['score_distribution'] = score_diagnostics(record, predict_score(row['model'], record['features']), row)
        difference = comparisons(before[row['track']]['scores']['refined'], row['scores']['refined'])
        row['comparison_to_linear15'] = difference
        lost = sum(len(p['metrics']['70']['lost_labels_s']) for p in difference)
        recovered = sum(len(p['metrics']['70']['recovered_labels_s']) for p in difference)
        row['linear15_delta'] = dict(lost_labels70=lost, recovered_labels70=recovered,
                                    net_matched70=recovered-lost)
        deltas.append(dict(track=row['track'], **row['linear15_delta']))
        for p in row['scores']['refined']:
            if p['labels'] == 0:
                cores.append(dict(track=row['track'], passage=p['id'],
                    extra70=p['accuracy_by_tolerance_ms']['70']['extra'],
                    extra_times_s=p['accuracy_by_tolerance_ms']['70']['extra_times_s']))
        horizon = (record['available']-record['candidates'])*record['hop']/record['sample_rate']*1000
        delays = [pair['delay_ms'] for p in row['scores']['refined']
                  for pair in p['association_early_35_late_200_ms']['pairs']]
        row['timing'] = dict(candidate_observation_min_ms=float(horizon.min()),
            candidate_observation_max_ms=float(horizon.max()), emitted_at_completed_availability_hop=True,
            wide_associated_later_than70ms=sum(t > 70 for t in delays))
        row['cost'] = dict(candidates=len(record['features']),
            prediction_us_per_candidate=row['prediction_cpu_s']/len(record['features'])*1e6,
            max_tree_comparisons=192, linear_feature_products=15,
            inference='Offline vectorised Python, not a native callback guarantee; no feature extraction rerun.')
    if len(cores) != 9:
        raise ValueError('expected exactly nine kick-free cores')
    variant['kick_free_cores'], variant['linear15_track_deltas'] = cores, deltas
    total = variant['totals']['refined']['tolerance_ms']['70']
    variant['acceptance'] = dict(count_target_met=(total['matched'] >= 223 and total['extra'] <= 70)
        or (total['matched'] >= 250 and total['extra'] <= 89),
        nine_kick_free_cores_zero=all(c['extra70'] == 0 for c in cores),
        at_most_one_lost_label_per_track=all(d['lost_labels70'] <= 1 for d in deltas),
        at_most_one_net_lost_match_per_track=all(d['net_matched70'] >= -1 for d in deltas))
    variant['acceptance']['intermediate_target_met'] = all(variant['acceptance'][k] for k in
        ('count_target_met', 'nine_kick_free_cores_zero', 'at_most_one_lost_label_per_track'))
    variant['prediction_cpu_s'] = sum(t['prediction_cpu_s'] for t in variant['tracks'])


def run(audio_root, baseline_path, cache, expanded_path, linear_path, boosted_path, rule_path, out):
    for variable in ('OPENBLAS_NUM_THREADS', 'VECLIB_MAXIMUM_THREADS', 'OMP_NUM_THREADS'):
        if os.environ.get(variable) != '1':
            raise ValueError(f'{variable}=1 required')
    baseline, expanded, linear_report, tree_report, rule = [json.loads(p.read_text())
        for p in (baseline_path, expanded_path, linear_path, boosted_path, rule_path)]
    if rule['status'] != 'predeclared' or rule['configurations'] != [
            dict(tree_logit_weight=w, linear_logit_weight=1-w, depth=3) for w in TREE_WEIGHTS]:
        raise ValueError('unexpected anchored experiment rule')
    for report in (linear_report, tree_report):
        for name, digest in report['source_sha256'].items():
            if sha(ROOT/'tools/audio_analysis/eval'/name) != digest:
                raise ValueError(f'cached component source changed: {name}')
    if sha(expanded_path) != linear_report['expanded_review_sha256']:
        raise ValueError('expanded labels changed')
    def forbidden(*args, **kwargs):
        raise RuntimeError('existing feature caches required; no decoding or extraction')
    with patch.object(cached, 'read_audio', side_effect=forbidden), \
         patch.object(cached, 'fusion_features', side_effect=forbidden):
        records = runner.load_records(audio_root, baseline, cache)
    records, coverage = append_reviews(records, expanded)
    records = [dict(r, features=r['features'][:, :15]) for r in records]
    linear = next(v for v in linear_report['variants'] if v['variant'] == 'linear_15')
    tree = next(v for v in tree_report['variants'] if v['variant'] == 'boosted_depth3')
    replay_reference(records, linear)
    fitter = FrozenFitter(linear, tree)
    out.parent.mkdir(parents=True, exist_ok=True)
    report = dict(hypothesis=rule, rule_sha256=sha(rule_path), baseline_sha256=sha(baseline_path),
        linear_report_sha256=sha(linear_path), boosted_report_sha256=sha(boosted_path),
        expanded_sha256=sha(expanded_path), original_label_sha256=baseline['label_sha256'],
        source_sha256={n:sha(ROOT/'tools/audio_analysis/eval'/n) for n in
            ('kick_anchored_score.py', 'run_kick_anchored_trial.py', 'test_kick_anchored_score.py')},
        upstream_source_sha256=tree_report['source_sha256'], baseline_replay_exact=True,
        cached_training_sets=len(fitter.tree), new_fits=0, feature_names=list(FEATURE_NAMES),
        coverage=coverage, variants=[], fixed='Exact nested cached linear15 and depth3 components, '
            'fixed logit weights; existing candidates,42.63–42.67ms actual horizon,60ms refractory, '
            'coarse threshold selection and one refinement. No changed labels or fitting.',
        limitations='Exploratory nine-song development only. No native benchmark, listening claim, '
            'live integration, reserved confirmation or calibrated probability interpretation.')
    for weight in TREE_WEIGHTS:
        started = time.monotonic()
        variant = runner.run_variant(records, f'anchored_tree_{weight:.2f}',
            lambda rows, held:fitter.fit(rows, held, weight), predict_score)
        add_cohort_results(variant, records)
        add_diagnostics(variant, records, linear)
        if variant['totals']['refined']['labels'] != 381 or variant['cohorts']['expanded']['refined']['labels'] != 207:
            raise ValueError('label cohort count changed')
        variant['evaluation_wall_s'] = time.monotonic()-started
        report['variants'].append(variant)
        report['cached_model_requests'] = fitter.requests
        report['complete'] = len(report['variants']) == len(TREE_WEIGHTS)
        out.write_text(json.dumps(report, indent=2)+'\n')
        print('TOTAL', variant['variant'], variant['totals']['refined'], variant['acceptance'], flush=True)
    return report


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('audio-root', 'baseline', 'cache', 'expanded', 'linear', 'boosted', 'rule', 'out'):
        parser.add_argument('--'+name, type=Path, required=True)
    args = parser.parse_args()
    run(args.audio_root, args.baseline, args.cache, args.expanded, args.linear, args.boosted, args.rule, args.out)
