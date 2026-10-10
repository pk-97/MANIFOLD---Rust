"""Recalibrate three frozen scorers with zero inner kick-free fires; no fitting."""
from __future__ import annotations

import argparse
import copy
import json
import os
from pathlib import Path
import time
from unittest.mock import patch

from . import kick_trajectory_features as cached
from . import run_kick_trajectory_trial as runner
from .kick_anchored_score import predict_score as anchored_predict
from .kick_attack_rejection import sha
from .kick_boosted_score import predict_score as tree_predict
from .kick_fusion_bandwise import FEATURE_NAMES
from .kick_fusion_calibration import THRESHOLDS, calibration_counts, choose_threshold, threshold_scores
from .kick_fusion_fine_calibration import refine_outer
from .run_kick_boosted_trial import score_diagnostics
from .run_kick_dsp_experiments import ROOT, evaluate
from .run_kick_fusion_calibration import comparisons, summarise
from .run_kick_fusion_trial import predict_score as linear_predict
from .run_kick_shape_trial import add_cohort_results, append_reviews

SCORERS = ('linear_15', 'boosted_depth3', 'anchored_075')


def zero_core_coarse(original):
    """Reuse observed coarse counts, changing only the inner core constraint."""
    coarse = copy.deepcopy(original)
    if [r['threshold'] for r in coarse['threshold_trials']] != list(THRESHOLDS):
        raise ValueError('cached calibration must contain the exact original coarse grid')
    coarse['original_inner_baseline'] = copy.deepcopy(coarse['inner_baseline'])
    coarse['previous_threshold'] = coarse['threshold']
    coarse['inner_baseline']['kick_free_extras'] = 0
    budget = coarse['inner_baseline']
    for row in coarse['threshold_trials']:
        row['feasible'] = row['wide_extras'] <= budget['wide_extras'] and row['kick_free_extras'] == 0
    chosen = choose_threshold(coarse['threshold_trials'], budget)
    coarse['threshold'] = chosen['threshold']
    coarse['selected_inner_counts'] = copy.deepcopy(chosen)
    return coarse


def verify_inner_counts(records, coarse, selected, predict, evaluator=evaluate):
    """Independently replay the selected point with outer data excluded entirely."""
    outer = coarse['outer_track']
    inner = {r['track']:r for r in records if r['track'] != outer}
    if set(inner) != set(coarse['threshold_selection_tracks']):
        raise ValueError('inner calibration songs changed')
    if {f['validation_track'] for f in coarse['inner_folds']} != set(inner):
        raise ValueError('cached folds do not cover every inner validation song')
    if len(coarse['inner_folds']) != len(inner):
        raise ValueError('duplicate inner folds')
    passages = []
    for fold in coarse['inner_folds']:
        name, model = fold['validation_track'], fold['model']
        if set(model['training_tracks']) != set(inner)-{name}:
            raise ValueError('cached model violates inner/outer song exclusions')
        record = inner[name]
        scores = predict(model, record['features'])
        passages.extend(threshold_scores(record, scores, selected['threshold'], evaluator)[1])
    observed = calibration_counts(passages)
    if any(observed[k] != selected[k] for k in observed):
        raise ValueError('selected inner counts disagree with independent replay')
    if observed['kick_free_extras'] != 0 or observed['wide_extras'] > coarse['inner_baseline']['wide_extras']:
        raise ValueError('selected point violates zero-core or original wide budget')
    return observed


def recalibrate_variant(records, frozen, name, predict):
    rows = []
    frozen_rows = {r['track']:r for r in frozen['tracks']}
    for record in records:
        original = frozen_rows[record['track']]
        coarse = zero_core_coarse(original['calibration'])
        refined = refine_outer(records, coarse, predict=predict)
        for selection in (coarse['selected_inner_counts'], refined['selected_inner_counts']):
            verify_inner_counts(records, coarse, selection, predict)
        started = time.process_time()
        scores = predict(original['model'], record['features'])
        cpu = time.process_time()-started
        for old_name, threshold in (('fixed_05', .5), ('calibrated', original['calibration']['threshold']),
                                   ('refined', original['refinement']['threshold'])):
            fires, passages = threshold_scores(record, scores, threshold)
            if fires != original['kick_hops'][old_name] or passages != original['scores'][old_name]:
                raise ValueError(f'frozen scorer replay differs: {name}/{record["track"]}/{old_name}')
        coarse_fires, coarse_scores = threshold_scores(record, scores, coarse['threshold'])
        refined_fires, refined_scores = threshold_scores(record, scores, refined['threshold'])
        row = dict(track=record['track'], model=original['model'], calibration=coarse, refinement=refined,
            previous_calibration_threshold=original['calibration']['threshold'],
            previous_refined_threshold=original['refinement']['threshold'],
            kick_hops=dict(fixed_05=original['kick_hops']['fixed_05'], calibrated=coarse_fires, refined=refined_fires),
            scores=dict(baseline=record['ref']['scores']['v5'], fixed_05=original['scores']['fixed_05'],
                        calibrated=coarse_scores, refined=refined_scores),
            comparison_to_previous_protocol=comparisons(original['scores']['refined'], refined_scores),
            diagnostics=runner.diagnostics(refined_scores, record['source']), prediction_cpu_s=cpu,
            sample_rate=record['sample_rate'], hop=record['hop'], cache_metadata=record['cache_metadata'],
            selected_inner_counts_independently_verified=True, frozen_scorer_replay_exact=True)
        row['score_distribution'] = score_diagnostics(record, scores, row)
        rows.append(row)
        print(name, row['track'], refined['threshold'], summarise(refined_scores)['tolerance_ms']['70'], flush=True)
    totals = {key:summarise([p for r in rows for p in r['scores'][key]])
              for key in ('baseline', 'fixed_05', 'calibrated', 'refined')}
    return dict(variant=name, tracks=rows, totals=totals,
        possible_duplicate_triggers=sum(r['diagnostics']['possible_duplicate_triggers'] for r in rows),
        kick_free_extras=sum(r['diagnostics']['kick_free_extras'] for r in rows),
        prediction_cpu_s=sum(r['prediction_cpu_s'] for r in rows))


def add_diagnostics(variant, records, original_linear):
    before = {r['track']:r for r in original_linear['tracks']}
    cores, deltas = [], []
    for row, record in zip(variant['tracks'], records):
        difference = comparisons(before[row['track']]['scores']['refined'], row['scores']['refined'])
        row['comparison_to_original_linear15'] = difference
        lost = sum(len(p['metrics']['70']['lost_labels_s']) for p in difference)
        recovered = sum(len(p['metrics']['70']['recovered_labels_s']) for p in difference)
        row['linear15_delta'] = dict(lost_labels70=lost, recovered_labels70=recovered, net_matched70=recovered-lost)
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
    if len(cores) != 9:
        raise ValueError('expected nine reviewed kick-free cores')
    variant['kick_free_cores'], variant['linear15_track_deltas'] = cores, deltas
    total = variant['totals']['refined']['tolerance_ms']['70']
    variant['acceptance'] = dict(count_target_met=(total['matched'] >= 223 and total['extra'] <= 70)
        or (total['matched'] >= 250 and total['extra'] <= 89),
        nine_kick_free_cores_zero=all(c['extra70'] == 0 for c in cores),
        at_most_one_lost_label_per_track=all(d['lost_labels70'] <= 1 for d in deltas),
        at_most_one_net_lost_match_per_track=all(d['net_matched70'] >= -1 for d in deltas))
    variant['acceptance']['intermediate_target_met'] = all(variant['acceptance'][k] for k in
        ('count_target_met', 'nine_kick_free_cores_zero', 'at_most_one_lost_label_per_track'))


def run(audio_root, baseline_path, cache, expanded_path, linear_path, boosted_path, anchored_path, rule_path, out):
    for variable in ('OPENBLAS_NUM_THREADS', 'VECLIB_MAXIMUM_THREADS', 'OMP_NUM_THREADS'):
        if os.environ.get(variable) != '1':
            raise ValueError(f'{variable}=1 required')
    baseline, expanded, linear_report, boosted_report, anchored_report, rule = [json.loads(p.read_text())
        for p in (baseline_path, expanded_path, linear_path, boosted_path, anchored_path, rule_path)]
    expected = [dict(scorer=n, inner_kick_free_budget=0, wide_extra_budget='unchanged original inner v5',
        thresholds='same23coarse plus51single-bracket refinement',
        fitting='reuse exact existing outer/inner models, no refit') for n in SCORERS]
    if rule['status'] != 'predeclared' or rule['configurations'] != expected:
        raise ValueError('unexpected final zero-core experiment rule')
    dependencies = {}
    for report in (linear_report, boosted_report, anchored_report):
        for field in ('source_sha256', 'upstream_source_sha256'):
            for name, digest in report.get(field, {}).items():
                if sha(ROOT/'tools/audio_analysis/eval'/name) != digest:
                    raise ValueError(f'frozen scorer dependency changed: {name}')
                dependencies[name] = digest
    if sha(expanded_path) != linear_report['expanded_review_sha256']:
        raise ValueError('expanded labels changed')
    def forbidden(*args, **kwargs):
        raise RuntimeError('cached features only; decoding and extraction forbidden')
    with patch.object(cached, 'read_audio', side_effect=forbidden), \
         patch.object(cached, 'fusion_features', side_effect=forbidden):
        records = runner.load_records(audio_root, baseline, cache)
    records, coverage = append_reviews(records, expanded)
    records = [dict(r, features=r['features'][:, :15]) for r in records]
    linear = next(v for v in linear_report['variants'] if v['variant'] == 'linear_15')
    configurations = ((linear, linear_predict),
        (next(v for v in boosted_report['variants'] if v['variant'] == 'boosted_depth3'), tree_predict),
        (next(v for v in anchored_report['variants'] if v['variant'] == 'anchored_tree_0.75'), anchored_predict))
    out.parent.mkdir(parents=True, exist_ok=True)
    report = dict(hypothesis=rule, rule_sha256=sha(rule_path), new_fits=0,
        previous_report_sha256={k:sha(p) for k,p in (('linear',linear_path),('boosted',boosted_path),('anchored',anchored_path))},
        baseline_sha256=sha(baseline_path), expanded_sha256=sha(expanded_path),
        original_label_sha256=baseline['label_sha256'], upstream_source_sha256=dependencies,
        source_sha256={n:sha(ROOT/'tools/audio_analysis/eval'/n) for n in
            ('run_kick_zero_core_trial.py','test_kick_zero_core_trial.py')},
        feature_names=list(FEATURE_NAMES), coverage=coverage, variants=[],
        fixed='Frozen models and coarse counts. Only inner core budget changes tozero; original wide '
            'budget,23coarse points,one51-point bracket refinement,ties,candidates and actual emission timing retained.',
        limitations='Zero inner core fires does not guarantee zero outer fires. All results remain '
            'exploratory nine-song development; no new fitting, audio extraction, reserved data, '
            'listening, native performance or live integration claim.')
    for name,(frozen,predict) in zip(SCORERS,configurations):
        started = time.monotonic()
        result = recalibrate_variant(records, frozen, name+'_zero_core', predict)
        add_cohort_results(result, records)
        add_diagnostics(result, records, linear)
        if len(result['tracks']) != 9 or result['totals']['refined']['labels'] != 381 or result['cohorts']['expanded']['refined']['labels'] != 207:
            raise ValueError('corpus or label counts changed')
        result['evaluation_wall_s'] = time.monotonic()-started
        report['variants'].append(result)
        report['complete'] = len(report['variants']) == len(SCORERS)
        out.write_text(json.dumps(report,indent=2)+'\n')
        print('TOTAL', result['variant'], result['totals']['refined'], result['acceptance'], flush=True)
    return report


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('audio-root','baseline','cache','expanded','linear','boosted','anchored','rule','out'):
        parser.add_argument('--'+name,type=Path,required=True)
    args=parser.parse_args()
    run(args.audio_root,args.baseline,args.cache,args.expanded,args.linear,args.boosted,args.anchored,args.rule,args.out)
