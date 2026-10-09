"""Diagnostic coverage of upper attacks linked to fresh low pulses, not a detector score.

One fixed cascade: upper onset, low onset within -10/+35ms, upper half-power
decay at 50ms. Query +/-50ms around reviewed kicks and original unmatched fires.
These label-centred queries are deliberately diagnostic; they do not establish
causal attribution or production precision/recall. Full kick-free cores expose
unrelated upper attacks that would also pass the cue.
"""
from __future__ import annotations

import argparse
import json
from pathlib import Path

import numpy as np
from scipy.signal import butter, sosfilt

from .kick_attack_rejection import read_audio, sha
from .kick_hybrid_experiment import controls
from .kick_upper_cue import upper_features, inspect_cues
from .run_kick_dsp_experiments import ROOT, development_sources


def query(events, anchor, sr, hop):
    nearby = [e for e in events if abs((e['upper_hop']+1)*hop/sr-anchor) <= .050+1e-12]
    linked = [e for e in nearby if e['low_linked']]
    complete = [e for e in nearby if e['linked_and_decayed']]
    best = min(complete, key=lambda e: abs((e['upper_hop']+1)*hop/sr-anchor)) if complete else None
    return dict(anchor_s=anchor, upper_present=bool(nearby), linked_present=bool(linked),
        complete_present=bool(complete), upper_edges=len(nearby),
        complete_available_delay_ms=(1000*((best['available_hop']+1)*hop/sr-anchor)
                                     if best is not None else None))


def coverage(rows):
    return dict(anchors=len(rows), **{key: sum(r[key] for r in rows)
        for key in ('upper_present', 'linked_present', 'complete_present')})


def run_controls():
    rows = []
    for sr in (44100, 48000):
        reference = {name: audio for name, audio, _ in controls(sr)}
        truth = [.5, 1., 1.5, 2.]
        rng = np.random.default_rng(14)
        n = round(.050*sr)
        noise = sosfilt(butter(2, [1000, 8000], btype='bandpass', fs=sr, output='sos'),
                       rng.normal(size=n))
        noise *= .25*np.exp(-np.arange(n)/sr/.008)
        hats = np.zeros(sr*4)
        notes = np.zeros(sr*4)
        t = np.arange(sr*4)/sr
        bass = .25*np.sin(2*np.pi*80*t)+.35*np.sin(2*np.pi*160*t)+.25*np.sin(2*np.pi*240*t)
        for onset in truth:
            start = round(onset*sr)
            hats[start:start+n] += noise
            length = round(.180*sr)
            envelope = np.minimum(np.arange(length)/sr/.002, 1.)
            notes[start:start+length] += bass[start:start+length]*envelope
        cases = [('stationary_bass', reference['stationary_bass'], []),
                 ('hats_only', hats, truth),
                 ('hats_over_stationary_bass', hats+reference['stationary_bass'], truth),
                 ('hats_with_new_bass_notes', hats+notes, truth),
                 ('low_frequency_kicks_over_bass', reference['kicks_over_bass'], truth)]
        for name, audio, anchors in cases:
            env, hop = upper_features(audio, sr)
            events = inspect_cues(env, sr, hop)
            scored = [e for e in events if (e['upper_hop']+1)*hop/sr >= .3]
            rows.append(dict(name=name, sample_rate=sr,
                contains_kicks=name=='low_frequency_kicks_over_bass',
                upper_edges=len(scored), linked_edges=sum(e['low_linked'] for e in scored),
                complete_edges=sum(e['linked_and_decayed'] for e in scored),
                onset_coverage=coverage([query(events,t,sr,hop) for t in anchors])))
    return rows


def run(audio_root, baseline_path, out):
    baseline = json.loads(baseline_path.read_text())
    references = {t['track']: t for t in baseline['tracks']}
    sources = development_sources(audio_root)
    labels_path = ROOT/'tests/fixtures/audio_labels/heldout_passages_2026-10-09.json'
    for track in json.loads(labels_path.read_text())['tracks']:
        sources.append(dict(track=track['track'], group='former_heldout',
            audio_path=track['master_path'], audio_sha256=track['audio_sha256'],
            passages=track['passages']))
    for name, digest in baseline['label_sha256'].items():
        if sha(ROOT/'tests/fixtures/audio_labels'/name) != digest:
            raise ValueError('frozen labels changed')
    tracks = []
    for source in sources:
        ref = references[source['track']]
        if source['audio_sha256'] != ref['audio_sha256'] or sha(Path(source['audio_path'])) != ref['audio_sha256']:
            raise ValueError('audio differs from frozen baseline')
        sr, audio = read_audio(Path(source['audio_path']))
        env, hop = upper_features(audio, sr)
        if sr != ref['sample_rate'] or hop != ref['hop']:
            raise ValueError('baseline sample grid changed')
        events = inspect_cues(env, sr, hop)
        positives, unmatched, bass_cores = [], [], []
        for score in ref['scores']['v5']:
            wide = score['association_early_35_late_200_ms']
            caught = {p['attack_s'] for p in wide['pairs']}
            if source['group'] == 'original_five':
                truth = source['truth']
                matched_hops = {round(p['available_s']*sr/hop)-1 for p in wide['pairs']}
                extras = [(i+1)*hop/sr for i in ref['variants']['v5']
                    if i not in matched_hops and not any(r['start_s'] <= (i+1)*hop/sr <= r['end_s']
                                                        for r in source['regions'])]
                assert len(extras) == wide['unmatched_triggers']
            else:
                passage = next(p for p in source['passages'] if p['id'] == score['id'])
                truth = [t for t in passage['kick_times_s'] if passage['start_s'] <= t < passage['end_s']
                         and t not in score['excluded_label_times_s']]
                extras = wide['extra_times_s']
                if not truth and 'bass' in score['id']:
                    core = [e for e in events if passage['start_s'] <= (e['upper_hop']+1)*hop/sr < passage['end_s']]
                    bass_cores.append(dict(id=score['id'], start_s=passage['start_s'], end_s=passage['end_s'],
                        upper_edges=len(core), linked_edges=sum(e['low_linked'] for e in core),
                        complete_edges=sum(e['linked_and_decayed'] for e in core),
                        complete_available_times_s=[(e['available_hop']+1)*hop/sr for e in core if e['linked_and_decayed']]))
            assert len(truth) == score['labels']
            positives.extend(dict(passage=score['id'], original_caught=t in caught,
                                  **query(events,t,sr,hop)) for t in truth)
            unmatched.extend(dict(passage=score['id'], **query(events,t,sr,hop)) for t in extras)
        row = dict(track=source['track'], audio_sha256=ref['audio_sha256'], sample_rate=sr, hop=hop,
            positive_coverage=coverage(positives),
            missed_coverage=coverage([p for p in positives if not p['original_caught']]),
            unmatched_coverage=coverage(unmatched), bass_cores=bass_cores,
            positive_rows=positives, unmatched_rows=unmatched)
        tracks.append(row)
        print(source['track'], {k:row[k] for k in ('positive_coverage','missed_coverage','unmatched_coverage')},
              [{k:v for k,v in b.items() if k != 'complete_available_times_s'} for b in bass_cores], flush=True)
    synthetic = run_controls()
    report = dict(method=__doc__, settings='Butterworth2 bands45–140,1–2k,2–4k,4–8kHz; '
        '3/80ms power followers at native completed hops. Upper rising edge: at least2 '
        'upper fast/slow ratios>2 and summed fast>1e-6. Fresh low rising edge: '
        'fast>1e-6 and fast/slow>1.2 within -10/+35ms of upper edge. Upper power '
        'at50ms must be <=half its peak in first15ms. Only report after50ms deadline. '
        'No cooldown or deduplication: counts are cue edges, not detector fires.',
        tracks=tracks, controls=synthetic,
        totals=dict(positive=coverage([p for t in tracks for p in t['positive_rows']]),
                    missed=coverage([p for t in tracks for p in t['positive_rows'] if not p['original_caught']]),
                    unmatched=coverage([p for t in tracks for p in t['unmatched_rows']])),
        baseline_sha256=sha(baseline_path),
        source_sha256={f:sha(ROOT/'tools/audio_analysis/eval'/f) for f in ('run_kick_upper_cue.py',
            'kick_upper_cue.py','kick_hybrid_experiment.py','run_kick_dsp_experiments.py')},
        label_sha256=baseline['label_sha256'],
        limitations='Diagnostic windows use existing provisional labels; coincidence '
        'with a kick is not source attribution. Original unmatched fires are not '
        'independently audited false positives except reviewed kick-free cores. '
        'All nine tracks are development material. No new reference templates, '
        'runtime calibration, threshold tuning, detector replacement or live changes.')
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(report, indent=2)+'\n')
    print('TOTALS',report['totals'])
    print('CONTROLS',synthetic)
    return report


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--audio-root',type=Path,required=True)
    parser.add_argument('--baseline',type=Path,required=True)
    parser.add_argument('--out',type=Path,required=True)
    args=parser.parse_args()
    run(args.audio_root,args.baseline,args.out)


if __name__=='__main__':
    main()
