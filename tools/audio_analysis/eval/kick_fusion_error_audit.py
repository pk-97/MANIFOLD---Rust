"""Replay frozen fusion decisions and audit their evidence, without fitting or tuning."""
from __future__ import annotations

import argparse
import csv
import json
from pathlib import Path

import numpy as np
from scipy.special import expit

from .kick_attack_rejection import causal_features, read_audio, sha
from .kick_fusion_bandwise import FEATURE_NAMES, fusion_features
from .kick_fusion_calibration import select_fires
from .run_kick_dsp_experiments import ROOT
from .run_kick_fusion_trial import predict_score

FOCUS = ('bad_guy_128bpm', 'midnight_patience')


def availability_hop(seconds, sr, hop):
    index = round(seconds * sr / hop) - 1
    if abs((index + 1) * hop / sr - seconds) > 1e-9:
        raise ValueError('event time is not an actual completed-hop availability')
    return index


def contributions(model, features):
    z = np.clip((features - np.asarray(model['mean'])) / np.asarray(model['scale']), -8, 8)
    terms = z * np.asarray(model['weights'])
    logits = terms.sum(axis=1) + model['intercept']
    np.testing.assert_allclose(expit(logits), predict_score(model, features), atol=1e-14)
    return terms, logits


def scored_labels(passages, original_truth=None):
    if original_truth is not None:
        return sorted(original_truth)
    # Tight and wide matching can select different labels: their union alone
    # is insufficient unless tight matches AND tight misses are represented.
    return sorted({pair['attack_s'] for p in passages
                   for pair in p['accuracy_by_tolerance_ms']['50']['pairs']}
                  | {t for p in passages
                     for t in p['accuracy_by_tolerance_ms']['50']['missed_times_s']})


def annotate_label_context(events, passages, labels):
    excluded = sorted({t for p in passages for t in p.get('excluded_label_times_s', [])})
    pairs = [pair for p in passages for pair in p['association_early_35_late_200_ms']['pairs']]
    for event in events:
        closest = min(labels, key=lambda t: abs(event['candidate_s'] - t)) if labels else None
        event['nearest_scored_label_s'] = closest
        event['nearest_scored_label_delta_ms'] = (
            (event['candidate_s'] - closest) * 1000 if closest is not None else None)
        event['nearest_scored_label_available_delta_ms'] = (
            (event['available_s'] - closest) * 1000 if closest is not None else None)
        event['nearest_excluded_label_s'] = min(
            excluded, key=lambda t: abs(event['candidate_s'] - t)) if excluded else None
        event['nearest_excluded_label_candidate_delta_ms'] = (
            (event['candidate_s'] - event['nearest_excluded_label_s']) * 1000 if excluded else None)
        event['nearest_label_assigned_available_s'] = next(
            (p['available_s'] for p in pairs if p['attack_s'] == closest), None)


def classify_events(fires, passages, sr, hop):
    """Preserve frozen wide matching, exclusions, and reviewed core semantics.

    Master passages supply explicit core extras, because a reviewed-margin match
    need not appear among their core pairs. Original clips have no review margin.
    """
    fire_set = set(fires)
    rows = []
    for passage in passages:
        wide = passage['association_early_35_late_200_ms']
        matched = {availability_hop(p['available_s'], sr, hop): p for p in wide['pairs']}
        if 'extra_times_s' in wide:
            extras = {availability_hop(t, sr, hop) for t in wide['extra_times_s']}
        else:
            excluded = passage['excluded']
            extras = {i for i in fires if i not in matched and not any(
                r['start_s'] <= (i + 1) * hop / sr <= r['end_s'] for r in excluded)}
        if not (set(matched) | extras) <= fire_set or set(matched) & extras:
            raise ValueError('frozen classifications do not match emitted hops')
        expected = wide.get('extra', wide.get('unmatched_triggers'))
        if len(extras) != expected or len(matched) != wide['matched']:
            raise ValueError('frozen event counts do not reconstruct')
        for index in sorted(set(matched) | extras):
            pair = matched.get(index)
            rows.append(dict(passage=passage['id'], available_hop=index,
                             classification='matched' if pair else 'wide_extra',
                             attack_s=pair['attack_s'] if pair else None))
    return rows


def distribution(values):
    return dict(count=len(values), minimum=float(np.min(values)),
                q25=float(np.quantile(values, .25)), median=float(np.median(values)),
                q75=float(np.quantile(values, .75)), maximum=float(np.max(values))) if len(values) else None


def summaries(events):
    groups = {}
    for kind in ('matched', 'wide_extra'):
        selected = [e for e in events if e['classification'] == kind]
        groups[kind] = dict(count=len(selected), score=distribution([e['score'] for e in selected]),
            features={name: distribution([e['features'][name] for e in selected]) for name in FEATURE_NAMES},
            contributions={name: distribution([e['logit_contributions'][name] for e in selected])
                           for name in FEATURE_NAMES})
    return groups


def envelope_window(power, candidate_s, deadline_s, sr, hop):
    """Diagnostic source envelope on its native grid; no decision times are changed."""
    start = round(candidate_s * sr / hop) - 1
    end = round(deadline_s * sr / hop) - 1
    if start < 0 or end >= len(power):
        raise ValueError('source event window is outside audio')
    first = power[start:min(end + 1, start + 3)]
    last = power[max(start, end - 2):end + 1]
    pre = power[max(0, start - max(1, round(.050 * sr / hop))):start + 1]
    whole = power[start:end + 1]
    return dict(first_mean=float(np.mean(first)), last_mean=float(np.mean(last)),
                window_peak=float(np.max(whole)), pre50ms_peak=float(np.max(pre)),
                peak_offset_ms=float(np.argmax(whole) * hop / sr * 1000),
                last_over_first=float((np.mean(last) + 1e-12) / (np.mean(first) + 1e-12)),
                native_grid_rounding_error_ms=float(((start + 1) * hop / sr - candidate_s) * 1000))


def add_stem_evidence(row, stems, offset):
    provenance = []
    for name, path, expected_sha in stems:
        if sha(path) != expected_sha:
            raise ValueError(f'stem hash mismatch: {path}')
        sr, audio = read_audio(path)
        envelopes, hop = causal_features(audio, sr)
        power = envelopes[:, :2, 0].sum(axis=1)
        for event in row['events']:
            event.setdefault('source_envelopes', {})[name] = envelope_window(
                power, event['candidate_s'] - offset, event['available_s'] - offset, sr, hop)
        provenance.append(dict(name=name, path=str(path), sha256=expected_sha, sample_rate=sr,
                               frames=len(audio), stem_to_master_seconds=offset))
    for event in row['events']:
        total = sum(e['window_peak'] for e in event['source_envelopes'].values())
        for entry in event['source_envelopes'].values():
            entry['share_of_stem_window_peaks'] = entry['window_peak'] / total if total else 0.0
    row['stem_provenance'] = provenance
    row['stem_evidence_qualification'] = (
        'Independent stem low+body (45–400 Hz) fast-envelope window peaks, not power fractions '
        'of the mastered mixture. Native source grids use nearest-hop timing, error recorded. '
        'A source contribution does not identify the instrument or prove causality.')


def bad_guy_sources(row, audio_root):
    evidence_path = ROOT / 'tools/audio_analysis/eval/scoreboard/kick_rejection_trial_2026-10-09.json'
    prior = next(t for t in json.loads(evidence_path.read_text())['tracks'] if t['track'] == row['track'])
    if prior['input_sha256']['mix'] != row['audio_sha256']:
        raise ValueError('ablation reference mix differs from frozen fusion mix')
    stems = prior['ablation']['stems']
    add_stem_evidence(row, [(name, audio_root / row['track'] / f'{name}.wav', p['input_sha256'])
                           for name, p in stems.items()], 0.0)
    ablations = []
    for name, provenance in stems.items():
        path = Path(provenance['ablation_path'])
        if sha(path) != provenance['ablation_sha256']:
            raise ValueError('cached ablation hash differs')
        sr, audio = read_audio(path)
        candidates, available, features, hop = fusion_features(audio, sr)
        if sr != row['sample_rate'] or hop != row['hop'] or len(audio) != row['frames']:
            raise ValueError('cached ablation timebase differs')
        fires = select_fires(predict_score(row['model'], features), available, sr, hop, row['threshold'])
        times = np.asarray([(i + 1) * hop / sr for i in fires])
        for event in row['events']:
            event.setdefault('stem_subtraction', {})[name] = dict(
                closest_fire_delta_ms=float(times[np.argmin(abs(times - event['available_s']))]
                                            - event['available_s']) * 1000 if len(times) else None,
                retained_fire_within_30ms=bool(np.any(abs(times - event['available_s']) < .030)))
        ablations.append(dict(stem=name, path=str(path), sha256=provenance['ablation_sha256'],
                              kick_hops=fires, candidates=len(candidates)))
    row['ablation_provenance'] = dict(prior_report=str(evidence_path), sha256=sha(evidence_path),
        runs=ablations, qualification='Frozen fusion model and cutoff replayed on cached mix-minus-stem '
        'signals. Nearby output survival can include a different candidate; this is local signal '
        'dependence, not instrument identity. No refitting, new cutoffs, or labels.')


def midnight_sources(row):
    path = ROOT / 'tools/audio_analysis/eval/additional_stem_sources.json'
    registry = json.loads(path.read_text())
    source = next(s for s in registry['sets'] if s['folder'] == 'MIDNIGHT PATIENCE STEMS')
    if source['master']['sha256'] != row['audio_sha256']:
        raise ValueError('registry master differs from frozen fusion audio')
    offset = source['alignment']['stem_to_master_seconds']
    stems = [(Path(s['file']).stem, Path(registry['source_root']) / s['file'], s['sha256'])
             for s in source['files'] if s['role'] == 'named_stem']
    add_stem_evidence(row, stems, offset)
    row['source_alignment'] = dict(registry=str(path), sha256=sha(path), **source['alignment'])


def run(report_path, audio_root, out):
    frozen = json.loads(report_path.read_text())
    if frozen['feature_names'] != list(FEATURE_NAMES):
        raise ValueError('expected the frozen fifteen-feature report')
    for name, expected in frozen['label_sha256'].items():
        if sha(ROOT / 'tests/fixtures/audio_labels' / name) != expected:
            raise ValueError(f'frozen labels changed: {name}')
    # Threshold calibration may be extended independently; only the frozen
    # feature/model implementations are dependencies of this stored-model replay.
    dependencies = ('kick_fusion_features.py', 'kick_fusion_bandwise.py', 'kick_attack_rejection.py',
                    'kick_tonal_experiment.py', 'kick_upper_cue.py', 'run_kick_fusion_trial.py')
    for name in dependencies:
        relative = f'tools/audio_analysis/eval/{name}'
        if sha(ROOT / relative) != frozen['source_sha256'][relative]:
            raise ValueError(f'frozen feature/model dependency changed: {name}')
    rows = []
    for recorded in frozen['tracks']:
        path = (audio_root / recorded['track'] / 'mix.wav' if recorded['group'] == 'original_five'
                else Path(recorded['audio_path']))
        if sha(path) != recorded['audio_sha256']:
            raise ValueError(f'frozen mix hash mismatch: {path}')
        sr, audio = read_audio(path)
        candidates, available, features, hop = fusion_features(audio, sr)
        threshold = recorded['calibration']['threshold']
        model = recorded['model']
        scores = predict_score(model, features)
        fires = select_fires(scores, available, sr, hop, threshold)
        if fires != recorded['kick_hops']['calibrated'] or (sr, hop) != (recorded['sample_rate'], recorded['hop']):
            raise ValueError(f'exact replay failed: {recorded["track"]}')
        terms, logits = contributions(model, features)
        events = classify_events(fires, recorded['scores']['calibrated'], sr, hop)
        indices = {int(a): i for i, a in enumerate(available)}
        original_truth = None
        if recorded['group'] == 'original_five':
            label_path = ROOT / 'tests/fixtures/audio_labels' / f'{recorded["track"]}.csv'
            with label_path.open() as handle:
                original_truth = [float(r['mix_time_s']) for r in csv.DictReader(handle)]
        labels = scored_labels(recorded['scores']['calibrated'], original_truth)
        envelopes = causal_features(audio, sr)[0] if recorded['track'] in FOCUS else None
        for event in events:
            i = indices[event['available_hop']]
            event.update(candidate_hop=int(candidates[i]), candidate_s=float((candidates[i] + 1) * hop / sr),
                available_s=float((available[i] + 1) * hop / sr), score=float(scores[i]),
                logit=float(logits[i]), intercept=model['intercept'],
                features=dict(zip(FEATURE_NAMES, features[i].tolist())),
                logit_contributions=dict(zip(FEATURE_NAMES, terms[i].tolist())))
            if envelopes is not None:
                event['mix_envelopes'] = {name: envelope_window(envelopes[:, band, 0],
                    event['candidate_s'], event['available_s'], sr, hop)
                    for band, name in enumerate(('low', 'body', 'mid'))}
        annotate_label_context(events, recorded['scores']['calibrated'], labels)
        row = dict(track=recorded['track'], audio_path=str(path), audio_sha256=recorded['audio_sha256'],
                   frames=len(audio), sample_rate=sr, hop=hop, threshold=threshold,
                   exact_frozen_replay=True, emitted_hops=len(fires), model=model,
                   summaries=summaries(events), events=events)
        if recorded['track'] == 'bad_guy_128bpm':
            bad_guy_sources(row, audio_root)
        elif recorded['track'] == 'midnight_patience':
            midnight_sources(row)
        rows.append(row)
        print('audited', row['track'], {k: v['count'] for k, v in row['summaries'].items()}, flush=True)
    result = dict(method=__doc__, frozen_report=str(report_path), frozen_sha256=sha(report_path),
        feature_names=list(FEATURE_NAMES), tracks=rows,
        audit_source_sha256=sha(Path(__file__)),
        limitations='Frozen wide-association classification; temporal association is not causal proof. '
        'All nine songs already informed development. No new labels, models, cutoffs or live changes. '
        'Feature contributions are signed standardized linear-logit terms, not causal importances. '
        'No auditory review. Spectral/energy evidence alone cannot distinguish kick from other drums.')
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(result, indent=2) + '\n')
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--report', required=True, type=Path)
    parser.add_argument('--audio-root', required=True, type=Path)
    parser.add_argument('--out', required=True, type=Path)
    args = parser.parse_args()
    run(args.report, args.audio_root, args.out)


if __name__ == '__main__':
    main()
