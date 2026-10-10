"""Evaluate predeclared paired-stem augmentation on unchanged natural masters."""
from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import time
from unittest.mock import patch

from . import kick_trajectory_features as cached
from . import run_kick_trajectory_trial as runner
from .kick_attack_rejection import sha
from .kick_fusion_bandwise import FEATURE_NAMES
from .kick_stem_augmented_score import MASSES, FoldFitter, load_augmentation, predict_score
from .run_kick_boosted_trial import add_diagnostics, replay_reference
from .run_kick_dsp_experiments import ROOT
from .run_kick_shape_trial import add_cohort_results, append_reviews


def run(audio_root, baseline_path, cache, expanded_path, previous_path, rule_path, mixtures_path, out):
    for name in ('OPENBLAS_NUM_THREADS', 'VECLIB_MAXIMUM_THREADS', 'OMP_NUM_THREADS'):
        if os.environ.get(name) != '1':
            raise ValueError(f'{name}=1 required')
    baseline, expanded, previous, rule = [json.loads(p.read_text()) for p in
        (baseline_path, expanded_path, previous_path, rule_path)]
    if rule['id'] != 'H4' or [c['augmented_mass_per_available_song'] for c in rule['configurations']] != list(MASSES):
        raise ValueError('frozen rule differs from declared H4 configurations')
    for name, digest in previous['source_sha256'].items():
        if sha(ROOT/'tools/audio_analysis/eval'/name) != digest:
            raise ValueError(f'frozen upstream source changed: {name}')
    if sha(expanded_path) != previous['expanded_review_sha256']:
        raise ValueError('frozen expanded annotations changed')
    def forbidden(*args, **kwargs):
        raise RuntimeError('existing caches required; audio decode and feature extraction forbidden')
    with patch.object(cached, 'read_audio', side_effect=forbidden), patch.object(cached, 'fusion_features', side_effect=forbidden):
        records = runner.load_records(audio_root, baseline, cache)
    records, coverage = append_reviews(records, expanded)
    records = [dict(r, features=r['features'][:, :15]) for r in records]
    frozen = next(v for v in previous['variants'] if v['variant'] == 'linear_15')
    replay_reference(records, frozen)
    mixtures = load_augmentation(mixtures_path)
    out.parent.mkdir(parents=True, exist_ok=True)
    fitter = FoldFitter(mixtures, out.parent/'models')
    report = dict(hypothesis=rule, rule_sha256=sha(rule_path), mixture_features_sha256=sha(mixtures_path),
        baseline_sha256=sha(baseline_path), previous_sha256=sha(previous_path), expanded_sha256=sha(expanded_path),
        original_label_sha256=baseline['label_sha256'], upstream_source_sha256=previous['source_sha256'],
        source_sha256={name: sha(ROOT/'tools/audio_analysis/eval'/name) for name in (
            'kick_stem_augmented_score.py', 'run_kick_stem_augmented_trial.py', 'test_kick_stem_augmented_score.py',
            'kick_boosted_score.py', 'run_kick_boosted_trial.py', 'kick_subspace_score.py')},
        baseline_replay_exact=True, feature_names=list(FEATURE_NAMES), coverage=coverage,
        fixed='Depth3/64trees; original15 features/candidates/horizon/refractory/labels. Original-only normalisation. '
              'Augmentation replaces only the declared fraction of each available training family mass; all families '
              'retain equal total/class mass. Entire validation family excluded in every inner/outer fit. '
              'Existing nested coarse-plus-refined cutoff selection runs on natural masters only.',
        variants=[], complete=False,
        limitations='Four local stem families/16 contexts; fixed-anchor pairs are synthetic diagnostic material, '
                    'not mastering reconstruction. No new extraction, model feature, label, reserve access or native runtime claim.')
    out.write_text(json.dumps(report, indent=2)+'\n')
    for mass in MASSES:
        start = time.monotonic()
        variant = runner.run_variant(records, f'stem_augmented_{mass:g}',
            lambda rows, held: fitter.fit(rows, held, mass), predict_score)
        add_cohort_results(variant, records)
        add_diagnostics(variant, records, frozen)
        if variant['cohorts']['expanded']['refined']['labels'] != 207:
            raise ValueError('expanded label count changed')
        for row in variant['tracks']:
            if row['track'] in row['model']['augmentation_families']:
                raise ValueError('outer source family leaked into augmentation')
            for fold in row['calibration']['inner_folds']:
                if {row['track'], fold['validation_track']}.intersection(fold['model']['augmentation_families']):
                    raise ValueError('inner source family leaked into augmentation')
        variant['evaluation_wall_s'] = time.monotonic()-start
        report['variants'].append(variant); report['fit_cache'] = fitter.statistics()
        report['completed_configurations'] = [v['variant'] for v in report['variants']]
        report['complete'] = len(report['variants']) == len(MASSES)
        out.write_text(json.dumps(report, indent=2)+'\n')
        print('TOTAL', variant['variant'], variant['totals']['refined'], variant['acceptance'], fitter.statistics(), flush=True)
    return report


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('audio-root', 'baseline', 'cache', 'expanded', 'previous', 'rule', 'mixtures', 'out'):
        parser.add_argument('--'+name, type=Path, required=True)
    args = parser.parse_args()
    run(args.audio_root, args.baseline, args.cache, args.expanded, args.previous, args.rule, args.mixtures, args.out)
