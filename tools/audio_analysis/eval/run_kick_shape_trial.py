"""One fixed shape-representation trial on expanded, song-excluded local reviews."""
from __future__ import annotations

import argparse
import copy
import json
from pathlib import Path
from unittest.mock import patch

from . import kick_trajectory_features as cached_module
from . import run_kick_trajectory_trial as runner
from .kick_attack_rejection import sha
from .kick_shape_features import FEATURE_NAMES, from_cached_features
from .run_kick_dsp_experiments import ROOT, evaluate
from .run_kick_fusion_calibration import summarise
from .run_kick_fusion_trial import fit_without, predict_score, training_labels


def append_reviews(records, expanded):
    """Add independently reviewed cores without changing original annotations."""
    if expanded.get('status') != 'lead_reviewed_visual_provisional':
        raise ValueError('expanded annotations require explicit lead review')
    additions = {r['track']: r for r in expanded['tracks']}
    if len(additions) != len(expanded['tracks']):
        raise ValueError('duplicate expanded track')
    allowed = {'late_night', 'midnight_patience', 'miracle', 'heavy_on_mind'}
    if set(additions) - allowed or set(additions) - {r['track'] for r in records}:
        raise ValueError('unexpected expanded track; reserved families are forbidden')
    result, coverage = [], []
    for record in records:
        row = dict(record)
        source = copy.deepcopy(record['source'])
        original_ids = {p['id'] for p in record['ref']['scores']['v5']}
        if row['track'] in additions:
            added = additions[row['track']]
            if source['group'] == 'original_five' or added['audio_sha256'] != source['audio_sha256']:
                raise ValueError('expanded review source differs from cached master')
            for core in added['cores']:
                coverage.append(dict(track=row['track'], id=core['id'],
                    start_s=core['start_s'], end_s=core['end_s'],
                    scoring_ready=core['scoring_ready'], reason=core.get('reason')))
                if not core['scoring_ready']:
                    if not core.get('reason'):
                        raise ValueError('unscorable core requires a reason')
                    continue
                if core['id'] in {p['id'] for p in source['passages']}:
                    raise ValueError('duplicate passage identity')
                if any(core['start_s'] < p['end_s'] and core['end_s'] > p['start_s']
                       for p in source['passages']):
                    raise ValueError('expanded core overlaps another scoring core')
                if not 0 <= core['review_start_s'] < core['review_end_s'] <= record['cache_metadata']['duration_s']:
                    raise ValueError('review context lies outside cached recording')
                # The existing scorer validates sorted labels and review margins.
                evaluate(dict(group='master', passages=[core]), [])
                source['passages'].append(copy.deepcopy(core))
            source['passages'].sort(key=lambda p: p['start_s'])
        reference = dict(record['ref'], scores=dict(v5=evaluate(source, [
            (i + 1) * row['hop'] / row['sample_rate'] for i in record['ref']['variants']['v5']])))
        replayed_original = [p for p in reference['scores']['v5'] if p['id'] in original_ids]
        if {p['id']: p for p in replayed_original} != {
                p['id']: p for p in record['ref']['scores']['v5']}:
            raise ValueError('original prototype scoring changed while adding reviews')
        mask, labels, ids = training_labels(source, reference, row['candidates'], row['available'],
                                            row['sample_rate'], row['hop'], row['cache_metadata']['duration_s'])
        row.update(source=source, ref=reference, training_mask=mask, labels=labels,
                   event_ids=ids, original_passage_ids=original_ids)
        result.append(row)
    if not any(c['scoring_ready'] for c in coverage):
        raise ValueError('no usable expanded cores')
    return result, coverage


def add_cohort_results(variant, records):
    """Keep the frozen reference and newly reviewed passages separately visible."""
    ids = {r['track']: r['original_passage_ids'] for r in records}
    for row in variant['tracks']:
        row['cohorts'] = {}
        for cohort in ('original', 'expanded'):
            keep = lambda p: (p['id'] in ids[row['track']]) == (cohort == 'original')
            row['cohorts'][cohort] = {name: summarise([p for p in row['scores'][name] if keep(p)])
                                     for name in ('baseline', 'refined')}
    variant['cohorts'] = {cohort: {name: summarise([
        p for row in variant['tracks'] for p in row['scores'][name]
        if (p['id'] in ids[row['track']]) == (cohort == 'original')])
        for name in ('baseline', 'refined')} for cohort in ('original', 'expanded')}
    if variant['cohorts']['original']['refined']['labels'] != 174:
        raise ValueError('frozen reference label count changed')


def run(audio_root, baseline_path, reference_path, cache, expanded_path, out):
    baseline, reference, expanded = [json.loads(p.read_text())
                                      for p in (baseline_path, reference_path, expanded_path)]
    def forbidden(*args, **kwargs):
        raise RuntimeError('this experiment requires existing feature caches; decoding/building forbidden')
    with patch.object(cached_module, 'read_audio', side_effect=forbidden), \
         patch.object(cached_module, 'fusion_features', side_effect=forbidden):
        records = runner.load_records(audio_root, baseline, cache)
    runner.verify_base_replay(records, reference)
    records, coverage = append_reviews(records, expanded)
    variants = []
    for name, transform in (
        ('linear_15', lambda x: x[:, :15]),
        ('linear_39', lambda x: x),
        ('linear_shape_24', from_cached_features),
    ):
        selected = [dict(r, features=transform(r['features'])) for r in records]
        result = runner.run_variant(selected, name, fit_without, predict_score)
        add_cohort_results(result, records)
        variants.append(result)
    names = set(cached_module.DEPENDENCIES) | {
        'run_kick_shape_trial.py', 'kick_shape_features.py', 'run_kick_trajectory_trial.py',
        'run_kick_fusion_trial.py', 'kick_fusion_calibration.py', 'kick_fusion_fine_calibration.py',
        'run_kick_dsp_experiments.py', 'run_kick_fusion_calibration.py',
        'live_kick_baseline.py', 'master_kick_comparison.py'}
    report = dict(
        hypothesis='Band-normalised temporal moments, directed time differences and pair concordance '
                   'make attack relationships more transferable than raw trajectory coordinates.',
        changed='Representation only: first15 frozen features plus9 fixed shape values; one configuration.',
        fixed='Same candidates, completed-hop deadlines, observation horizon, refractory, regularised linear '
              'scorer, corrected training exclusions, song/class/event weights, and nested cutoff protocol. '
              'Both comparison representations use the same expanded training data; no target-song fitting.',
        acceptance='Full target remains >=95% recall at70ms and >=95% precision, >=90% recall each track, '
                   'zero fires in four original reviewed kick-free cores, >=166/174 and <=8 extras on the '
                   'original pack. Expanded and untouched final validation are also required. '
                   'Report improvements and regressions against both linear controls and the v5 prototype.',
        feature_names=list(FEATURE_NAMES), expanded_review_sha256=sha(expanded_path),
        baseline_sha256=sha(baseline_path), reference_sha256=sha(reference_path),
        source_sha256={name: sha(ROOT/'tools/audio_analysis/eval'/name) for name in sorted(names)},
        original_label_sha256=baseline['label_sha256'], coverage=coverage, variants=variants,
        limitations='Explored development songs with provisional visual source-assisted labels. '
                    'Shape values do not reconstruct clipped information or identify sources by themselves. '
                    'No added observation delay, reserved audio, native timing proof, or live integration.')
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(report, indent=2)+'\n')
    return report


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('audio-root', 'baseline', 'reference', 'cache', 'expanded', 'out'):
        parser.add_argument('--'+name, type=Path, required=True)
    args = parser.parse_args()
    run(args.audio_root, args.baseline, args.reference, args.cache, args.expanded, args.out)
