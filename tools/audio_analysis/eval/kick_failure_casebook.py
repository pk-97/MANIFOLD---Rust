"""Small descriptive kick failure audit. No detector, fitting or threshold sweep.

Positive observations end 20/35/50 ms after a provisional label. Negative
observations end at an actual unwanted firing, whose acoustic onset is unknown.
All measurements use past samples only at that endpoint. This asymmetry is
intentional: compare available evidence, not purportedly aligned source onsets.
"""
from __future__ import annotations

import argparse
import json
from pathlib import Path

import numpy as np

from .kick_attack_rejection import read_audio
from .live_kick_baseline import sha
from .run_kick_dsp_experiments import development_sources, evaluate

ROOT = Path(__file__).resolve().parents[3]
SCORES = ROOT / 'tools/audio_analysis/eval/scoreboard'
FEATURES = ('growth_db', 'low_fraction', 'high_fraction', 'spectral_change',
            'crest', 'tail_head_db')
# Chosen before measuring features: cover ordinary hits, rolls, weak/masked
# misses, late hits, bass-dependent extras and drum-dependent extras.
CASES = (
    ('apricots_128bpm', 'caught', 1.65),
    ('bad_guy_128bpm', 'caught', .935),
    ('bad_guy_128bpm', 'caught', 4.685),
    ('feel_the_vibration_174bpm', 'caught', 3.305),
    ('inhale_exhale_145bpm', 'caught', 2.07),
    ('late_night', 'caught', 199.585),
    ('late_night', 'caught', 205.56),
    ('midnight_patience', 'caught', 116.5),
    ('feel_the_vibration_174bpm', 'missed_50ms', 4.96),
    ('inhale_exhale_145bpm', 'missed_50ms', 12.62),
    ('late_night', 'missed_50ms', 204.45),
    ('late_night', 'missed_50ms', 204.725),
    ('late_night', 'missed_50ms', 210.975),
    ('midnight_patience', 'missed_50ms', 116.96),
    ('midnight_patience', 'missed_50ms', 121.845),
    ('midnight_patience', 'missed_50ms', 126.96),
    ('bad_guy_128bpm', 'extra_v5', .4849206349206349),
    ('bad_guy_128bpm', 'extra_v5', 4.231065759637188),
    ('inhale_exhale_145bpm', 'extra_v5', 1.2906666666666666),
    ('inhale_exhale_145bpm', 'extra_v5', 7.9093333333333335),
    ('late_night', 'extra_hybrid_kick_free', 128.07466666666667),
    ('late_night', 'extra_hybrid_kick_free', 132.78933333333333),
    ('midnight_patience', 'extra_hybrid_kick_free', 28.256),
    ('midnight_patience', 'extra_hybrid_kick_free', 32.688),
)


def measurements(samples, sr, end_sample, width):
    """Two adjacent equal windows, ending at end_sample (exclusive)."""
    if width < 4 or end_sample < 2 * width or end_sample > len(samples):
        raise ValueError('two complete windows required')
    before = samples[end_sample-2*width:end_sample-width]
    after = samples[end_sample-width:end_sample]
    window = np.hanning(width)
    freq = np.fft.rfftfreq(width, 1/sr)
    mask = (freq >= 45) & (freq <= 8000)
    a = np.abs(np.fft.rfft(before * window))[mask]
    b = np.abs(np.fft.rfft(after * window))[mask]
    f = freq[mask]
    power = b*b
    total = power.sum()
    previous_power, current_power = np.mean(before*before), np.mean(after*after)
    if min(previous_power, current_power, total) <= 1e-20:
        raise ValueError('silent/undefined feature window; do not silently impute')
    cosine = np.dot(a,b) / (np.linalg.norm(a)*np.linalg.norm(b))
    half = width // 2
    head, tail = np.mean(after[:half]**2), np.mean(after[half:]**2)
    return dict(growth_db=float(10*np.log10(current_power/previous_power)),
        low_fraction=float(power[f < 140].sum()/total),
        high_fraction=float(power[f >= 2000].sum()/total),
        spectral_change=float(1-np.clip(cosine,0,1)),
        crest=float(np.max(np.abs(after))/np.sqrt(current_power)),
        tail_head_db=float(10*np.log10(max(tail,1e-20)/max(head,1e-20))))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--audio-root', type=Path, required=True)
    parser.add_argument('--hybrid-report', type=Path, required=True)
    parser.add_argument('--out', type=Path, required=True)
    args = parser.parse_args()
    hybrid = json.loads(args.hybrid_report.read_text())
    ablations = json.loads((SCORES/'kick_rejection_trial_2026-10-09.json').read_text())
    rows, sources = [], []
    for source in development_sources(args.audio_root):
        selected = [c for c in CASES if c[0] == source['track']]
        if not selected:
            continue
        path = Path(source['audio_path'])
        if sha(path) != source['audio_sha256']:
            raise ValueError('audio differs from reviewed source')
        sr, samples = read_audio(path)
        hop = round(sr*256/48000)
        scores = evaluate(source, [(i+1)*hop/sr for i in source['native_hops']])
        strict = [s['accuracy_by_tolerance_ms']['50'] for s in scores]
        pairs = [p for s in scores for p in s['association_early_35_late_200_ms']['pairs']]
        misses = [t for s in strict for t in s['missed_times_s']]
        extras = [t for s in strict for t in s['extra_times_s']]
        sources.append(dict(track=source['track'],path=str(path),sha256=source['audio_sha256'],sample_rate=sr))
        for track, kind, time in selected:
            positive = kind in ('caught','missed_50ms')
            associated = next((p for p in pairs if abs(p['attack_s']-time)<1e-8),None)
            evidence = {}
            if positive:
                assert (time in misses) == (kind == 'missed_50ms')
                assert kind != 'caught' or associated is not None
                evidence = dict(label_status='provisional visual',
                    associated_v5_delay_ms=associated['delay_ms'] if associated else None)
                if track == 'inhale_exhale_145bpm' and time == 12.62:
                    record = next(t for t in ablations['tracks'] if t['track']==track)
                    removed = record['ablation']['stems']['bass']
                    removed_path = Path(removed['ablation_path'])
                    assert sha(removed_path) == removed['ablation_sha256']
                    removed_sr, removed_audio = read_audio(removed_path)
                    assert removed_sr == sr
                    evidence['without_bass_counterfactual'] = dict(
                        audio_path=str(removed_path),sha256=removed['ablation_sha256'],
                        nearby_recorded_fires_s=[t for t in removed['event_times_s'] if abs(t-time)<.05],
                        features_at_label_plus_50ms=measurements(
                            removed_audio,sr,round((time+.05)*sr),round(.05*sr)),
                        limit='Offline stem subtraction, not live separation or proof that the nearby fire identifies this kick.')
            elif kind == 'extra_v5':
                assert any(abs(t-time)<1e-8 for t in extras)
                record = next(t for t in ablations['tracks'] if t['track']==track)
                event = next(e for e in record['ablation']['event_evidence'] if abs(e['candidate_s']-time)<1e-8)
                evidence = dict(label_status='extra against provisional reviewed labels',
                    ablations_removing_event_within_30ms=event['ablations_removing_event_within_30ms'],
                    stem_power_proportions=event['stem_power_proportions_low_plus_body_fast'])
            else:
                passage = next(p for p in source['passages'] if p['start_s']<=time<p['end_s'])
                assert not passage['kick_times_s']
                run = next(t for t in hybrid['variants']['rms_presence']['tracks'] if t['track']==track)
                assert run['audio_sha256']==source['audio_sha256']
                assert any(abs((i+1)*hop/sr-time)<1e-8 for i in run['kick_hops'])
                evidence = dict(label_status='reviewed kick-free master passage',passage=passage['id'])
            values = {}
            for ms in (20,35,50):
                width = round(ms*sr/1000)
                end = round(time*sr) + (width if positive else 0)
                nominal = measurements(samples,sr,end,width)
                shifts = [measurements(samples,sr,end+round(shift*sr/1000),width)
                          for shift in (-20,0,20)] if positive else [nominal]
                values[str(ms)] = dict(nominal=nominal,
                    label_shift_range={k:[min(v[k] for v in shifts),max(v[k] for v in shifts)] for k in FEATURES})
            rows.append(dict(track=track,kind=kind,time_s=time,evidence=evidence,features=values))
        print(source['track'],len(selected),'cases',flush=True)
    summary = {}
    for ms in ('20','35','50'):
        summary[ms] = {}
        for name in FEATURES:
            groups = {}
            for group in ('caught','missed_50ms','extra'):
                chosen=[r for r in rows if (r['kind'].startswith('extra') if group=='extra' else r['kind']==group)]
                values=[r['features'][ms]['nominal'][name] for r in chosen]
                groups[group]=dict(min=min(values),median=float(np.median(values)),max=max(values))
            positive=[r['features'][ms]['nominal'][name] for r in rows if not r['kind'].startswith('extra')]
            negative=[r['features'][ms]['nominal'][name] for r in rows if r['kind'].startswith('extra')]
            summary[ms][name]=dict(groups=groups,nominal_ranges_overlap=max(min(positive),min(negative))<=min(max(positive),max(negative)))
    result=dict(method=__doc__,sources=sources,cases=rows,summary=summary,
        source_sha256={name:sha(Path(__file__).parent/name) for name in (
            'kick_failure_casebook.py','kick_attack_rejection.py','run_kick_dsp_experiments.py',
            'live_kick_baseline.py','master_kick_comparison.py')},
        reference_sha256={name:sha(SCORES/name) for name in (
            'kick_rejection_trial_2026-10-09.json','kick_attack_trial_2026-10-09.json',
            'master_kick_runs_2026-10-09.json')},hybrid_report_sha256=sha(args.hybrid_report),
        labels_sha256={p.name:sha(p) for p in (ROOT/'tests/fixtures/audio_labels').glob('*') if p.suffix in ('.csv','.json')},
        limitations=[
            'Selected diagnostic cases, not a representative accuracy benchmark or held-out test.',
            'Positive windows end after label; negative windows end at firing. No claim of identical acoustic onset alignment. Positive +/-20 ms endpoint sensitivity is retained.',
            'No classifier, fitted threshold, feature combination or prediction accuracy. Range overlap does not rule out multivariate separation.',
            '20/35/50 ms FFT windows have approximately 50/29/20 Hz bin spacing; short low-frequency spectra are coarse and Hann windows emphasize window centres.',
            'Features are causal at their endpoint; label-relative endpoints are offline oracle probes, not a proposed online onset detector.',
            'Ablation establishes dependence, not instrument identity. Labels are not independently listened ground truth.'])
    args.out.parent.mkdir(parents=True,exist_ok=True)
    args.out.write_text(json.dumps(result,indent=2)+'\n')
    print(json.dumps({ms:{k:v['nominal_ranges_overlap'] for k,v in s.items()} for ms,s in summary.items()}))


if __name__ == '__main__':
    main()
