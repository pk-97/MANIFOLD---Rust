"""Score reviewed master passages using full-song detector output.

Input runs come from the existing Rust harnesses, without resets or time shifts.
Review margins participate in matching but do not contribute scored labels or
extra triggers. This prevents a near-boundary kick from becoming a false fire.
"""
from __future__ import annotations

import argparse
import json
from pathlib import Path

from .live_kick_baseline import delay_stats, match_events, sha


def score_passage(predictions, passage):
    start, end = passage['start_s'], passage['end_s']
    review_start, review_end = passage['review_start_s'], passage['review_end_s']
    if not review_start <= start - .2 < end + .2 <= review_end:
        raise ValueError('at least 200ms of reviewed context is required')
    truth = passage['kick_times_s']
    if truth != sorted(set(truth)) or not all(review_start <= t <= review_end for t in truth):
        raise ValueError('labels must be sorted, unique, and inside reviewed context')
    regions = passage.get('uncertain_regions', [])
    # The annotations describe uncertain audio, not a detector decision window.
    # Include possible responses to it: widest early tolerance and late diagnostic.
    excluded = [dict(start_s=r['start_s'] - .07, end_s=r['end_s'] + .2,
                     reason=r['reason']) for r in regions]
    def valid(t):
        return not any(r['start_s'] <= t <= r['end_s'] for r in excluded)
    ignored_labels = [t for t in truth if not valid(t)]
    truth = [t for t in truth if valid(t)]
    pred = sorted(p for p in predictions if review_start <= p <= review_end and valid(p))
    def core(t):
        return start <= t < end
    scored_truth = {i for i,t in enumerate(truth) if core(t)}
    def score(early, late):
        pairs = match_events(pred, truth, early, late)
        matched_truth = {t for t,p in pairs}
        matched_pred = {p for t,p in pairs}
        core_pairs = [dict(attack_s=truth[t], available_s=pred[p],
                           delay_ms=round(1000*(pred[p]-truth[t]),3))
                      for t,p in pairs if t in scored_truth]
        missed = [truth[t] for t in sorted(scored_truth - matched_truth)]
        extras = [p for i,p in enumerate(pred) if core(p) and i not in matched_pred]
        return dict(matched=len(core_pairs), missed=len(missed), extra=len(extras),
                    missed_times_s=missed, extra_times_s=extras, pairs=core_pairs,
                    **delay_stats([p['delay_ms'] for p in core_pairs]))
    return dict(labels=len(scored_truth), excluded_regions=excluded,
                excluded_label_times_s=ignored_labels,
                accuracy_by_tolerance_ms={str(ms):score(ms/1000,ms/1000) for ms in (35,50,70)},
                association_early_35_late_200_ms=score(.035,.2))


def main():
    ap=argparse.ArgumentParser(description=__doc__)
    ap.add_argument('--runs',required=True,type=Path)
    ap.add_argument('--labels',required=True,type=Path)
    ap.add_argument('--out',required=True,type=Path)
    args=ap.parse_args()
    runs=json.loads(args.runs.read_text())
    labels=json.loads(args.labels.read_text())
    rows=[]
    for track in labels['tracks']:
        matches=[r for r in runs if r['track']==track['track']]
        if {r['detector'] for r in matches}!={'mod_harness','kick_attack_probe'} or len(matches)!=2:
            raise ValueError('exactly one run per detector required')
        for run in matches:
            if run['audio_sha256']!=track['audio_sha256']:
                raise ValueError('master audio hash differs from reviewed source')
            expected=[(i+1)*run['hop_samples']/run['sample_rate'] for i in run['kick_hops']]
            if expected!=run['raw_available_times_s']:
                raise ValueError('availability must use the unshifted hop end')
            for passage in track['passages']:
                rows.append(dict(track=track['track'],detector=run['detector'],
                                 passage=passage['id'],start_s=passage['start_s'],end_s=passage['end_s'],
                                 **score_passage(expected,passage)))
    totals={}
    for detector in ('mod_harness','kick_attack_probe'):
        selected=[r for r in rows if r['detector']==detector]
        pairs=[p for r in selected for p in r['association_early_35_late_200_ms']['pairs']]
        totals[detector]=dict(labels=sum(r['labels'] for r in selected),
            accuracy_by_tolerance_ms={str(ms):{k:sum(r['accuracy_by_tolerance_ms'][str(ms)][k] for r in selected)
                                    for k in ('matched','missed','extra')} for ms in (35,50,70)},
            association_early_35_late_200_ms={k:sum(r['association_early_35_late_200_ms'][k] for r in selected)
                                            for k in ('matched','missed','extra')},
            association_delay=delay_stats([p['delay_ms'] for p in pairs]))
    report=dict(method='Full-master native-rate stereo mean; fixed detector settings; no resets, calibration or backdating. Visual provisional labels, not audited listening truth. One-to-one matching uses reviewed margins; counts use core passage only. Uncertain audio excludes possible responses from -70ms to +200ms. Timing is sample availability, not app latency.',
                runs_sha256=sha(args.runs),labels_sha256=sha(args.labels),totals=totals,passages=rows)
    args.out.write_text(json.dumps(report,indent=2)+'\n')
    print(json.dumps(totals,indent=2))


if __name__=='__main__':
    main()
