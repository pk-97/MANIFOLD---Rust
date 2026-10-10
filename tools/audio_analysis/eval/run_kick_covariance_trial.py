"""Three predeclared covariance scorers on frozen nine-song cached features."""
from __future__ import annotations

import argparse
import json
from pathlib import Path
from unittest.mock import patch

import numpy as np

from . import kick_trajectory_features as cached
from . import run_kick_trajectory_trial as runner
from .kick_attack_rejection import sha
from .kick_covariance_score import FoldFitter, SHRINKAGES, decision_logits, predict_score
from .kick_fusion_bandwise import FEATURE_NAMES
from .kick_hard_negative_weights import temporal_eligibility
from .run_kick_dsp_experiments import ROOT
from .run_kick_fusion_calibration import comparisons, summarise
from .run_kick_shape_trial import append_reviews, add_cohort_results


def ranking(record, logits):
    positive = record['training_mask'] & (record['labels'] == 1)
    negative = temporal_eligibility(record)
    p, n = logits[positive], np.sort(logits[negative])
    ids = record['event_ids'][positive]
    events, counts = np.unique(ids, return_counts=True)
    weights = np.zeros(len(ids))
    for event, count in zip(events, counts):
        weights[ids == event] = 1/len(events)/count
    order = (np.searchsorted(n, p, side='left')+np.searchsorted(n, p, side='right'))/(2*len(n))
    return dict(event_balanced_candidate_auc=float(weights @ order),
        positive_candidates=int(positive.sum()), positive_events=len(events),
        negative_candidates=len(n), positive_logit_quantiles=np.quantile(p, [.1, .5, .9]).tolist(),
        negative_logit_quantiles=np.quantile(n, [.1, .5, .9, .99]).tolist())


def run(audio_root, baseline_path, cache, expanded_path, previous_path, rule_path, out):
    baseline, expanded, previous, rule = [json.loads(p.read_text())
        for p in (baseline_path, expanded_path, previous_path, rule_path)]
    hypothesis = next(h for h in rule['hypotheses'] if h['id'] == 'H2')
    if [c['shrinkage'] for c in hypothesis['configurations']] != list(SHRINKAGES):
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
    variants = []
    for shrinkage in SHRINKAGES:
        fitter = FoldFitter(shrinkage)
        variant = runner.run_variant(records, f'covariance_{shrinkage:g}', fitter.fit, predict_score)
        add_cohort_results(variant, records)
        for row, record, before in zip(variant['tracks'], records, frozen['tracks']):
            assert row['track'] == before['track']
            for model in [row['model']] + [f['model'] for f in row['calibration']['inner_folds']]:
                if row['track'] in model['training_tracks']:
                    raise ValueError('outer song leaked into fitting')
            row['ranking'] = ranking(record, decision_logits(row['model'], record['features']))
            row['comparison_to_linear15'] = comparisons(before['scores']['refined'], row['scores']['refined'])
            row['lost_linear15_labels70'] = sum(len(p['metrics']['70']['lost_labels_s'])
                for p in row['comparison_to_linear15'])
            row['matched70_change_from_linear15'] = (
                summarise(row['scores']['refined'])['tolerance_ms']['70']['matched']
                -summarise(before['scores']['refined'])['tolerance_ms']['70']['matched'])
        variant['unique_training_sets'] = len(fitter.cache)
        variant['macro_candidate_auc'] = float(np.mean([
            r['ranking']['event_balanced_candidate_auc'] for r in variant['tracks']]))
        variant['net_track_safeguard'] = all(r['matched70_change_from_linear15'] >= -1 for r in variant['tracks'])
        variant['track_safeguard'] = all(r['lost_linear15_labels70'] <= 1 for r in variant['tracks'])
        metric = variant['totals']['refined']['tolerance_ms']['70']
        variant['intermediate_pass'] = ((metric['matched'] >= 223 and metric['extra'] <= 70)
            or (metric['matched'] >= 250 and metric['extra'] <= 89)) and variant['track_safeguard'] \
            and variant['kick_free_extras'] == 0
        variants.append(variant)
        out.parent.mkdir(parents=True, exist_ok=True)
        report = dict(hypothesis=hypothesis, run_manifest_sha256=sha(rule_path),
            previous_sha256=sha(previous_path), baseline_sha256=sha(baseline_path),
            expanded_sha256=sha(expanded_path), original_label_sha256=baseline['label_sha256'],
            source_sha256={name:sha(ROOT/'tools/audio_analysis/eval'/name) for name in (
                'kick_covariance_score.py', 'run_kick_covariance_trial.py')},
            upstream_source_sha256=previous['source_sha256'], feature_names=list(FEATURE_NAMES),
            coverage=coverage, variants=variants,
            limitations='Exploratory nine-song development, provisional annotations. Scores are not '
                'probabilities. Frozen coarse-plus-refinement cutoff uses inner-fold wide extras; '
                'not a guarantee of outer70ms extras. No reserved or native streaming validation.')
        out.write_text(json.dumps(report, indent=2)+'\n')
        print('TOTAL', variant['variant'], metric, 'safeguards', variant['track_safeguard'],
              variant['kick_free_extras'], 'AUC', variant['macro_candidate_auc'], flush=True)
    return report


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('audio-root', 'baseline', 'cache', 'expanded', 'previous', 'rule', 'out'):
        parser.add_argument('--'+name, type=Path, required=True)
    args = parser.parse_args()
    run(args.audio_root, args.baseline, args.cache, args.expanded, args.previous, args.rule, args.out)
