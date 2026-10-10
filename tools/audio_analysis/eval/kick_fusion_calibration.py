"""Nested song-excluded calibration of the frozen causal fusion decision score."""
from __future__ import annotations

import numpy as np

from .kick_excess_balance_trial import compact_score
from .run_kick_dsp_experiments import evaluate
from .run_kick_fusion_trial import fit_without, predict_score

THRESHOLDS = tuple(round(.5 + i * .025, 3) for i in range(20)) + (.99, .999, 1.01)
REFRACTORY_SECONDS = .060


def select_fires(scores, available, sr, hop, threshold):
    """Apply the frozen chronological refractory at actual evidence availability."""
    if len(scores) != len(available):
        raise ValueError('scores and availability must align')
    if np.any(np.diff(available) < 0):
        raise ValueError('availability must be chronological')
    last, fires = -np.inf, []
    for score, index in zip(scores, available):
        if score >= threshold and (index - last) * hop / sr >= REFRACTORY_SECONDS - 1e-12:
            fires.append(int(index))
            last = index
    return fires


def threshold_scores(record, scores, threshold, evaluator=evaluate):
    fires = select_fires(scores, record['available'], record['sample_rate'], record['hop'], threshold)
    times = [(index + 1) * record['hop'] / record['sample_rate'] for index in fires]
    return fires, evaluator(record['source'], times)


def calibration_counts(passages):
    return dict(matched_70ms=sum(p['accuracy_by_tolerance_ms']['70']['matched'] for p in passages),
                wide_extras=sum(compact_score(p)['association']['extra'] for p in passages),
                kick_free_extras=sum(compact_score(p)['association']['extra']
                                     for p in passages if p['labels'] == 0))


def choose_threshold(rows, baseline_counts):
    """Maximise 70ms hits within both baseline budgets; deterministic fixed ties."""
    feasible = [r for r in rows if r['wide_extras'] <= baseline_counts['wide_extras']
                and r['kick_free_extras'] <= baseline_counts['kick_free_extras']]
    if not feasible:
        raise ValueError('no feasible threshold; silence must be included')
    return max(feasible, key=lambda r: (r['matched_70ms'], -r['wide_extras'], r['threshold']))


def calibrate_outer(records, outer_track, *, fit=fit_without, predict=predict_score,
                    evaluator=evaluate):
    """Only the other songs enter fitting, standardisation, or threshold selection."""
    inner_records = [r for r in records if r['track'] != outer_track]
    if len(inner_records) < 2:
        raise ValueError('at least two inner songs required')
    if len({r['track'] for r in records}) != len(records):
        raise ValueError('track identities must be unique')
    predictions, inner_models = [], []
    for inner in inner_records:
        model = fit(inner_records, inner['track'])
        forbidden = {outer_track, inner['track']}
        if forbidden.intersection(model['training_tracks']):
            raise ValueError('held-out song leaked into inner fit')
        predictions.append((inner, predict(model, inner['features'])))
        inner_models.append(dict(validation_track=inner['track'], model=model))
    baseline = calibration_counts([p for r in inner_records for p in r['ref']['scores']['v5']])
    rows = []
    for threshold in THRESHOLDS:
        passages = [p for r, scores in predictions
                    for p in threshold_scores(r, scores, threshold, evaluator)[1]]
        counts = calibration_counts(passages)
        rows.append(dict(threshold=threshold, **counts,
                         feasible=counts['wide_extras'] <= baseline['wide_extras']
                         and counts['kick_free_extras'] <= baseline['kick_free_extras']))
    chosen = choose_threshold(rows, baseline)
    return dict(outer_track=outer_track, threshold=chosen['threshold'],
                threshold_selection_tracks=[r['track'] for r in inner_records],
                inner_baseline=baseline, selected_inner_counts=chosen,
                threshold_trials=rows, inner_folds=inner_models)


def nested_calibration(records, *, fit=fit_without, predict=predict_score, evaluator=evaluate):
    folds = []
    for outer in records:
        calibration = calibrate_outer(records, outer['track'], fit=fit, predict=predict,
                                      evaluator=evaluator)
        model = fit(records, outer['track'])
        if outer['track'] in model['training_tracks']:
            raise ValueError('held-out song leaked into outer fit')
        probabilities = predict(model, outer['features'])
        fixed_fires, fixed = threshold_scores(outer, probabilities, .5, evaluator)
        fires, calibrated = threshold_scores(outer, probabilities, calibration['threshold'], evaluator)
        folds.append(dict(track=outer['track'], model=model, calibration=calibration,
                          kick_hops=dict(fixed_05=fixed_fires, calibrated=fires),
                          scores=dict(baseline=outer['ref']['scores']['v5'],
                                      fixed_05=fixed, calibrated=calibrated)))
    return folds
