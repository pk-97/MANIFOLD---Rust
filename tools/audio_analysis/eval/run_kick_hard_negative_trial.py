"""Frozen 1x/2x/4x negative-weight comparison on cached fifteen-feature DSP."""
from __future__ import annotations

import argparse
import json
from pathlib import Path
from unittest.mock import patch

import numpy as np

from . import kick_trajectory_features as cached
from . import run_kick_trajectory_trial as runner
from .kick_attack_rejection import sha
from .kick_fusion_bandwise import FEATURE_NAMES
from .kick_hard_negative_weights import FoldFitter, MULTIPLIERS, reviewed_masks, temporal_eligibility
from .run_kick_dsp_experiments import ROOT
from .run_kick_fusion_trial import predict_score
from .run_kick_shape_trial import append_reviews, add_cohort_results


def rank_diagnostics(record, model, baseline):
    """Within-song ordering on one frozen pool, independent of event cutoff choice."""
    positive = record['training_mask'] & (record['labels'] == 1)
    negative = temporal_eligibility(record)
    ids = record['event_ids'][positive]
    events, counts = np.unique(ids, return_counts=True)
    if not len(events) or not negative.any():
        raise ValueError('both classes required for rank diagnostics')
    weights = np.zeros(len(ids))
    for event, count in zip(events, counts):
        weights[ids == event] = 1/len(events)/count

    def logits(m):
        z = np.clip((record['features']-np.asarray(m['mean']))/np.asarray(m['scale']), -8, 8)
        return z @ np.asarray(m['weights']) + m['intercept']

    current, previous = logits(model), logits(baseline)

    def class_result(values):
        p, n = values[positive], values[negative]
        margins = p[:, None]-n[None, :]
        ordering = np.mean((margins > 0)+.5*(margins == 0), axis=1)
        return dict(event_balanced_candidate_auc=float(weights @ ordering),
            positive_logit_quantiles=np.quantile(p, [.1, .5, .9]).tolist(),
            negative_logit_quantiles=np.quantile(n, [.1, .5, .9, .99]).tolist(),
            event_balanced_mean_logit_margin=float(weights @ p-n.mean()))

    result = class_result(current)
    result.update(positive_candidates=int(positive.sum()), positive_events=len(events),
        negative_candidates=int(negative.sum()), baseline=class_result(previous),
        positive_paired_logit_change=np.quantile((current-previous)[positive], [.1, .5, .9]).tolist(),
        negative_paired_logit_change=np.quantile((current-previous)[negative], [.1, .5, .9]).tolist())
    return result


def baseline_replay(variant, frozen):
    before = {r['track']: r for r in frozen['tracks']}
    for row in variant['tracks']:
        old = before[row['track']]
        for name in ('fixed_05', 'calibrated', 'refined'):
            if row['kick_hops'][name] != old['kick_hops'][name] or row['scores'][name] != old['scores'][name]:
                raise ValueError(f'expanded 1x replay differs: {row["track"]}/{name}')
        for name in ('calibration', 'refinement'):
            if row[name]['threshold'] != old[name]['threshold']:
                raise ValueError('1x calibration cutoff differs')
        pairs = [(row['model'], old['model'])] + [
            (a['model'], b['model']) for a, b in zip(row['calibration']['inner_folds'], old['calibration']['inner_folds'])]
        for a, b in pairs:
            for field in ('training_tracks', 'mean', 'scale', 'weights', 'intercept'):
                if a[field] != b[field]:
                    raise ValueError(f'1x nested model differs: {field}')


def coefficient_diagnostics(variants):
    base = np.asarray([r['model']['weights'] for r in variants[0]['tracks']])
    result = []
    for variant in variants:
        w = np.asarray([r['model']['weights'] for r in variant['tracks']])
        delta = w-base
        result.append(dict(variant=variant['variant'], features=[dict(
            feature=name, median=float(np.median(w[:, i])), minimum=float(w[:, i].min()),
            maximum=float(w[:, i].max()), positive_folds=int((w[:, i]>0).sum()),
            negative_folds=int((w[:, i]<0).sum()), median_change=float(np.median(delta[:, i])),
            increasing_folds=int((delta[:, i]>1e-12).sum()), decreasing_folds=int((delta[:, i]<-1e-12).sum()))
            for i, name in enumerate(FEATURE_NAMES)],
            cosine_with_baseline=[float(a @ b/(np.linalg.norm(a)*np.linalg.norm(b))) for a, b in zip(w, base)]))
    return result


def run(audio_root, baseline_path, cache, expanded_path, previous_path, review_path, rule_path, out):
    baseline, expanded, previous, review, rule = [json.loads(p.read_text())
        for p in (baseline_path, expanded_path, previous_path, review_path, rule_path)]
    if rule['multipliers'] != list(MULTIPLIERS) or rule['status'] != 'frozen_before_weighted_fits':
        raise ValueError('unexpected experiment rule')
    # No changed upstream feature, fit, calibration or scoring source may hide in the comparison.
    for name, digest in previous['source_sha256'].items():
        if sha(ROOT/'tools/audio_analysis/eval'/name) != digest:
            raise ValueError(f'frozen upstream source changed: {name}')
    if sha(expanded_path) != previous['expanded_review_sha256']:
        raise ValueError('expanded label source changed')
    def forbidden(*args, **kwargs):
        raise RuntimeError('existing feature caches required; no audio decode or extraction')
    with patch.object(cached, 'read_audio', side_effect=forbidden), \
         patch.object(cached, 'fusion_features', side_effect=forbidden):
        records = runner.load_records(audio_root, baseline, cache)
    records, coverage = append_reviews(records, expanded)
    records = [dict(r, features=r['features'][:, :15]) for r in records]
    fitter = FoldFitter(reviewed_masks(records, review))
    variants = []
    frozen = next(v for v in previous['variants'] if v['variant'] == 'linear_15')
    for multiplier in MULTIPLIERS:
        fit = lambda r, held: fitter.fit(r, held, multiplier)
        variant = runner.run_variant(records, f'hard_negative_{multiplier}x', fit, predict_score)
        add_cohort_results(variant, records)
        if multiplier == 1:
            baseline_replay(variant, frozen)
        for row, record, before in zip(variant['tracks'], records, frozen['tracks']):
            if row['track'] != before['track']:
                raise ValueError('baseline track order changed')
            row['ranking'] = rank_diagnostics(record, row['model'], before['model'])
            # Verify that every potential hard negative received a source disposition.
            for model in [row['model']] + [f['model'] for f in row['calibration']['inner_folds']]:
                key = tuple(tuple(k) for k in model['training_input_keys'])
                unweighted = fitter.baselines[key]
                for r in records:
                    if r['track'] not in model['training_tracks']:
                        continue
                    potential = temporal_eligibility(r) & (predict_score(unweighted, r['features']) >= .90)
                    evidence = next(t for t in review['tracks'] if t['track'] == r['track'])
                    audited = {c['candidate_index'] for c in evidence['candidates']}
                    if set(np.flatnonzero(potential)) - audited:
                        raise ValueError('an eligible high-score negative lacks evidence review')
        variant['macro_candidate_auc'] = float(np.mean([r['ranking']['event_balanced_candidate_auc']
                                                       for r in variant['tracks']]))
        variants.append(variant)
    report = dict(rule=rule, rule_sha256=sha(rule_path), review_sha256=sha(review_path),
        previous_sha256=sha(previous_path), baseline_sha256=sha(baseline_path),
        expanded_sha256=sha(expanded_path), original_label_sha256=baseline['label_sha256'],
        source_sha256={name: sha(ROOT/'tools/audio_analysis/eval'/name) for name in (
            'kick_hard_negative_weights.py', 'run_kick_hard_negative_trial.py')},
        upstream_source_sha256=previous['source_sha256'], baseline_replay_exact=True,
        unique_training_sets=len(fitter.baselines), fitted_models=len(fitter.models),
        feature_names=list(FEATURE_NAMES), coverage=coverage, variants=variants,
        coefficients=coefficient_diagnostics(variants),
        limitations='Exploratory nine-song development and provisional raw/source visual labels. '
                    'Candidate ranking is temporal-label diagnostic, not source causation or event accuracy. '
                    'Scores are not calibrated probabilities. Calibration budgets are inner wide extras, '
                    'not an outer70ms guarantee. No reserved data, live/native timing proof or new features.')
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(report, indent=2)+'\n')
    return report


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('audio-root', 'baseline', 'cache', 'expanded', 'previous', 'review', 'rule', 'out'):
        parser.add_argument('--'+name, type=Path, required=True)
    args = parser.parse_args()
    run(args.audio_root, args.baseline, args.cache, args.expanded, args.previous, args.review, args.rule, args.out)
