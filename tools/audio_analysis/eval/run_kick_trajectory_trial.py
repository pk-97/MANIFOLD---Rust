"""Two fixed exploratory hypotheses on shared causal features, never on reserved audio."""
from __future__ import annotations

import argparse
import json
from pathlib import Path
import time

import numpy as np

from .kick_attack_rejection import sha
from .kick_fusion_calibration import nested_calibration, select_fires, threshold_scores
from .kick_fusion_fine_calibration import refine_outer
from .kick_trajectory_features import FEATURE_NAMES, cached_features
from .run_kick_dsp_experiments import ROOT, development_sources
from .run_kick_fusion_calibration import comparisons, summarise
from .run_kick_fusion_trial import fit_without, predict_score, training_labels


def load_records(audio_root, baseline, cache):
    sources = development_sources(audio_root)
    labels = ROOT/'tests/fixtures/audio_labels'
    for name, digest in baseline['label_sha256'].items():
        if sha(labels/name) != digest:
            raise ValueError(f'frozen labels changed: {name}')
    for t in json.loads((labels/'heldout_passages_2026-10-09.json').read_text())['tracks']:
        sources.append(dict(track=t['track'], group='former_heldout', audio_path=t['master_path'],
                            audio_sha256=t['audio_sha256'], passages=t['passages']))
    references = {t['track']: t for t in baseline['tracks']}
    records = []
    for source in sources:
        ref = references[source['track']]
        if ref['audio_sha256'] != source['audio_sha256']:
            raise ValueError('reference source mismatch')
        cached = cached_features(source['audio_path'], source['audio_sha256'], cache)
        meta = cached['metadata']; sr, hop = meta['sample_rate'], meta['hop']
        mask, y, ids = training_labels(source, ref, cached['candidates'], cached['available'],
                                      sr, hop, meta['duration_s'])
        records.append(dict(track=source['track'], source=source, ref=ref, sample_rate=sr, hop=hop,
            candidates=cached['candidates'], available=cached['available'], features=cached['features'],
            training_mask=mask, labels=y, event_ids=ids, cache_metadata=meta))
        print('cache', source['track'], 'hit' if cached['cache_hit'] else 'built', flush=True)
    return records


def verify_base_replay(records, reference):
    rows = {t['track']: t for t in reference['tracks']}
    for r in records:
        row = rows[r['track']]
        scores = predict_score(row['model'], r['features'][:, :15])
        for name, cutoff in (('fixed_05', .5), ('calibrated', row['calibration']['threshold'])):
            fires = select_fires(scores, r['available'], r['sample_rate'], r['hop'], cutoff)
            if fires != row['kick_hops'][name]:
                raise ValueError(f'frozen fifteen-feature replay differs: {r["track"]}/{name}')


def negative_minutes(source):
    """Reviewed kick-free core duration, subtracting merged uncertain intervals."""
    if source['group'] == 'original_five':
        return 0.0
    seconds = 0.0
    for p in source['passages']:
        start, end = p['start_s'], p['end_s']
        if any(start <= t < end for t in p['kick_times_s']):
            continue
        intervals = sorted((max(start, x['start_s']-.07), min(end, x['end_s']+.2))
                           for x in p.get('uncertain_regions', [])
                           if x['end_s']+.2 > start and x['start_s']-.07 < end)
        excluded = 0.0; previous_end = start
        for left, right in intervals:
            excluded += max(0, right-max(left, previous_end))
            previous_end = max(previous_end, right)
        seconds += end-start-excluded
    return seconds/60


def diagnostics(scores, source):
    wide = [p['association_early_35_late_200_ms'] for p in scores]
    potential_duplicates = sum(len(w['possible_duplicate_times_s']) if 'possible_duplicate_times_s' in w
        else sum(any(-.035 <= t-p['attack_s'] <= .2 for p in w['pairs']) for t in w['extra_times_s'])
        for w in wide)
    delays = [p['delay_ms'] for w in wide for p in w['pairs']]
    minutes = negative_minutes(source)
    extras = sum(p['accuracy_by_tolerance_ms']['70']['extra'] for p in scores if p['labels'] == 0)
    return dict(possible_duplicate_triggers=potential_duplicates,
                p95_associated_delay_ms=float(np.quantile(delays, .95)) if delays else None,
                kick_free_minutes=minutes, kick_free_extras=extras,
                kick_free_false_triggers_per_minute=extras/minutes if minutes else None)


def run_variant(records, variant, fit, predict):
    tracks = nested_calibration(records, fit=fit, predict=predict)
    for row, record in zip(tracks, records):
        refined = refine_outer(records, row['calibration'], predict=predict)
        started = time.process_time()
        scores = predict(row['model'], record['features'])
        prediction_cpu = time.process_time()-started
        fires, passages = threshold_scores(record, scores, refined['threshold'])
        row['scores']['refined'] = passages
        row['kick_hops']['refined'] = fires
        row['refinement'] = refined
        row['diagnostics'] = diagnostics(passages, record['source'])
        row['comparison_to_prototype'] = comparisons(row['scores']['baseline'], passages)
        row['prediction_cpu_s'] = prediction_cpu
        row['sample_rate'], row['hop'] = record['sample_rate'], record['hop']
        row['cache_metadata'] = record['cache_metadata']
        print(variant, row['track'], refined['threshold'], summarise(passages)['tolerance_ms']['70'], flush=True)
    totals = {v: summarise([p for t in tracks for p in t['scores'][v]])
              for v in ('baseline', 'fixed_05', 'calibrated', 'refined')}
    minutes = sum(t['diagnostics']['kick_free_minutes'] for t in tracks)
    extras = sum(t['diagnostics']['kick_free_extras'] for t in tracks)
    return dict(variant=variant, tracks=tracks, totals=totals,
        kick_free_minutes=minutes, kick_free_extras=extras,
        kick_free_false_triggers_per_minute=extras/minutes if minutes else None,
        possible_duplicate_triggers=sum(t['diagnostics']['possible_duplicate_triggers'] for t in tracks))


def run(audio_root, baseline_path, reference_path, cache, out):
    from .kick_subspace_score import fit_without as subspace_fit, predict_score as subspace_predict
    baseline, reference = (json.loads(p.read_text()) for p in (baseline_path, reference_path))
    records = load_records(audio_root, baseline, cache)
    verify_base_replay(records, reference)
    variants = [run_variant(records, 'linear_39', fit_without, predict_score),
                run_variant(records, 'svd_39_rank2', subspace_fit, subspace_predict)]
    files = ('run_kick_trajectory_trial.py', 'kick_trajectory_features.py', 'kick_subspace_score.py',
             'kick_fusion_calibration.py', 'kick_fusion_fine_calibration.py',
             'run_kick_fusion_trial.py', 'run_kick_dsp_experiments.py',
             'run_kick_fusion_calibration.py', 'live_kick_baseline.py', 'master_kick_comparison.py')
    report = dict(hypotheses=[
        'Representation: retain eight ordered low/body/upper power observations relative to pre-candidate background, alongside the frozen15 features; keep regularised linear scorer unchanged.',
        'Decision rule: on the exact same39 values and candidate windows, compare class-conditional rank2 weightedSVD reconstruction residuals against linear fusion.'],
        fixed='Same candidates,40ms horizon rounded up to hop boundary,60ms refractory, labels, song/class/event weighting, nested song exclusions and coarse-plus-one-refinement cutoff protocol. One configuration per hypothesis; no additional search.',
        acceptance='At least166/174 matched within70ms and no more than8 unmatched on the reference pack, >=90% recall each track, allfourkickfreecoreszero; expandedlocalcorpus and reservedvalidation ALSO required. Neither this reference result nor its absence proves generalisation.',
        feature_names=list(FEATURE_NAMES), source_sha256={f:sha(ROOT/'tools/audio_analysis/eval'/f) for f in files},
        baseline_sha256=sha(baseline_path), reference_sha256=sha(reference_path),
        label_sha256=baseline['label_sha256'], frozen_base_replay_exact=True, variants=variants,
        limitations='Exploratory nine-song development only. Provisional labels, no reserved validation yet. SVD residuals and logistic scores are not calibrated probabilities. Possible duplicates are temporal associations, not source identifications. CPU timings are offline throughput, not native callback guarantees. No extra observation delay, GPU or live integration.')
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(report, indent=2)+'\n')
    print('TOTALS', {v['variant']:v['totals']['refined'] for v in variants}, flush=True)
    return report


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('audio-root', 'baseline', 'reference', 'cache', 'out'):
        parser.add_argument('--'+name, type=Path, required=True)
    a = parser.parse_args()
    run(a.audio_root, a.baseline, a.reference, a.cache, a.out)
