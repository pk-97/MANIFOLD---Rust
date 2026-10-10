"""Frozen source-addition controls for existing, song-excluded kick scores.

Run ``prepare`` and inspect source_checks_*.png before ``score``. These are
mono stem mixtures, not reconstructions or accuracy tests of the masters.
"""
from __future__ import annotations

import argparse
from datetime import datetime, timezone
import hashlib
import json
from pathlib import Path
from unittest.mock import patch

import numpy as np
import soundfile as sf

from . import kick_fusion_bandwise as bandwise
from . import kick_fusion_features as base
from .kick_fusion_calibration import select_fires
from .run_kick_fusion_trial import predict_score

ROOT = Path(__file__).resolve().parents[3]
REVIEW = Path('/Users/peterkiemann/.cache/manifold/kick-expanded-review-2026-10-09')
DEFAULT_OUT = Path('/Users/peterkiemann/.cache/manifold/kick-research-2026-10-09-evening/mixtures')
DEFAULT_MODEL = Path('/Users/peterkiemann/.cache/manifold/kick-shape-2026-10-09/trial.json')
SR = 48000
LEVELS = (0.0, 1.0, 2.0)
# Chosen only from the source-supported labels and passage review, before scores.
# Midnight has three positive reviewed passages; its original supplies two contexts.
CONTEXTS = (
    ('late_night', 'expanded_late_night_s2', 72.225),
    ('late_night', 'expanded_late_night_s3', 103.615),
    ('late_night', 'expanded_late_night_s5', 183.34),
    ('late_night', 'late_night_kick_heavy', 205.56),
    ('midnight_patience', 'midnight_patience_kick_heavy', 117.64),
    ('midnight_patience', 'midnight_patience_kick_heavy', 123.095),
    ('midnight_patience', 'expanded_midnight_patience_s5', 192.18),
    ('midnight_patience', 'expanded_midnight_patience_s6', 228.55),
    ('miracle', 'expanded_miracle_s2', 64.6725),
    ('miracle', 'miracle_kick', 87.375),
    ('miracle', 'expanded_miracle_s3', 101.7775),
    ('miracle', 'expanded_miracle_s5', 184.675),
    ('heavy_on_mind', 'expanded_heavy_on_mind_s2', 64.565),
    ('heavy_on_mind', 'expanded_heavy_on_mind_s3', 104.625),
    ('heavy_on_mind', 'expanded_heavy_on_mind_s4', 143.5525),
    ('heavy_on_mind', 'expanded_heavy_on_mind_s6', 215.325),
)


def sha(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def write_json(path, value):
    Path(path).write_text(json.dumps(value, indent=2, allow_nan=False) + '\n')


def mono(values):
    values = np.asarray(values, dtype=np.float64)
    return values.mean(axis=1) if values.ndim == 2 else values


def native_read(path, first, count):
    with sf.SoundFile(path) as f:
        f.seek(first)
        values = mono(f.read(count, dtype='float64'))
        if len(values) != count:
            raise ValueError('short native source excerpt')
        return values, f.samplerate


def cached_excerpt(track, passage, role, first, count, sr, offset):
    """Reuse raw review excerpts, preserving their exact source sample origin."""
    if not passage.startswith('expanded_'):
        return None
    stratum = int(passage.rsplit('_s', 1)[1])
    if track in ('late_night', 'midnight_patience'):
        path = REVIEW / 'raw-review-late-midnight' / f'{track}_s{stratum}_excerpts.npz'
        index = json.loads((path.parent / 'assets_index.json').read_text())
        row = next(t for t in index['tracks'] if t['track'] == track)
        expected = next(c['excerpts_sha256'] for c in row['cores'] if c['stratum'] == stratum)
        if sha(path) != expected:
            raise ValueError('changed raw review cache')
        with np.load(path, allow_pickle=False) as data:
            metadata = json.loads(str(data['metadata']))[role]
            origin = metadata['source_sample_range'][0]
            start = first - origin
            if metadata['sample_rate'] != sr or not 0 <= start <= len(data[role]) - count:
                return None
            values = data[role][start:start + count].astype(np.float64)
    else:
        index = REVIEW / 'raw-review-miracle-heavy/raw_evidence_index.json'
        track_row = next(t for t in json.loads(index.read_text())['tracks'] if t['track'] == track)
        entry = next(c for c in track_row['cores'] if c['stratum'] == stratum)['excerpts'][role]
        path = Path(entry['path'])
        if sha(path) != entry['sha256']:
            raise ValueError('changed raw review cache')
        origin = round((entry['first_sample_master_s'] - offset) * sr)
        start = first - origin
        if not 0 <= start <= entry['frames'] - count:
            return None
        values, actual_sr = native_read(path, start, count)
        if actual_sr != sr:
            raise ValueError('cache rate changed')
    return values, dict(path=str(path), sha256=sha(path), source_first_sample=first,
                        source_frames=count, cache_origin_sample=origin)


def construct_conditions(kick, accompaniment, headroom=.95):
    """One common scalar across all gains and both source-presence conditions."""
    kick, accompaniment = np.asarray(kick), np.asarray(accompaniment)
    if kick.shape != accompaniment.shape or kick.ndim != 1:
        raise ValueError('aligned mono sources required')
    if not np.all(np.isfinite(kick)) or not np.all(np.isfinite(accompaniment)):
        raise ValueError('non-finite source')
    raw = {(gain, present): gain * accompaniment + (kick if present else 0)
           for gain in LEVELS for present in (True, False)}
    peak = max(float(np.max(np.abs(x), initial=0)) for x in raw.values())
    common = min(1.0, headroom / peak) if peak else 1.0
    return common, {key: value * common for key, value in raw.items()}


def source_anchor(kick, local_label_s, interval_start_s=None):
    """A diagnostic raw-source threshold, never a replacement master annotation."""
    begin = round(max(local_label_s - .060, interval_start_s if interval_start_s is not None else local_label_s - .020) * SR)
    end = round((local_label_s + .100) * SR)
    values = np.abs(kick[begin:end])
    threshold = .05 * float(np.max(values))
    if threshold < 1e-5:
        raise ValueError('source does not contain a clear pulse')
    first = begin + int(np.flatnonzero(values >= threshold)[0])
    return first, threshold


def fixed_anchor_features(samples, sample_rate, anchor_hop):
    """Reuse the exact frozen reductions with one exogenous source-time anchor."""
    def single(envelopes, upper):
        edges = np.zeros(len(envelopes), dtype=bool)
        edges[anchor_hop] = True
        return edges
    with patch.object(base, '_rise_edges', side_effect=single):
        candidates, available, features, hop = bandwise.fusion_features(samples, sample_rate)
    if len(candidates) != 1 or candidates[0] != anchor_hop:
        raise ValueError('fixed source anchor has no complete evidence window')
    return available, features, hop


def envelope_diagnostic(samples, anchor_hop, deadline_hop):
    """Expose existing ratio numerator/denominator; this adds no detector feature."""
    envelopes, _ = base.causal_features(samples, SR)
    result = {}
    for band, name in enumerate(('low', 'body')):
        fast, slow = envelopes[anchor_hop:deadline_hop+1, band].T
        ratio = (fast+base.EPSILON)/(slow+base.EPSILON)
        local = int(np.argmax(ratio))
        result[name] = dict(fast_at_max_ratio=float(fast[local]), slow_at_max_ratio=float(slow[local]),
                            max_log_rise=float(np.log(ratio[local])), ratio_hop=anchor_hop+local)
    return result


def prepare(out):
    out.mkdir(parents=True, exist_ok=True)
    freeze = out / 'frozen_contexts.json'
    if freeze.exists():
        raise ValueError('contexts are already frozen; refusing to overwrite')
    registry_path = ROOT / 'tools/audio_analysis/eval/additional_stem_sources.json'
    registry = json.loads(registry_path.read_text())
    sources = {s['folder'].lower().replace(' stems', '').replace(' ', '_'): s for s in registry['sets']}
    labels, label_hashes = {}, {}
    for name in ('master', 'heldout', 'expanded'):
        path = ROOT / f'tests/fixtures/audio_labels/{name}_passages_2026-10-09.json'
        label_hashes[str(path)] = sha(path)
        for track in json.loads(path.read_text())['tracks']:
            for passage in track.get('passages', track.get('cores', [])):
                labels[passage['id']] = passage
    rows = []
    for i, (track, passage_id, label) in enumerate(CONTEXTS):
        passage = labels[passage_id]
        if label not in passage['kick_times_s'] or not passage.get('scoring_ready', True):
            raise ValueError('context is not an accepted source-supported label')
        registry_row = sources[track]
        offset = .136375 if track == 'midnight_patience' else 0.0
        first = round((label - offset - 1.300) * SR)
        origin = first / SR + offset
        count = 2 * SR
        parts, provenance = {}, []
        for source in registry_row['files']:
            if source['role'] != 'named_stem':
                continue
            path = Path(registry['source_root']) / source['file']
            is_kick = 'kick' in path.name.lower()
            reuse = cached_excerpt(track, passage_id, 'kick', first, count, SR, offset) if is_kick else None
            if reuse:
                values, cache = reuse
            else:
                values, actual_sr = native_read(path, first, count)
                if actual_sr != SR:
                    raise ValueError('source not native 48k')
                cache = None
            parts['kick' if is_kick else source['file']] = values
            provenance.append(dict(path=str(path), registered_sha256=source['sha256'],
                hash_status='registry full-file read-verified; this run hashes native excerpt',
                source_first_sample=first, source_frames=count, cache=cache,
                mono_excerpt_sha256=hashlib.sha256(values.tobytes()).hexdigest()))
        kick = parts.pop('kick')
        accompaniment = np.sum(list(parts.values()), axis=0)
        onset_entries = passage.get('onset_intervals', passage.get('kick_onset_intervals', passage.get('onset_review', [])))
        interval = next((v.get('interval_s', [v.get('start_s'), v.get('end_s')]) for v in onset_entries
                         if v['midpoint_s'] == label), None)
        interval_start = interval[0] - origin if interval and interval[0] is not None else None
        anchor_sample, amplitude_threshold = source_anchor(kick, label - origin, interval_start)
        common, conditions = construct_conditions(kick, accompaniment)
        master_sr = int(registry_row['master']['audio']['sample_rate'])
        master_first, master_count = round(origin * master_sr), round(2 * master_sr)
        reuse = cached_excerpt(track, passage_id, 'master', master_first, master_count, master_sr, 0)
        if reuse:
            master, master_cache = reuse
        else:
            master, rate = native_read(registry_row['master']['path'], master_first, master_count)
            assert rate == master_sr
            master_cache = None
        context_id = f'{i+1:02d}_{track}'
        path = out / (context_id + '.npz')
        np.savez_compressed(path, kick=kick, accompaniment=accompaniment, master=master,
                            **{f'accompaniment_{j}': p for j, p in enumerate(parts.values())})
        rows.append(dict(id=context_id, track=track, bpm=registry_row['bpm'], passage=passage_id,
            master_label_s=label, master_label_interval_s=interval, master_origin_s=origin,
            stem_to_master_seconds=offset, stem_first_sample=first, duration_s=2.0,
            source_anchor_sample=anchor_sample, source_anchor_master_s=origin + anchor_sample / SR,
            source_anchor_threshold=amplitude_threshold,
            source_anchor_note='First absolute mono sample >=5% local source peak from reviewed onset-bracket start to label +100ms; diagnostic clock only. Search restriction avoids previous-kick tails.',
            anchor_hop=anchor_sample // 256, common_gain=common,
            max_condition_peak=max(float(np.max(np.abs(x))) for x in conditions.values()),
            named_accompaniment=list(parts), source_provenance=provenance,
            master=dict(path=registry_row['master']['path'], registered_sha256=registry_row['master']['sha256'],
                        sample_rate=master_sr, first_sample=master_first, cache=master_cache),
            cache=str(path), cache_sha256=sha(path)))
        print('prepared', context_id, 'source-label ms', round(1000*(rows[-1]['source_anchor_master_s']-label), 3), flush=True)
    report = dict(status='frozen_before_first_mixture_score', frozen_utc=datetime.now(timezone.utc).isoformat(),
        hypothesis='Accompaniment changes relative rise/balance or candidate timing before eliminating the fixed kick attack.',
        acceptance='Diagnose same-source changes without claiming finished-master accuracy. Report candidate coverage separately from fixed-anchor scores and all kick-removed responses.',
        selection='Four accepted source-supported labels per family, spread across reviewed positive passages, selected without detector outcomes; two original Midnight contexts because only three positive passages exist.',
        source_registry_sha256=sha(registry_path), label_sha256=label_hashes, levels=list(LEVELS),
        mix='Arithmetic mono stem sum; kick fixed; all named non-kick stems at gain0/1/2. Same common scalar in all six conditions, no limiter or per-condition normalisation.',
        limitations='Provisional source-assisted visual master labels; mixed drums may contain kick leakage. Kick-removed means named isolated kick omitted. Source sums differ from finished masters.',
        contexts=rows)
    write_json(freeze, report)
    source_plots(out, report)
    return report


def source_plots(out, report):
    """Bounded raw-waveform observation: named source pulse and label alignment."""
    from PIL import Image, ImageDraw
    for track in dict.fromkeys(c['track'] for c in report['contexts']):
        image = Image.new('RGB', (1500, 1100), 'white')
        draw = ImageDraw.Draw(image)
        draw.text((20, 10), track + ': source (blue), master (grey); red=source anchor, green=label interval', fill='black')
        for row, context in enumerate(c for c in report['contexts'] if c['track'] == track):
            data = np.load(context['cache'])
            start, end = context['master_label_s'] - .160, context['master_label_s'] + .220
            top = 40 + row * 260
            draw.text((20, top), f"{context['id']}  {context['passage']} label={context['master_label_s']:.6f}s source={context['source_anchor_master_s']:.6f}s", fill='black')
            for j, (name, color) in enumerate((('kick', '#174FBB'), ('master', '#555555'))):
                rate = SR if name == 'kick' else context['master']['sample_rate']
                origin = context['master_origin_s'] if name == 'kick' else context['master']['first_sample'] / rate
                values = data[name][round((start-origin)*rate):round((end-origin)*rate)]
                scale = max(float(np.max(np.abs(values))), 1e-10)
                y = top + 65 + j * 105
                draw.text((20,y-45), f'{name} peak {scale:.5f}', fill=color)
                for x in range(1400):
                    a, b = int(x*len(values)/1400), max(int((x+1)*len(values)/1400), int(x*len(values)/1400)+1)
                    v = values[a:b]
                    if len(v):
                        draw.line((80+x, y-40*float(np.max(v))/scale, 80+x, y-40*float(np.min(v))/scale), fill=color)
                for time, c in [(context['source_anchor_master_s'], 'red')] + [(t, 'green') for t in (context['master_label_interval_s'] or []) if t is not None]:
                    x = 80 + (time-start)/(end-start)*1400
                    draw.line((x,y-45,x,y+45), fill=c)
            draw.text((80,top+235), f'{start:.4f}s'+' '*130+f'{end:.4f}s', fill='black')
        image.save(out / f'source_checks_{track}.png')


def score(out, model_path):
    frozen_path = out / 'frozen_contexts.json'
    frozen = json.loads(frozen_path.read_text())
    variant = next(v for v in json.loads(model_path.read_text())['variants'] if v['variant'] == 'linear_15')
    models = {r['track']: r for r in variant['tracks']}
    results = []
    for context in frozen['contexts']:
        if sha(context['cache']) != context['cache_sha256']:
            raise ValueError('frozen source cache changed')
        data = np.load(context['cache'])
        model = models[context['track']]['model']
        threshold = models[context['track']]['refinement']['threshold']
        if model['held_out'] != context['track'] or context['track'] in model['training_tracks']:
            raise ValueError('target family in fixed model training')
        common, conditions = construct_conditions(data['kick'], data['accompaniment'])
        if common != context['common_gain']:
            raise ValueError('frozen gain changed')
        origin, target = context['master_origin_s'], context['source_anchor_master_s']
        rows = []
        for (gain, present), samples in conditions.items():
            candidates, available, features, hop = bandwise.fusion_features(samples, SR)
            scores = predict_score(model, features)
            fires = select_fires(scores, available, SR, hop, threshold)
            starts, ends = origin + (candidates+1)*hop/SR, origin + (available+1)*hop/SR
            near = np.flatnonzero((starts >= target-.020) & (starts <= target+.030))
            eligible = np.flatnonzero(np.abs(ends-context['master_label_s']) <= .070)
            anchor_available, anchored, _ = fixed_anchor_features(samples, SR, context['anchor_hop'])
            anchored_score = float(predict_score(model, anchored)[0])
            envelope_values = envelope_diagnostic(samples, context['anchor_hop'], int(anchor_available[0]))
            z = np.clip((anchored[0]-np.asarray(model['mean']))/np.asarray(model['scale']), -8, 8)
            frame = slice(context['source_anchor_sample'], context['source_anchor_sample']+round(.070*SR))
            removed = conditions[gain, False]
            identity_error = float(np.max(np.abs(samples-removed-common*data['kick']))) if present else None
            rows.append(dict(gain=gain, kick_present=present, fixed_anchor_score=anchored_score,
                fixed_anchor_above_cutoff=anchored_score >= threshold,
                fixed_anchor_emission_master_s=origin+(int(anchor_available[0])+1)*hop/SR,
                fixed_anchor_features=dict(zip(bandwise.FEATURE_NAMES, anchored[0].tolist())),
                fixed_anchor_envelopes=envelope_values,
                fixed_anchor_logit_contributions=dict(zip(bandwise.FEATURE_NAMES, (z*np.asarray(model['weights'])).tolist())),
                source_addition_max_error=identity_error,
                kick_over_accompaniment_70ms_db=(float(10*np.log10((np.sum(data['kick'][frame]**2)+1e-24)/(np.sum((gain*data['accompaniment'][frame])**2)+1e-24))) if gain else None),
                nearby_candidates=[dict(candidate_master_s=float(starts[j]), emission_master_s=float(ends[j]),
                    candidate_source_delta_ms=float(1000*(starts[j]-target)), emission_label_delta_ms=float(1000*(ends[j]-context['master_label_s'])),
                    score=float(scores[j]), above_cutoff=bool(scores[j]>=threshold), fired=int(available[j]) in fires,
                    features=dict(zip(bandwise.FEATURE_NAMES, features[j].tolist()))) for j in near],
                eligible_candidate_count=len(eligible),
                eligible_max_score=float(np.max(scores[eligible])) if len(eligible) else None,
                all_candidate_starts_master_s=starts.tolist(), all_candidate_emissions_master_s=ends.tolist(),
                all_candidate_scores=scores.tolist(), emitted_master_s=[origin+(i+1)*hop/SR for i in fires],
                target_emissions_master_s=[origin+(i+1)*hop/SR for i in fires if abs(origin+(i+1)*hop/SR-context['master_label_s']) <= .070]))
        results.append(dict(id=context['id'], track=context['track'], label_s=context['master_label_s'],
            source_anchor_s=target, cutoff=threshold, common_gain=common,
            model_sha256=hashlib.sha256(json.dumps(model, sort_keys=True).encode()).hexdigest(), conditions=rows))
        print('scored', context['id'], [(r['gain'], r['kick_present'], round(r['fixed_anchor_score'], 3), len(r['target_emissions_master_s'])) for r in rows], flush=True)
    report = dict(frozen_contexts_sha256=sha(frozen_path), model_report_sha256=sha(model_path),
        script_sha256=sha(__file__), feature_names=list(bandwise.FEATURE_NAMES),
        dependency_sha256={name: sha(Path(__file__).parent/name) for name in (
            'kick_fusion_bandwise.py', 'kick_fusion_features.py', 'kick_attack_rejection.py',
            'kick_upper_cue.py', 'kick_tonal_experiment.py', 'run_kick_fusion_trial.py', 'kick_fusion_calibration.py')},
        note='Fixed-anchor rows are exogenous causal measurement controls, not emitted events. Actual natural-candidate emissions are reported separately at completed evidence hops. No fitting, retuning, delay correction or master reconstruction.',
        contexts=results)
    write_json(out / 'mixture_results.json', report)
    return report


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('action', choices=('prepare', 'score'))
    parser.add_argument('--out', type=Path, default=DEFAULT_OUT)
    parser.add_argument('--model', type=Path, default=DEFAULT_MODEL)
    args = parser.parse_args()
    prepare(args.out) if args.action == 'prepare' else score(args.out, args.model)
