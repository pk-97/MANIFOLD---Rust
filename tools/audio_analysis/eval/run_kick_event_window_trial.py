"""Three predeclared training-only latent-window fits on frozen full-mix caches."""
from __future__ import annotations

import argparse
import json
from pathlib import Path
from unittest.mock import patch

import numpy as np

from . import kick_trajectory_features as cached
from . import run_kick_trajectory_trial as runner
from .kick_attack_rejection import sha
from .kick_event_window_score import FoldFitter, SELECTIONS
from .run_kick_boosted_trial import replay_reference
from .run_kick_covariance_trial import ranking
from .run_kick_dsp_experiments import ROOT
from .run_kick_fusion_calibration import comparisons, summarise
from .run_kick_fusion_trial import predict_score
from .run_kick_shape_trial import add_cohort_results, append_reviews


def run(audio_root, baseline_path, cache, expanded_path, previous_path, rule_path, out):
    baseline, expanded, previous, rule = [json.loads(p.read_text())
        for p in (baseline_path, expanded_path, previous_path, rule_path)]
    if rule['id'] != 'H5' or [c['selection'] for c in rule['configurations']] != list(SELECTIONS):
        raise ValueError('predeclaration differs')
    for name, digest in previous['source_sha256'].items():
        if sha(ROOT/'tools/audio_analysis/eval'/name) != digest:
            raise ValueError(f'frozen upstream source changed: {name}')
    if sha(expanded_path) != previous['expanded_review_sha256']:
        raise ValueError('expanded labels changed')

    def forbidden(*args, **kwargs):
        raise RuntimeError('cache-only experiment may not decode audio')
    with patch.object(cached, 'read_audio', side_effect=forbidden), \
         patch.object(cached, 'fusion_features', side_effect=forbidden):
        records = runner.load_records(audio_root, baseline, cache)
    records, coverage = append_reviews(records, expanded)
    records = [dict(r, features=r['features'][:, :15]) for r in records]
    frozen = next(v for v in previous['variants'] if v['variant'] == 'linear_15')
    replay_reference(records, frozen)
    variants = []
    for selection in SELECTIONS:
        fitter = FoldFitter(selection)
        variant = runner.run_variant(records, f'event_window_{selection}', fitter.fit, predict_score)
        add_cohort_results(variant, records)
        core_count = 0
        for row, record, before in zip(variant['tracks'], records, frozen['tracks']):
            assert row['track'] == before['track']
            for model in [row['model']] + [f['model'] for f in row['calibration']['inner_folds']]:
                if row['track'] in model['training_tracks']:
                    raise ValueError('outer song leaked into fitting')
            model = row['model']
            z = np.clip((record['features']-model['mean'])/model['scale'], -8, 8)
            row['ranking'] = ranking(record, z @ model['weights']+model['intercept'])
            comp = comparisons(before['scores']['refined'], row['scores']['refined'])
            row['comparison_to_linear15'] = comp
            row['linear15_delta'] = dict(lost_labels70=sum(len(p['metrics']['70']['lost_labels_s']) for p in comp),
                recovered_labels70=sum(len(p['metrics']['70']['recovered_labels_s']) for p in comp),
                net_matched70=summarise(row['scores']['refined'])['tolerance_ms']['70']['matched']
                  -summarise(before['scores']['refined'])['tolerance_ms']['70']['matched'])
            core_count += sum(p['labels'] == 0 for p in row['scores']['refined'])
        if core_count != 9:
            raise ValueError('kick-free core set changed')
        variant['unique_training_sets'] = len(fitter.cache)
        variant['macro_candidate_auc'] = float(np.mean([
            r['ranking']['event_balanced_candidate_auc'] for r in variant['tracks']]))
        metric = variant['totals']['refined']['tolerance_ms']['70']
        variant['acceptance'] = dict(count_target_met=(metric['matched'] >= 223 and metric['extra'] <= 70)
            or (metric['matched'] >= 250 and metric['extra'] <= 89),
            nine_kick_free_cores_zero=variant['kick_free_extras'] == 0,
            at_most_one_lost_label_per_track=all(r['linear15_delta']['lost_labels70'] <= 1 for r in variant['tracks']))
        variant['acceptance']['intermediate_target_met'] = all(variant['acceptance'].values())
        variants.append(variant)
        out.parent.mkdir(parents=True, exist_ok=True)
        report = dict(hypothesis=rule, rule_sha256=sha(rule_path),
            previous_sha256=sha(previous_path), baseline_sha256=sha(baseline_path),
            expanded_sha256=sha(expanded_path), original_label_sha256=baseline['label_sha256'],
            source_sha256={name:sha(ROOT/'tools/audio_analysis/eval'/name) for name in (
                'kick_event_window_score.py', 'run_kick_event_window_trial.py',
                'test_kick_event_window_score.py', 'run_kick_covariance_trial.py')},
            upstream_source_sha256=previous['source_sha256'], baseline_replay_exact=True,
            coverage=coverage, variants=variants,
            limitations='Exploratory development only, same provisional annotations. Latent selection '
                'occurs only inside training folds. Unchosen positive windows are ignored, never '
                'relabeled negative. No inference history, extra wait, new audio or reserved validation.')
        out.write_text(json.dumps(report, indent=2)+'\n')
        print('TOTAL', variant['variant'], metric, variant['acceptance'], flush=True)
    return report


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('audio-root', 'baseline', 'cache', 'expanded', 'previous', 'rule', 'out'):
        parser.add_argument('--'+name, type=Path, required=True)
    args = parser.parse_args()
    run(args.audio_root, args.baseline, args.cache, args.expanded, args.previous, args.rule, args.out)
