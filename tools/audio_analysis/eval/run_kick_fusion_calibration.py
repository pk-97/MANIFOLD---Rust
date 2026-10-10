"""One fixed nested threshold calibration experiment on nine explored development songs.

Outer songs are excluded from all inner fits, normalisation, and threshold choice.
The logistic model, candidate proposer, 40ms horizon and 60ms refractory are frozen.
"""
from __future__ import annotations

import argparse
import inspect
import json
from pathlib import Path
import time

import numpy as np

from .kick_attack_rejection import read_audio, sha
from .kick_excess_balance_trial import compact_score
from .kick_fusion_calibration import THRESHOLDS, nested_calibration
from .kick_fusion_features import FEATURE_NAMES, fusion_features
from .live_kick_baseline import delay_stats
from .run_kick_dsp_experiments import ROOT, development_sources
from .run_kick_fusion_trial import training_labels


def summarise(passages):
    return dict(labels=sum(p['labels'] for p in passages),
        tolerance_ms={str(ms): {key: sum(p['accuracy_by_tolerance_ms'][str(ms)][key]
                                       for p in passages)
                               for key in ('matched', 'missed', 'extra')} for ms in (35, 50, 70)},
        association={key: sum(compact_score(p)['association'][key] for p in passages)
                     for key in ('matched', 'missed', 'extra')},
        association_delay=delay_stats([pair['delay_ms'] for p in passages
                                       for pair in p['association_early_35_late_200_ms']['pairs']]))


def comparisons(baseline, proposed):
    rows = []
    for old, new in zip(baseline, proposed):
        if old['id'] != new['id']:
            raise ValueError('passage identity mismatch')
        metrics = {}
        for key in ('35', '50', '70', 'association'):
            if key == 'association':
                before = {pair['attack_s'] for pair in old['association_early_35_late_200_ms']['pairs']}
                after = {pair['attack_s'] for pair in new['association_early_35_late_200_ms']['pairs']}
                lost, recovered = before - after, after - before
            else:
                # Original-five strict scores intentionally omit pair details.
                before = set(old['accuracy_by_tolerance_ms'][key]['missed_times_s'])
                after = set(new['accuracy_by_tolerance_ms'][key]['missed_times_s'])
                lost, recovered = after - before, before - after
            metrics[key] = dict(lost_labels_s=sorted(lost), recovered_labels_s=sorted(recovered))
        rows.append(dict(passage=old['id'], metrics=metrics))
    return rows


def run(audio_root, baseline_path, out, extractor=fusion_features, variant='nine_feature',
        feature_names=FEATURE_NAMES, extractor_paths=()):
    """Run a single extractor variant with the identical nested calibration protocol.

    Extractors return (candidate_hops, actual_available_hops, feature_matrix, hop).
    Pass feature_names and all additional dependency paths for variant provenance.
    """
    audio_root, baseline_path, out = map(Path, (audio_root, baseline_path, out))
    baseline = json.loads(baseline_path.read_text())
    refs = {t['track']: t for t in baseline['tracks']}
    sources = development_sources(audio_root)
    label_root = ROOT / 'tests/fixtures/audio_labels'
    for t in json.loads((label_root / 'heldout_passages_2026-10-09.json').read_text())['tracks']:
        sources.append(dict(track=t['track'], group='former_heldout', audio_path=t['master_path'],
                            audio_sha256=t['audio_sha256'], passages=t['passages']))
    for name, digest in baseline['label_sha256'].items():
        if sha(label_root / name) != digest:
            raise ValueError(f'frozen labels changed: {name}')
    if len(sources) != 9 or {s['track'] for s in sources} != set(refs):
        raise ValueError('expected the exact frozen nine-song development corpus')
    records = []
    for source in sources:
        ref = refs[source['track']]
        if source['audio_sha256'] != ref['audio_sha256'] or sha(Path(source['audio_path'])) != ref['audio_sha256']:
            raise ValueError('audio differs from frozen reference')
        sr, audio = read_audio(Path(source['audio_path']))
        started = time.process_time()
        candidates, available, features, hop = extractor(audio, sr)
        cpu = time.process_time() - started
        if features.shape != (len(candidates), len(feature_names)) or len(available) != len(candidates):
            raise ValueError('invalid extractor shape')
        if not np.all(np.isfinite(features)) or np.any(available < candidates):
            raise ValueError('invalid features or availability')
        mask, y, ids = training_labels(source, ref, candidates, available, sr, hop, len(audio) / sr)
        records.append(dict(track=source['track'], source=source, ref=ref, sample_rate=sr, hop=hop,
            candidates=candidates, available=available, features=features, training_mask=mask,
            labels=y, event_ids=ids, duration_s=len(audio) / sr, feature_cpu_s=cpu))
        print('features', source['track'], len(candidates), 'training', int(mask.sum()), flush=True)
    tracks = nested_calibration(records)
    for row, record in zip(tracks, records):
        row.update(group=record['source']['group'], audio_path=record['source']['audio_path'],
                   audio_sha256=record['source']['audio_sha256'], sample_rate=record['sample_rate'],
                   hop=record['hop'], duration_s=record['duration_s'],
                   candidates=len(record['candidates']), training_samples=int(record['training_mask'].sum()),
                   training_positive=int(record['labels'][record['training_mask']].sum()),
                   feature_cpu_s=record['feature_cpu_s'])
        row['comparisons'] = {variant: comparisons(row['scores']['baseline'], row['scores'][variant])
                              for variant in ('fixed_05', 'calibrated')}
        print('fold', row['track'], 'threshold', row['calibration']['threshold'],
              {name: summarise(passages)['association'] for name, passages in row['scores'].items()}, flush=True)
    variants = ('baseline', 'fixed_05', 'calibrated')
    totals = {v: summarise([p for t in tracks for p in t['scores'][v]]) for v in variants}
    bass_cores = [dict(track=t['track'], passage=base['id'],
                      scores={v: summarise([t['scores'][v][i]]) for v in variants})
                  for t in tracks for i, base in enumerate(t['scores']['baseline']) if base['labels'] == 0]
    files = ('kick_fusion_calibration.py', 'run_kick_fusion_calibration.py',
             'run_kick_fusion_trial.py', 'kick_fusion_features.py', 'kick_upper_cue.py',
             'kick_attack_rejection.py', 'kick_tonal_experiment.py', 'kick_hybrid_experiment.py',
             'run_kick_dsp_experiments.py', 'kick_excess_balance_trial.py',
             'live_kick_baseline.py', 'master_kick_comparison.py')
    source_paths = [ROOT / 'tools/audio_analysis/eval' / f for f in files]
    source_paths += [Path(inspect.getsourcefile(extractor)), *(Path(p) for p in extractor_paths)]
    report = dict(method=__doc__, variant=variant, feature_names=list(feature_names),
        threshold_grid=list(THRESHOLDS), threshold_objective='Maximise summed inner 70ms matched '
        'subject to summed wide-association extras and summed kick-free-core extras each <= '
        'inner baseline. Ties: fewer wide extras, then higher threshold. 1.01 is silence.',
        training='Outer fit: eight songs. Each inner validation song predicted from the remaining '
        'seven. All fits and standardisation reuse frozen L2=.01, balanced song/class/event '
        'weights, weighted mean/std, std floor=.001 and z clip=8. Training samples restricted '
        'to reviewed cores and frozen exclusions. Actual available hop end; no backdating.',
        totals=totals, kick_free_core_totals={v: summarise([p for t in tracks for p in t['scores'][v]
                                                          if p['labels'] == 0]) for v in variants},
        bass_only_cores=bass_cores, tracks=tracks,
        selected_thresholds={t['track']: t['calibration']['threshold'] for t in tracks},
        source_sha256={str(p.relative_to(ROOT)) if p.is_relative_to(ROOT) else str(p): sha(p)
                       for p in dict.fromkeys(source_paths)},
        label_sha256=baseline['label_sha256'], baseline_path=str(baseline_path),
        baseline_sha256=sha(baseline_path),
        limitations='All nine songs already informed development; exploratory nested cross-validation, '
        'not untouched validation. Labels remain provisional. Wide association is diagnostic only. '
        'Balanced logistic scores are not probabilities. Inner aggregate budgets do not guarantee '
        'outer aggregate or per-core budgets, retained label identities, or native callback behaviour. '
        'Full chronological native-rate audio; no live integration or further threshold search.')
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(report, indent=2) + '\n')
    print('TOTALS', json.dumps(totals), flush=True)
    return report


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--audio-root', type=Path, required=True)
    parser.add_argument('--baseline', type=Path, required=True)
    parser.add_argument('--out', type=Path, required=True)
    args = parser.parse_args()
    run(args.audio_root, args.baseline, args.out)


if __name__ == '__main__':
    main()
