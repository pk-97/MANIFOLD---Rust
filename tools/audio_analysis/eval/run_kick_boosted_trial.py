"""Three predeclared boosted-tree configurations on frozen fifteen-feature caches."""
from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import time
from unittest.mock import patch

import numpy as np

from . import kick_trajectory_features as cached
from . import run_kick_trajectory_trial as runner
from .kick_attack_rejection import sha
from .kick_boosted_score import DEPTHS, FoldFitter, parameters, predict_score
from .kick_fusion_bandwise import FEATURE_NAMES
from .kick_fusion_calibration import THRESHOLDS, threshold_scores
from .run_kick_dsp_experiments import ROOT
from .run_kick_fusion_calibration import comparisons, summarise
from .run_kick_fusion_trial import predict_score as linear_predict
from .run_kick_shape_trial import add_cohort_results, append_reviews


def replay_reference(records, frozen):
    """Verify every expanded linear15 control event without refitting that control."""
    before = {r['track']: r for r in frozen['tracks']}
    if len(records) != 9 or {r['track'] for r in records} != set(before):
        raise ValueError('expected the exact nine development songs')
    for record in records:
        row = before[record['track']]
        scores = linear_predict(row['model'], record['features'])
        for name, cutoff in (('fixed_05', .5), ('calibrated', row['calibration']['threshold']),
                             ('refined', row['refinement']['threshold'])):
            fires, passages = threshold_scores(record, scores, cutoff)
            if fires != row['kick_hops'][name] or passages != row['scores'][name]:
                raise ValueError(f'expanded linear15 replay differs: {record["track"]}/{name}')


def score_diagnostics(record, scores, row):
    mask, labels = record['training_mask'], record['labels']
    cutoff = row['refinement']['threshold']

    def distribution(values):
        if not len(values):
            return dict(rows=0)
        return dict(rows=len(values), quantiles=np.quantile(values, [0, .01, .1, .5, .9, .99, 1]).tolist(),
            at_least_095=int(np.count_nonzero(values >= .95)),
            at_least_099=int(np.count_nonzero(values >= .99)),
            at_least_0999=int(np.count_nonzero(values >= .999)),
            at_most_0001=int(np.count_nonzero(values <= .001)),
            exactly_at_cutoff=int(np.count_nonzero(values == cutoff)),
            within_0001_of_cutoff=int(np.count_nonzero(np.abs(values-cutoff) <= .001)))

    trials = row['refinement']['threshold_trials']
    selected = row['refinement']['selected_inner_counts']
    equal = [r['threshold'] for r in trials if r['feasible']
             and (r['matched_70ms'], r['wide_extras']) ==
                 (selected['matched_70ms'], selected['wide_extras'])]
    positive = scores[mask & (labels == 1)]
    return dict(all_candidates=distribution(scores),
        reviewed_positive=distribution(positive), reviewed_negative=distribution(scores[mask & (labels == 0)]),
        threshold=dict(value=cutoff, coarse=row['calibration']['threshold'],
            at_lower_grid_boundary=cutoff == min(THRESHOLDS),
            silence_sentinel=cutoff > 1,
            above_outer_score_max=bool(len(scores) and cutoff > scores.max()),
            all_reviewed_positives_below_grid=bool(len(positive) and positive.max() < min(THRESHOLDS)),
            equivalent_inner_optimal_thresholds=equal,
            outer_scores_saturated_high=bool(np.count_nonzero(scores >= .999) > .01*len(scores)),
            outer_scores_saturated_low=bool(np.count_nonzero(scores <= .001) > .01*len(scores))))


def add_diagnostics(variant, records, baseline):
    previous = {r['track']: r for r in baseline['tracks']}
    cores, loss_rows = [], []
    for row, record in zip(variant['tracks'], records):
        scores = predict_score(row['model'], record['features'])
        row['score_distribution'] = score_diagnostics(record, scores, row)
        old = previous[row['track']]
        comparison = comparisons(old['scores']['refined'], row['scores']['refined'])
        lost = sum(len(p['metrics']['70']['lost_labels_s']) for p in comparison)
        recovered = sum(len(p['metrics']['70']['recovered_labels_s']) for p in comparison)
        row['comparison_to_linear15'] = comparison
        row['linear15_delta'] = dict(lost_labels70=lost, recovered_labels70=recovered,
                                    net_matched70=recovered-lost)
        loss_rows.append(dict(track=row['track'], **row['linear15_delta']))
        for passage in row['scores']['refined']:
            if passage['labels'] == 0:
                cores.append(dict(track=row['track'], passage=passage['id'],
                                  extra70=passage['accuracy_by_tolerance_ms']['70']['extra'],
                                  wide_extra=passage['association_early_35_late_200_ms']['extra']))
        delays = [p['delay_ms'] for passage in row['scores']['refined']
                  for p in passage['association_early_35_late_200_ms']['pairs']]
        horizon = (record['available']-record['candidates'])*record['hop']/record['sample_rate']*1000
        row['timing'] = dict(candidate_observation_min_ms=float(horizon.min()),
            candidate_observation_max_ms=float(horizon.max()),
            emitted_at_completed_availability_hop=True,
            wide_associated_later_than70ms=sum(d > 70 for d in delays),
            wide_associated_earlier_than_minus35ms=sum(d < -35 for d in delays))
        duration = record['cache_metadata']['duration_s']
        row['cost'] = dict(candidates=len(scores), recording_seconds=duration,
            independent_prediction_cpu_s=row['prediction_cpu_s'],
            independent_prediction_us_per_candidate=row['prediction_cpu_s']/len(scores)*1e6,
            independent_prediction_cpu_s_per_audio_second=row['prediction_cpu_s']/duration,
            trees=len(row['model']['trees']),
            max_split_comparisons_per_candidate=len(row['model']['trees'])*row['model']['max_depth'],
            serialized_model_bytes=len(json.dumps(row['model'], separators=(',', ':')).encode()))
    total = variant['totals']['refined']['tolerance_ms']['70']
    count_target = ((total['matched'] >= 223 and total['extra'] <= 70)
                    or (total['matched'] >= 250 and total['extra'] <= 89))
    if len(cores) != 9:
        raise ValueError(f'expected nine reviewed kick-free cores, found {len(cores)}')
    variant['kick_free_cores'] = cores
    variant['linear15_track_deltas'] = loss_rows
    variant['acceptance'] = dict(count_target_met=count_target,
        nine_kick_free_cores_zero=all(c['extra70'] == 0 for c in cores),
        at_most_one_lost_label_per_track=all(r['lost_labels70'] <= 1 for r in loss_rows),
        at_most_one_net_lost_match_per_track=all(r['net_matched70'] >= -1 for r in loss_rows))
    variant['acceptance']['intermediate_target_met'] = all(variant['acceptance'][k] for k in
        ('count_target_met', 'nine_kick_free_cores_zero', 'at_most_one_lost_label_per_track'))
    variant['prediction_cpu_s'] = sum(r['prediction_cpu_s'] for r in variant['tracks'])


def run(audio_root, baseline_path, cache, expanded_path, previous_path, manifest_path, out):
    for name in ('OPENBLAS_NUM_THREADS', 'VECLIB_MAXIMUM_THREADS', 'OMP_NUM_THREADS'):
        if os.environ.get(name) != '1':
            raise ValueError(f'{name}=1 is required for bounded CPU concurrency')
    baseline, expanded, previous, manifest = [json.loads(p.read_text())
        for p in (baseline_path, expanded_path, previous_path, manifest_path)]
    h1 = next(h for h in manifest['hypotheses'] if h['id'] == 'H1')
    declared = [dict(parameters(d)) for d in DEPTHS]
    for row in declared:
        row.pop('n_iter_no_change')
    if h1['configurations'] != declared:
        raise ValueError('manifest differs from fixed boosted configurations')
    for name, digest in previous['source_sha256'].items():
        if sha(ROOT/'tools/audio_analysis/eval'/name) != digest:
            raise ValueError(f'frozen upstream source changed: {name}')
    if sha(expanded_path) != previous['expanded_review_sha256']:
        raise ValueError('expanded annotation source changed')
    def forbidden(*args, **kwargs):
        raise RuntimeError('existing feature caches required; audio decode and feature extraction forbidden')
    with patch.object(cached, 'read_audio', side_effect=forbidden), \
         patch.object(cached, 'fusion_features', side_effect=forbidden):
        records = runner.load_records(audio_root, baseline, cache)
    records, coverage = append_reviews(records, expanded)
    records = [dict(r, features=r['features'][:, :15]) for r in records]
    frozen = next(v for v in previous['variants'] if v['variant'] == 'linear_15')
    replay_reference(records, frozen)
    out.parent.mkdir(parents=True, exist_ok=True)
    fitter = FoldFitter(out.parent/'models')
    report = dict(hypothesis=h1, manifest_sha256=sha(manifest_path),
        fixed='Frozen15 cached measurements, candidate grid,40ms horizon,60ms refractory, '
              'song/class/event weights, fold-only mean/scale,z clipping8; existing nested '
              'calibration and one refinement. No audio decode, FFT, extra configuration or reserved data.',
        baseline_sha256=sha(baseline_path), previous_sha256=sha(previous_path),
        expanded_sha256=sha(expanded_path), original_label_sha256=baseline['label_sha256'],
        source_sha256={name:sha(ROOT/'tools/audio_analysis/eval'/name) for name in
            ('kick_boosted_score.py', 'run_kick_boosted_trial.py', 'test_kick_boosted_score.py',
             'kick_subspace_score.py')}, upstream_source_sha256=previous['source_sha256'],
        baseline_replay_exact=True, feature_names=list(FEATURE_NAMES), coverage=coverage,
        variants=[], limitations='Exploratory nine-song development; provisional annotations. '
            'Balanced boosted scores are not event probabilities. Calibration retains the existing '
            '.5 lower grid boundary, with explicit boundary/saturation diagnostics. '
            'Python CPU throughput is not a native real-time guarantee; no feature-extraction '
            'benchmark rerun, live integration, listening claim or reserved confirmation.')
    for depth in DEPTHS:
        started = time.monotonic()
        variant = runner.run_variant(records, f'boosted_depth{depth}',
            lambda rows, held: fitter.fit(rows, held, depth), predict_score)
        add_cohort_results(variant, records)
        add_diagnostics(variant, records, frozen)
        if variant['cohorts']['expanded']['refined']['labels'] != 207:
            raise ValueError('expanded label count changed')
        variant['evaluation_wall_s'] = time.monotonic()-started
        report['variants'].append(variant)
        report['fit_cache'] = fitter.statistics()
        report['completed_configurations'] = [v['variant'] for v in report['variants']]
        report['complete'] = len(report['variants']) == len(DEPTHS)
        out.write_text(json.dumps(report, indent=2)+'\n')
        print('TOTAL', variant['variant'], variant['totals']['refined'],
              variant['acceptance'], fitter.statistics(), flush=True)
    return report


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('audio-root', 'baseline', 'cache', 'expanded', 'previous', 'manifest', 'out'):
        parser.add_argument('--'+name, type=Path, required=True)
    args = parser.parse_args()
    run(args.audio_root, args.baseline, args.cache, args.expanded, args.previous,
        args.manifest, args.out)
