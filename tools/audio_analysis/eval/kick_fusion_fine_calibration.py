"""One inner-only threshold refinement of the frozen 15-feature bandwise fusion."""
from __future__ import annotations

import argparse
import copy
import json
from pathlib import Path

import numpy as np

from .kick_attack_rejection import sha
from .kick_fusion_bandwise import FEATURE_NAMES, fusion_features
from .kick_fusion_calibration import THRESHOLDS, calibration_counts, choose_threshold, threshold_scores
from .run_kick_dsp_experiments import ROOT, development_sources, evaluate
from .run_kick_fusion_calibration import comparisons, run as coarse_run, summarise
from .run_kick_fusion_trial import predict_score


def refinement_grid(coarse):
    """Preserve every coarse point and add 51 interior points in one inner-only bracket."""
    selected = coarse['threshold']
    lower = [r['threshold'] for r in coarse['threshold_trials']
             if not r['feasible'] and r['threshold'] < selected]
    if not lower:
        return list(THRESHOLDS), None
    bracket = [max(lower), selected]
    added = np.linspace(*bracket, 53)[1:-1].tolist()
    return sorted(set(THRESHOLDS).union(added)), bracket


def refine_outer(records, coarse, *, predict=predict_score, evaluator=evaluate):
    """Reuse the nested inner models; the outer record is never read here."""
    outer = coarse['outer_track']
    inner = {r['track']: r for r in records if r['track'] != outer}
    if set(inner) != set(coarse['threshold_selection_tracks']):
        raise ValueError('inner records differ from coarse calibration')
    predictions = []
    for fold in coarse['inner_folds']:
        name, model = fold['validation_track'], fold['model']
        if set(model['training_tracks']) != set(inner) - {name}:
            raise ValueError('inner model must exclude outer and validation songs')
        record = inner[name]
        predictions.append((record, predict(model, record['features'])))
    grid, bracket = refinement_grid(coarse)
    rows = {r['threshold']: r for r in coarse['threshold_trials']}
    budget = coarse['inner_baseline']
    for threshold in grid:
        if threshold in rows:
            continue
        counts = calibration_counts([p for r, scores in predictions
                                     for p in threshold_scores(r, scores, threshold, evaluator)[1]])
        rows[threshold] = dict(threshold=threshold, **counts,
                              feasible=counts['wide_extras'] <= budget['wide_extras']
                              and counts['kick_free_extras'] <= budget['kick_free_extras'])
    trials = [rows[t] for t in grid]
    chosen = choose_threshold(trials, budget)
    return dict(outer_track=outer, threshold=chosen['threshold'], coarse_threshold=coarse['threshold'],
                bracket=bracket, added_thresholds=[t for t in grid if t not in THRESHOLDS],
                threshold_selection_tracks=coarse['threshold_selection_tracks'],
                inner_baseline=budget, selected_inner_counts=chosen, threshold_trials=trials)


def build_report(coarse, refinements):
    """Keep frozen score schemas and label identities for baseline/coarse/refined comparisons."""
    report = copy.deepcopy(coarse)
    variants = ('baseline', 'fixed_05', 'coarse', 'refined')
    for row, refined in zip(report['tracks'], refinements):
        if row['track'] != refined['track']:
            raise ValueError('refinement track order differs')
        row['scores']['coarse'] = row['scores'].pop('calibrated')
        row['kick_hops']['coarse'] = row['kick_hops'].pop('calibrated')
        row['scores']['refined'] = refined['scores']
        row['kick_hops']['refined'] = refined['kick_hops']
        row['coarse_calibration'] = row.pop('calibration')
        row['refinement'] = refined['calibration']
        row['comparisons'] = {v: comparisons(row['scores']['baseline'], row['scores'][v])
                              for v in variants[1:]}
        row['coarse_to_refined'] = comparisons(row['scores']['coarse'], row['scores']['refined'])
    report['method'] = __doc__
    report['variant'] = 'bandwise_15_inner_grid_refinement'
    report['refinement_rule'] = ('Keep all 23 coarse thresholds. Add 51 evenly spaced interior '
        'points between the nearest lower infeasible coarse threshold and the selected feasible '
        'coarse threshold. With no lower infeasible point keep the coarse grid. Inner results only; '
        'unchanged summed 70ms objective, wide/kick-free budgets, and tie rules.')
    report['limitations'] = ('All nine songs already explored development data, not untouched '
        'validation. Provisional labels; wide association is diagnostic unmatched triggers, not '
        'proven false kicks. Inner aggregate budgets do not guarantee outer or per-core budgets '
        'or retained labels. This is one authorised refinement, no additional search or live changes.')
    report['totals'] = {v: summarise([p for t in report['tracks'] for p in t['scores'][v]]) for v in variants}
    report['kick_free_core_totals'] = {v: summarise([p for t in report['tracks'] for p in t['scores'][v]
                                                   if p['labels'] == 0]) for v in variants}
    report['bass_only_cores'] = [dict(track=t['track'], passage=p['id'],
        scores={v: summarise([t['scores'][v][i]]) for v in variants})
        for t in report['tracks'] for i, p in enumerate(t['scores']['baseline']) if p['labels'] == 0]
    report['selected_thresholds'] = {t['track']: dict(coarse=t['coarse_calibration']['threshold'],
        refined=t['refinement']['threshold']) for t in report['tracks']}
    return report


def run(audio_root, baseline_path, coarse_reference, out):
    out, coarse_reference = Path(out), Path(coarse_reference)
    extracted = []

    def collect(audio, sample_rate):
        result = fusion_features(audio, sample_rate)
        extracted.append(result)
        return result

    bandwise_path = ROOT / 'tools/audio_analysis/eval/kick_fusion_bandwise.py'
    coarse = coarse_run(audio_root, baseline_path, out.with_name('coarse_replay.json'),
                        extractor=collect, variant='bandwise_15', feature_names=FEATURE_NAMES,
                        extractor_paths=(bandwise_path,))
    frozen = json.loads(coarse_reference.read_text())
    frozen_tracks = {t['track']: t for t in frozen['tracks']}
    replay = {t['track']: all(t['kick_hops'][v] == frozen_tracks[t['track']]['kick_hops'][v]
                             for v in ('fixed_05', 'calibrated')) for t in coarse['tracks']}
    if len(replay) != 9 or not all(replay.values()):
        raise ValueError(f'coarse replay differs: {replay}')
    sources = development_sources(Path(audio_root))
    label_path = ROOT / 'tests/fixtures/audio_labels/heldout_passages_2026-10-09.json'
    sources += [dict(track=t['track'], group='former_heldout', passages=t['passages'])
                for t in json.loads(label_path.read_text())['tracks']]
    records = [dict(track=t['track'], source=s, features=ex[2], available=ex[1],
                    sample_rate=t['sample_rate'], hop=ex[3])
               for t, s, ex in zip(coarse['tracks'], sources, extracted)]
    refinements = []
    for record, row in zip(records, coarse['tracks']):
        refined = refine_outer(records, row['calibration'])
        scores = predict_score(row['model'], record['features'])
        fires, passages = threshold_scores(record, scores, refined['threshold'])
        refinements.append(dict(track=row['track'], calibration=refined, kick_hops=fires, scores=passages))
        print('REFINED', row['track'], refined['coarse_threshold'], refined['threshold'],
              summarise(passages)['tolerance_ms'], flush=True)
    report = build_report(coarse, refinements)
    report.update(coarse_reference=str(coarse_reference), coarse_reference_sha256=sha(coarse_reference),
                  coarse_replay_exact=replay, coarse_replay_sha256=sha(out.with_name('coarse_replay.json')))
    report['source_sha256']['tools/audio_analysis/eval/kick_fusion_fine_calibration.py'] = sha(Path(__file__))
    out.write_text(json.dumps(report, indent=2) + '\n')
    print('TOTALS', json.dumps(report['totals']), flush=True)
    return report


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--audio-root', required=True, type=Path)
    parser.add_argument('--baseline', required=True, type=Path)
    parser.add_argument('--coarse-reference', required=True, type=Path)
    parser.add_argument('--out', required=True, type=Path)
    args = parser.parse_args()
    run(args.audio_root, args.baseline, args.coarse_reference, args.out)


if __name__ == '__main__':
    main()
