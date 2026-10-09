"""Fixed two-component spectral fitting: known-stem diagnostic and foreign templates.

This is a small NNLS mixture experiment inspired by NMF transcription, not a
reproduction of a published system. A kick spectrum competes with a strictly
past-only background spectrum. No existing v5 candidate gate is applied.
"""
from __future__ import annotations

import argparse
import json
from pathlib import Path
import time

import numpy as np
from scipy.signal import find_peaks, lfilter

from .kick_attack_rejection import read_audio, sha
from .kick_excess_balance_trial import compact_score
from .kick_mixture_fit import causal_spectra, explained_kick_power, detect_activation
from .run_kick_dsp_experiments import ROOT, development_sources, evaluate


def template_sources():
    registry_path = ROOT / 'tools/audio_analysis/eval/additional_stem_sources.json'
    registry = json.loads(registry_path.read_text())
    names = dict(zip(('LATE NIGHT STEMS', 'MIDNIGHT PATIENCE STEMS',
                      'MIRACLE STEMS', 'HEAVY ON MIND STEMS'),
                     ('late_night', 'midnight_patience', 'miracle', 'heavy_on_mind')))
    return [dict(track=names[s['folder']], path=str(Path(registry['source_root']) / f['file']),
                 sha256=f['sha256']) for s in registry['sets'] if s['folder'] in names
            for f in s['files'] if 'kick' in Path(f['file']).name.lower()]


def make_template(source, sr):
    path = Path(source['path'])
    if sha(path) != source['sha256']:
        raise ValueError(f'template source changed: {path}')
    _, audio = read_audio(path, target_sr=sr)
    width = round(.002 * sr)
    # Offline template preparation only: source peaks select attacks without
    # inspecting mix detector outputs, beat grids or scored onset labels.
    envelope = lfilter(np.ones(width)/width, [1], np.abs(audio))
    peak_level = float(envelope.max())
    peaks, _ = find_peaks(envelope, distance=round(.120*sr),
                         prominence=peak_level*.1)
    onsets = []
    for peak in peaks:
        if envelope[peak] < .5*peak_level:
            continue
        onset = int(peak)
        while onset > 0 and envelope[onset-1] > .1*envelope[peak]:
            onset -= 1
        if onset/sr < 1 or onset+round(.080*sr) > len(audio):
            continue
        if onsets and onset/sr-onsets[-1] < .120:
            continue
        onsets.append(onset/sr)
        if len(onsets) == 5:
            break
    if len(onsets) != 5:
        raise ValueError(f'five isolated attack references unavailable: {path}')
    hop = round(sr*256/48000)
    # Analyse only the early source prefix needed for the five references.
    spectra = causal_spectra(audio[:int((onsets[-1]+.080)*sr)+hop], sr, hop)
    times = (np.arange(len(spectra))+1)*hop/sr
    templates = []
    for onset in onsets:
        selected = spectra[(times >= onset) & (times < onset+.080)].mean(axis=0)
        norm = np.linalg.norm(selected)
        if norm <= 1e-12:
            raise ValueError('silent template')
        templates.append(selected/norm)
    template = np.mean(templates, axis=0)
    template /= np.linalg.norm(template)
    return template, dict(**source, sample_rate=sr, hop=hop, source_onsets_s=onsets,
                          reference_span_after_onset_s=[0, .080], template=template.tolist())


def score_fires(source, fires, sr, hop):
    return evaluate(source, [(i+1)*hop/sr for i in fires])


def run(audio_root, heldout_run, out):
    sources = development_sources(audio_root)
    heldout = json.loads(heldout_run.read_text())
    labels_path = ROOT / 'tests/fixtures/audio_labels/heldout_passages_2026-10-09.json'
    if sha(labels_path) != heldout['labels_sha256']:
        raise ValueError('review labels changed')
    for track in json.loads(labels_path.read_text())['tracks']:
        ref = next(t for t in heldout['tracks'] if t['track'] == track['track'])
        sources.append(dict(track=track['track'], group='former_heldout',
            audio_path=track['master_path'], audio_sha256=track['audio_sha256'],
            native_hops=ref['baseline_hops'], passages=track['passages']))
    # Diagnose Heavy first, then complete the fixed foreign-template comparison.
    sources.sort(key=lambda s: s['track'] != 'heavy_on_mind')
    references = template_sources()
    templates, provenance, rows = {}, [], []
    for source in sources:
        path = Path(source['audio_path'])
        if sha(path) != source['audio_sha256']:
            raise ValueError(f'evaluation audio changed: {path}')
        sr, audio = read_audio(path)
        hop = round(sr*256/48000)
        if sr not in templates:
            templates[sr] = {}
            for ref in references:
                template, evidence = make_template(ref, sr)
                templates[sr][ref['track']] = template
                provenance.append(evidence)
        foreign_names = sorted(n for n in templates[sr] if n != source['track'])
        foreign = np.mean([templates[sr][n] for n in foreign_names], axis=0)
        foreign /= np.linalg.norm(foreign)
        methods = dict(foreign_template=foreign)
        if source['track'] == 'heavy_on_mind':
            methods['own_stem_diagnostic'] = templates[sr]['heavy_on_mind']
        start = time.process_time()
        spectra = causal_spectra(audio, sr, hop)
        feature_cpu = time.process_time()-start
        variants = {}
        for name, template in methods.items():
            start = time.process_time()
            power = explained_kick_power(spectra, template, sr, hop)
            fires = detect_activation(power, sr, hop)
            model_cpu = time.process_time()-start
            if fires != sorted(set(fires)) or not all(0 <= i < len(spectra) for i in fires):
                raise ValueError('invalid output indices')
            variants[name] = dict(template_tracks=foreign_names if name == 'foreign_template'
                else ['heavy_on_mind'], kick_hops=fires,
                scores=score_fires(source, fires, sr, hop),
                model_cpu_s=model_cpu,
                processing_cpu_s_per_audio_s=(feature_cpu+model_cpu)/(len(audio)/sr))
        baseline = score_fires(source, source['native_hops'], sr, hop)
        comparisons = []
        for old, new in zip(baseline, variants['foreign_template']['scores']):
            labels = lambda p: {r['attack_s'] for r in
                               p['association_early_35_late_200_ms']['pairs']}
            before, after = labels(old), labels(new)
            comparisons.append(dict(passage=old['id'], lost_labels_s=sorted(before-after),
                                    recovered_labels_s=sorted(after-before)))
        rows.append(dict(track=source['track'], group=source['group'],
            audio_sha256=source['audio_sha256'], sample_rate=sr, hop=hop,
            duration_s=len(audio)/sr, feature_cpu_s=feature_cpu,
            baseline_scores=baseline, variants=variants, comparisons=comparisons))
        print(source['track'], dict(v5=[compact_score(p) for p in baseline],
            **{n: [compact_score(p) for p in v['scores']] for n, v in variants.items()}), flush=True)
    totals = {}
    for name in ('v5', 'foreign_template'):
        passages = [p for t in rows for p in (t['baseline_scores'] if name == 'v5'
                     else t['variants'][name]['scores'])]
        totals[name] = {metric: {k: sum(compact_score(p)[metric][k] for p in passages)
            for k in ('matched', 'missed', 'extra')} for metric in ('strict_50ms', 'association')}
    files = ('run_kick_mixture_trial.py', 'kick_mixture_fit.py', 'kick_tonal_experiment.py',
        'kick_attack_rejection.py', 'kick_excess_balance_trial.py', 'live_kick_baseline.py',
        'master_kick_comparison.py', 'run_kick_dsp_experiments.py')
    result = dict(method=__doc__, settings='2048-point trailing Hann FFT, 30–2500Hz; '
        'native ~5.33ms hop. Kick template averaged from first five strong isolated '
        'source attacks after 1s, each first80ms. Foreign template averages other '
        'songs equally. Background is past-only80ms EMA. Exact two-column NNLS '
        'residual improvement;3/80ms power followers; ratio>2, rearm<1.2, floor1e-6, '
        '60ms refractory. No sweeps.',
        decision_rule='Own-stem diagnostic should improve Heavy recall without extras '
        'in its kick/bass cores. Foreign-template support requires more associated hits, '
        'no original associated labels lost, no increase in unmatched overall or bass-only cores.',
        totals=totals, tracks=rows, template_provenance=provenance,
        source_sha256={f: sha(ROOT/'tools/audio_analysis/eval'/f) for f in files},
        label_sha256={p.name: sha(p) for p in (ROOT/'tests/fixtures/audio_labels').glob('*')
                      if p.is_file() and p.suffix in ('.csv', '.json')},
        limitations='Own-song template is privileged diagnostic information, not '
        'generalisation. Every foreign template excludes the evaluation song. Only '
        'existing provisional short passages scored; all nine tracks are development '
        'material. Magnitude spectra are approximately additive; interference and '
        'mastering violate exact additivity. CPU timing is batched offline process '
        'time, not a native callback benchmark. No live changes.')
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(result, indent=2)+'\n')
    print(json.dumps(totals, indent=2))
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--audio-root', type=Path, required=True)
    parser.add_argument('--heldout-run', type=Path, required=True)
    parser.add_argument('--out', type=Path, required=True)
    args = parser.parse_args()
    run(args.audio_root, args.heldout_run, args.out)


if __name__ == '__main__':
    main()
