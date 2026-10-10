"""Score frozen full-recording outputs on separately reviewed local passages.

No fitting, thresholds, feature extraction or reserved recordings are used here.
"""
from __future__ import annotations

import argparse
import json
import math
from pathlib import Path

from .live_kick_baseline import delay_stats, sha
from .master_kick_comparison import score_passage


COMPARISONS = (
    ('linear_15', 'linear_15'),
    ('boosted_depth3', 'boosted_depth3'),
    ('anchored_tree_0.75', 'anchored_tree_0.75'),
    ('anchored_075_zero_core', 'anchored_075_zero_core'),
)


def score_reviewed_core(predictions, passage):
    """Retain the usual matching and uncertainty rules at a recording boundary.

    A recording starting at sample zero has no earlier input or emitted events.
    The legacy scorer requires a 200ms left review margin even there. Extend its
    *empty time domain*, not the audio or labels, for this call only. Keep the
    real review bounds in the manifest and report this exception explicitly.
    """
    if not all(math.isfinite(t) and t >= 0 for t in predictions):
        raise ValueError('predictions must be nonnegative, finite emission times')
    recording_start = passage['start_s'] == 0 and passage['review_start_s'] == 0
    scored = passage
    if recording_start:
        coordinate = passage['raw_excerpt_coordinates']['master']
        if coordinate['source_sample_range'][0] != 0 or coordinate['master_axis_origin_s'] != 0:
            raise ValueError('opening core must begin at actual recording sample zero')
        if any(t < 0 for t in passage['kick_times_s']):
            raise ValueError('no pre-recording labels are allowed')
        scored = dict(passage, review_start_s=-.2)
    result = score_passage(predictions, scored)
    result['recording_start_boundary'] = recording_start
    result['actual_review_bounds_s'] = [passage['review_start_s'], passage['review_end_s']]
    return result


def aggregate(rows):
    pairs = [p for r in rows for p in r['association_early_35_late_200_ms']['pairs']]
    return dict(labels=sum(r['labels'] for r in rows),
        accuracy_by_tolerance_ms={str(ms):{k:sum(r['accuracy_by_tolerance_ms'][str(ms)][k] for r in rows)
            for k in ('matched','missed','extra')} for ms in (35,50,70)},
        association_early_35_late_200_ms={k:sum(r['association_early_35_late_200_ms'][k] for r in rows)
            for k in ('matched','missed','extra')},
        association_delay=delay_stats([p['delay_ms'] for p in pairs]),
        late_associations_over_70ms=sum(p['delay_ms'] > 70 for p in pairs))


def assert_holdout(model, track):
    if track in model['training_tracks']:
        raise ValueError('validation family was used to fit its scorer')
    for value in model.values():
        if isinstance(value, dict) and 'training_tracks' in value:
            assert_holdout(value, track)


def run(selection_path, labels_path, out):
    selection = json.loads(selection_path.read_text())
    labels = json.loads(labels_path.read_text())
    if labels['status'] != 'lead_reviewed_visual_provisional':
        raise ValueError('lead review is required before scoring')
    selected = sorted((c['track'],c['id'],c['start_s'],c['end_s']) for c in selection['cores'])
    reviewed = sorted((t['track'],c['id'],c['start_s'],c['end_s'])
        for t in labels['tracks'] for c in t['cores'])
    if selected != reviewed:
        raise ValueError('preselected cores changed')
    reports = []
    for name,digest in selection['frozen_model_reports'].items():
        path = Path(name)
        if sha(path) != digest:
            raise ValueError(f'frozen report changed: {path}')
        reports.append(json.loads(path.read_text()))
    output = []
    for name,variant_name in COMPARISONS:
        matches = [v for report in reports for v in report['variants'] if v['variant']==variant_name]
        if len(matches) != 1:
            raise ValueError(f'exactly one frozen variant required: {variant_name}')
        variant = matches[0]
        rows = []
        exclusions = []
        for track in labels['tracks']:
            fitted = next(r for r in variant['tracks'] if r['track']==track['track'])
            if fitted['cache_metadata']['signature']['audio_sha256'] != track['audio_sha256']:
                raise ValueError('reviewed audio differs from frozen full-recording output')
            assert_holdout(fitted['model'],track['track'])
            for key in ('calibration','refinement'):
                if track['track'] in fitted[key]['threshold_selection_tracks']:
                    raise ValueError('validation family used in cutoff selection')
            predictions = [(i+1)*fitted['hop']/fitted['sample_rate'] for i in fitted['kick_hops']['refined']]
            for core in track['cores']:
                if not core['scoring_ready']:
                    exclusions.append(dict(track=track['track'],passage=core['id'],reason=core['reason']))
                    continue
                rows.append(dict(track=track['track'],passage=core['id'],
                    start_s=core['start_s'],end_s=core['end_s'],
                    **score_reviewed_core(predictions,core)))
        negative = [r for r in rows if r['labels']==0]
        output.append(dict(variant=name,totals=aggregate(rows),
            per_track={t:aggregate([r for r in rows if r['track']==t]) for t in sorted({r['track'] for r in rows})},
            negative_cores={r['passage']:r['accuracy_by_tolerance_ms']['70']['extra'] for r in negative},
            passages=rows,unscored=exclusions))
    report = dict(method=__doc__,selection_sha256=sha(selection_path),labels_sha256=sha(labels_path),
        frozen_model_reports=selection['frozen_model_reports'],
        source_sha256={n:sha(Path(__file__).parent/n) for n in
            ('verify_kick_evening_passages.py','master_kick_comparison.py','live_kick_baseline.py')},
        variants=output,
        limitations='Provisional visual labels on additional passages of development families, not reserved-song '
            'confirmation. No tuning on these results. The existing381 labels remain unchanged. Two Late Night '
            'cores are unscorable, not negative. Uncertain audio and its possible responses are excluded '
            'identically for every scorer. Recording-start cores retain all actual nonnegative emissions; '
            'a virtual empty pre-recording domain only satisfies the legacy matcher margin contract.')
    out.parent.mkdir(parents=True,exist_ok=True)
    out.write_text(json.dumps(report,indent=2)+'\n')
    for v in output:
        print(v['variant'],v['totals']['labels'],v['totals']['accuracy_by_tolerance_ms']['70'],v['negative_cores'])
    return report


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('selection','labels','out'):
        parser.add_argument('--'+name,type=Path,required=True)
    args = parser.parse_args()
    run(args.selection,args.labels,args.out)
