"""Prepare fixed-stratum, detector-blind source proposals for four development songs.

These are unreviewed proposals, never accepted labels. Selection uses duration and
existing review bounds only. The source method is frozen before master plotting:
4 ms RMS / 1 ms hop, max(median + 5 MAD, 5.5% peak), 80 ms merge, as implemented
by the existing Miracle review helper. Exactly silent stereo source cores remain
source evidence only. No BPM, detector, model, score or reserved song is used.
"""
from __future__ import annotations

import argparse
import hashlib
import importlib.util
import json
from pathlib import Path

import numpy as np
from PIL import Image, ImageDraw, ImageFont
from scipy.signal import butter, lfilter, sosfilt
import soundfile as sf


ROOT = Path(__file__).resolve().parents[3]
REGISTRY = ROOT / 'tools/audio_analysis/eval/additional_stem_sources.json'
HELPER = Path.home() / '.cache/manifold/kick-heldout-2026-10-09/miracle/review_miracle.py'
TRACKS = ('late_night', 'midnight_patience', 'miracle', 'heavy_on_mind')
RESERVED = ('waypoints', 'know_youre_there')
CORE_SECONDS = 12.0
REVIEW_MARGIN = .25


def sha(path):
    digest = hashlib.sha256()
    with path.open('rb') as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b''):
            digest.update(chunk)
    return digest.hexdigest()


def select_cores(duration, existing):
    """Six fixed stratum centres; overlapping windows are skipped, never moved."""
    if duration < 92.0:
        raise ValueError('six disjoint 12 s strata require at least 92 s')
    width = (duration - 20.0) / 6
    rows = []
    for index in range(6):
        center = 10.0 + (index + .5) * width
        start, end = center - CORE_SECONDS / 2, center + CORE_SECONDS / 2
        review_start, review_end = start - REVIEW_MARGIN, end + REVIEW_MARGIN
        overlap = [p['id'] for p in existing if review_start < p['end_s'] + REVIEW_MARGIN
                   and review_end > p['start_s'] - REVIEW_MARGIN]
        rows.append(dict(stratum=index + 1, start_s=start, end_s=end,
                         review_start_s=review_start, review_end_s=review_end,
                         status='skipped_existing_review_overlap' if overlap else 'selected',
                         overlapping_existing_core_ids=overlap))
    return rows


def onset_to_master(source_time, offset):
    return dict(source_proposal_s=float(source_time), master_proposal_s=float(source_time + offset),
                source_rms_support_s=[float(source_time - .002), float(source_time + .002)],
                master_mapped_rms_support_s=[float(source_time + offset - .002),
                                             float(source_time + offset + .002)],
                uncertainty=dict(master_onset_interval_s=None,
                    meaning='RMS support is measurement support, not an onset confidence interval. '
                    'Source/master attack agreement and accepted onset timing remain unreviewed.'))


def load_helper():
    spec = importlib.util.spec_from_file_location('existing_miracle_review', HELPER)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def master_envelopes(audio, sr):
    """Existing 2nd-order low/body bands and 3 ms fast power smoothing for display."""
    hop = max(1, round(sr * 256 / 48000))
    alpha = np.exp(-1.0 / (.003 * sr))
    rows = []
    for low, high in ((45., 140.), (140., 400.)):
        filtered = sosfilt(butter(2, [low, high], btype='bandpass', fs=sr, output='sos'), audio)
        power = lfilter([1 - alpha], [1., -alpha], filtered * filtered)
        rows.append(np.sqrt(np.maximum(power[hop - 1::hop], 0)))
    return (np.arange(len(rows[0])) + 1) * hop / sr, np.stack(rows)


def font(size):
    return ImageFont.truetype('/System/Library/Fonts/Menlo.ttc', size)


def plot_track(track, cores, traces, out):
    width, row_height, top = 1900, 170, 95
    image = Image.new('RGB', (width, top + 6 * row_height + 20), '#101722')
    draw = ImageDraw.Draw(image)
    draw.text((24, 15), f'{track} — UNREVIEWED source proposals; not labels', font=font(24), fill='#eef2f7')
    draw.text((24, 49), 'Blue: master 45–140 Hz | Green: master 140–400 Hz | Orange: kick-source RMS / proposals',
              font=font(17), fill='#bdc8d6')
    draw.text((24, 72), 'Master bands share each row’s scale; kick has its own scale. Grey: 250 ms review margins. No lag fitted.',
              font=font(15), fill='#bdc8d6')
    left, right = 115, width - 22
    for index, core in enumerate(cores):
        y0, y1 = top + index * row_height + 26, top + (index + 1) * row_height - 25
        draw.text((20, y0 + 5), f'S{core["stratum"]}', font=font(19), fill='#eef2f7')
        title = f'{core["start_s"]:.3f}–{core["end_s"]:.3f} s'
        if core['status'] != 'selected':
            draw.text((left, y0), title + ' SKIPPED: existing review overlap', font=font(17), fill='#aeb9c7')
            continue
        title += f' | {core["core_proposal_count"]} core proposals; source silent={core["kick_stereo_exactly_silent"]}'
        draw.text((left, y0 - 23), title, font=font(15), fill='#eef2f7')
        rs, re = core['review_start_s'], core['review_end_s']
        def xx(t):
            return left + (t - rs) / (re - rs) * (right - left)
        draw.rectangle((left, y0, right, y1), fill='#182230', outline='#425064')
        draw.rectangle((left, y0, xx(core['start_s']), y1), fill='#303743')
        draw.rectangle((xx(core['end_s']), y0, right, y1), fill='#303743')
        for second in np.arange(np.ceil(rs), re):
            x = xx(second)
            draw.line((x, y0, x, y1), fill='#354052')
            draw.text((x - 24, y1 + 4), f'{second:.0f}s', font=font(13), fill='#b6c2d1')
        data = traces[core['stratum']]
        master_scale = max(float(np.max(data['master_low'])), float(np.max(data['master_body'])), 1e-12)
        for time_key, key, color, scale in (
            ('master_times', 'master_low', '#55b0ff', master_scale),
            ('master_times', 'master_body', '#62d5a0', master_scale),
            ('source_times_master', 'source_rms', '#ff9b62', max(float(np.max(data['source_rms'])), 1e-12)),
        ):
            times, values = data[time_key], data[key]
            # Peak-preserving pixel reduction keeps narrow proposal evidence visible.
            bins = np.clip(((times - rs) / (re - rs) * (right - left)).astype(int), 0, right - left)
            peaks = np.zeros(right - left + 1)
            np.maximum.at(peaks, bins, values)
            points = [(left + j, y1 - float(value / scale) * (y1 - y0 - 4)) for j, value in enumerate(peaks)]
            draw.line(points, fill=color, width=1)
        for proposal in core['proposals']:
            x = xx(proposal['master_proposal_s'])
            draw.line((x, y0, x, y0 + 15), fill='#ff9b62', width=2)
    path = out / f'{track}_six_strata.png'
    image.save(path)
    return str(path)


def run(out):
    out.mkdir(parents=True, exist_ok=True)
    helper = load_helper()
    registry = json.loads(REGISTRY.read_text())
    label_paths = [ROOT / 'tests/fixtures/audio_labels' / name for name in
                   ('master_passages_2026-10-09.json', 'heldout_passages_2026-10-09.json')]
    reviews = {track['track']: track for path in label_paths for track in json.loads(path.read_text())['tracks']}
    report = dict(method=__doc__, review_status='UNREVIEWED_SOURCE_PROPOSALS', ground_truth=False,
        detector_execution=False, reserved_families_not_read=list(RESERVED),
        selection='12 s cores at centres of six equal strata across master 10 s..duration−10 s. '
                  'Both new and existing review ranges include 250 ms margins; overlaps are skipped without replacement.',
        proposal_method='Reused Miracle source_attack_candidates: 4 ms RMS, 1 ms hop; '
                        'max(median+5 MAD, .055 peak), 80 ms merge. Exactly silent source windows produce no candidates.',
        source_sha256=dict(registry=sha(REGISTRY), script=sha(Path(__file__)), helper=sha(HELPER)),
        reused_helper=str(HELPER), existing_review_sha256={str(p.relative_to(ROOT)): sha(p) for p in label_paths},
        tracks=[])
    for track in TRACKS:
        group = next(s for s in registry['sets'] if s['folder'].lower().replace(' stems', '').replace(' ', '_') == track)
        kick_entry = next(s for s in group['files'] if 'kick' in Path(s['file']).stem.lower())
        kick_path = Path(registry['source_root']) / kick_entry['file']
        master_path = Path(group['master']['path'])
        for path, expected in ((kick_path, kick_entry['sha256']), (master_path, group['master']['sha256'])):
            if sha(path) != expected:
                raise ValueError(f'registry hash mismatch: {path}')
        offset = group['alignment'].get('stem_to_master_seconds', 0.0) if track == 'midnight_patience' else 0.0
        master_info = sf.info(str(master_path))
        kick_stereo, kick_sr = sf.read(str(kick_path), dtype='float32', always_2d=True)
        kick = kick_stereo.mean(axis=1, dtype=np.float32)
        duration = master_info.frames / master_info.samplerate
        cores = select_cores(duration, reviews[track]['passages'])
        traces = {}
        # Complete and freeze source proposals before decoding the master.
        for core in cores:
            if core['status'] != 'selected':
                continue
            rs, re = core['review_start_s'] - offset, core['review_end_s'] - offset
            if rs < 0 or re > len(kick) / kick_sr:
                raise ValueError('selected source review window is outside recording')
            a, b = round((core['start_s'] - offset) * kick_sr), round((core['end_s'] - offset) * kick_sr)
            silent = bool(np.all(kick_stereo[a:b] == 0))
            source_times, energy = helper.energy_envelope(kick, kick_sr, rs, re)
            if np.max(energy, initial=0) == 0:
                proposals, stats = [], dict(threshold=0.0, peak=0.0, status='exactly_silent_review_window')
            else:
                proposals, stats = helper.source_attack_candidates(kick, kick_sr, rs, re)
            mapped = [onset_to_master(t, offset) for t in proposals]
            for proposal in mapped:
                proposal['inside_core'] = core['start_s'] <= proposal['master_proposal_s'] < core['end_s']
            core.update(proposals=mapped, core_proposal_count=sum(p['inside_core'] for p in mapped),
                        proposal_statistics=stats, kick_stereo_exactly_silent=silent,
                        silence_qualification='Exact source-core stereo zeros do not establish a kick-free master.',
                        kick_native_core_samples=[a, b])
            traces[core['stratum']] = dict(source_times_master=source_times + offset, source_rms=energy)
        source_proposals_path = out / f'{track}_source_proposals.json'
        source_proposals_path.write_text(json.dumps(dict(track=track, ground_truth=False, cores=cores), indent=2) + '\n')
        del kick_stereo, kick
        master_stereo, master_sr = sf.read(str(master_path), dtype='float32', always_2d=True)
        master = master_stereo.mean(axis=1, dtype=np.float32)
        del master_stereo
        times, envelopes = master_envelopes(master, master_sr)
        for core in cores:
            if core['status'] != 'selected':
                continue
            keep = (times >= core['review_start_s']) & (times <= core['review_end_s'])
            traces[core['stratum']].update(master_times=times[keep], master_low=envelopes[0, keep],
                                           master_body=envelopes[1, keep])
        trace_path = out / f'{track}_review_traces.npz'
        np.savez_compressed(trace_path, **{f's{index}_{name}': value for index, data in traces.items()
                                         for name, value in data.items()})
        plot = plot_track(track, cores, traces, out)
        row = dict(track=track, duration_s=duration, master=dict(path=str(master_path),
            sha256=group['master']['sha256'], sample_rate=master_sr, frames=len(master)),
            kick=dict(path=str(kick_path), sha256=kick_entry['sha256'], sample_rate=kick_sr),
            stem_to_master_seconds=offset, offset_is_detector_latency_compensation=False,
            alignment_evidence=dict(registry=group['alignment'],
                                    existing_review=reviews[track].get('alignment_evidence'),
                                    qualification='Only Midnight’s documented source offset is applied. '
                                    'Other songs use seconds with no fitted lag/warp; diagnostics do not prove onset agreement.'),
            selected_core_count=len(traces), cores=cores, plot=plot,
            source_proposals_written_before_master_decode=str(source_proposals_path),
            trace_cache=dict(path=str(trace_path), sha256=sha(trace_path)))
        report['tracks'].append(row)
        print(track, 'cores', len(traces), 'proposals', sum(c.get('core_proposal_count', 0) for c in cores), flush=True)
        del master, envelopes, times
    path = out / 'expanded_review_proposals.json'
    path.write_text(json.dumps(report, indent=2) + '\n')
    return report


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--out', type=Path, required=True)
    args = parser.parse_args()
    run(args.out)
