"""Native raw-source evidence for eight detector-blind evening review cores."""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path

import numpy as np
from PIL import Image, ImageDraw, ImageFont
from scipy.signal import spectrogram
import soundfile as sf

ROOT = Path(__file__).resolve().parents[3]
CACHE = Path('/Users/peterkiemann/.cache/manifold/kick-research-2026-10-09-evening')
FONT = '/System/Library/Fonts/Menlo.ttc'


def sha(path):
    digest = hashlib.sha256()
    with Path(path).open('rb') as handle:
        for chunk in iter(lambda: handle.read(1024*1024), b''):
            digest.update(chunk)
    return digest.hexdigest()


def nearest(values, targets):
    index = np.clip(np.searchsorted(values, targets), 1, len(values)-1)
    return index-(abs(values[index-1]-targets) < abs(values[index]-targets))


def render(core, windows, destination, columns=1):
    """Exact raw waveform bins and centred annotation spectra on a master clock."""
    width, block_height = 2300, 550
    image = Image.new('RGB', (width, 70+block_height*((len(windows)+columns-1)//columns)), 'white')
    draw = ImageDraw.Draw(image)
    font = ImageFont.truetype(FONT, 17)
    small = ImageFont.truetype(FONT, 13)
    draw.text((15, 8), f"{core['id']} | core {core['start_s']:.6f}–{core['end_s']:.6f}s | raw review, no detector", fill='black', font=font)
    draw.text((15, 34), 'Master seconds. Spectra: stereo mean power, periodic Hann2048/hop96, centred annotation only; -85..-15dB. Onset clock: raw waveforms.', fill='black', font=small)
    data = {}
    for role, meta in core['excerpts'].items():
        values, sr = sf.read(meta['path'], dtype='float64', always_2d=True)
        with np.load(meta['spectrogram_path']) as stored:
            data[role] = dict(wave=values, sr=sr, origin=meta['master_axis_origin_s'],
                times=stored['times_master_s'], frequency=stored['frequency_hz'], power=stored['power'])
    for row, (start, end) in enumerate(windows):
        left = (row % columns)*(width//columns)
        x0, x1 = left+155, left+width//columns-40
        top = 65+(row//columns)*block_height
        draw.text((left+10, top), f'{start:.6f}–{end:.6f}s', fill='black', font=font)
        def axis(y, height, label):
            draw.text((left+6, y+3), label, fill='black', font=small)
            draw.rectangle((x0, y, x1, y+height), outline='#aaaaaa')
            for boundary in (core['start_s'], core['end_s']):
                if start <= boundary <= end:
                    x = x0+(boundary-start)/(end-start)*(x1-x0)
                    draw.line((x, y, x, y+height), fill='cyan', width=2)
        def waveform(role, y, height):
            d = data[role]; values, sr, origin = d['wave'], d['sr'], d['origin']
            mono = values.mean(axis=1)
            a, b = max(0, round((start-origin)*sr)), min(len(values), round((end-origin)*sr))
            peak = max(float(np.max(np.abs(mono[a:b] if columns > 1 else mono))), 1e-12)
            axis(y, height, f'{role} mono\npeak {peak:.4g}')
            for pixel in range(x1-x0):
                pa = max(a, round((start+pixel/(x1-x0)*(end-start)-origin)*sr))
                pb = min(b, round((start+(pixel+1)/(x1-x0)*(end-start)-origin)*sr))
                if pb > pa >= 0:
                    segment = mono[pa:pb]
                    draw.line((x0+pixel, y+height/2-float(segment.max())/peak*height*.46,
                               x0+pixel, y+height/2-float(segment.min())/peak*height*.46),
                              fill='#2B5D87' if role == 'master' else '#7D3C98')
        def spectrum(role, y, height):
            d = data[role]
            target_time = start+(np.arange(x1-x0)+.5)/(x1-x0)*(end-start)
            ti = nearest(d['times'], target_time)
            target_freq = np.geomspace(4000, 30, height)
            fi = nearest(d['frequency'], target_freq)
            db = 10*np.log10(np.maximum(d['power'][np.ix_(fi, ti)], 1e-14))
            level = np.clip((db+85)/70, 0, 1)
            stops = np.array([[2, 1, 12], [60, 15, 85], [145, 40, 90], [230, 100, 55], [255, 230, 145]])
            lower = np.minimum((level*4).astype(int), 3); fraction = (level*4-lower)[..., None]
            rgb = (stops[lower]*(1-fraction)+stops[lower+1]*fraction).astype('uint8')
            rgb[:, (target_time < d['times'][0]) | (target_time > d['times'][-1])] = [235,235,235]
            image.paste(Image.fromarray(rgb), (x0, y)); axis(y, height, role+' Hz')
            for hz in (50, 100, 300, 1000, 3000):
                yy = y+np.log(4000/hz)/np.log(4000/30)*height
                draw.text((left+85, yy-7), str(hz), fill='black', font=small)
        waveform('master', top+30, 75)
        spectrum('master', top+112, 165)
        waveform('kick', top+285, 75)
        spectrum('kick', top+367, 135)
        step = .05 if end-start <= .8 else (.1 if end-start <= 1.5 else .25)
        for tick in np.arange(np.ceil(start/step)*step, end, step):
            x = x0+(tick-start)/(end-start)*(x1-x0)
            draw.text((x-26, top+511), f'{tick:.3f}' if step < .1 else f'{tick:.2f}', fill='black', font=small)
            draw.line((x, top+30, x, top+35), fill='#888888')
    image.save(destination)


def prepare(selection_path, out):
    out.mkdir(parents=True, exist_ok=True)
    selection = json.loads(selection_path.read_text())
    registry_path = ROOT/'tools/audio_analysis/eval/additional_stem_sources.json'
    registry = json.loads(registry_path.read_text())
    registry_rows = {s['folder'].lower().replace(' stems', '').replace(' ', '_'): s for s in registry['sets']}
    report = dict(selection_sha256=sha(selection_path), registry_sha256=sha(registry_path), script_sha256=sha(__file__),
        method='Native stereo excerpts with exact sample indices and no resampling/warping. Midnight source clock +.136375s only. All selected full source hashes verified. Raw waveform bins preserve extrema; centred STFT uses exact frame centres with nearest-bin display. No detector output.',
        sources={}, cores=[])
    for selected in selection['cores']:
        row = registry_rows[selected['track']]
        offset = .136375 if selected['track'] == 'midnight_patience' else 0.
        kick = next(f for f in row['files'] if f['role'] == 'named_stem' and 'kick' in f['file'].lower())
        paths = {'master': (Path(row['master']['path']), row['master']['sha256']),
                 'kick': (Path(registry['source_root'])/kick['file'], kick['sha256'])}
        core = dict(selected, review_start_s=max(0., selected['start_s']-.25),
                    review_end_s=selected['end_s']+.25, stem_to_master_seconds=offset, excerpts={})
        for role, (path, digest) in paths.items():
            if str(path) not in report['sources']:
                actual = sha(path)
                if actual != digest:
                    raise ValueError('registered source hash changed')
                report['sources'][str(path)] = dict(registered_sha256=digest, verified_sha256=actual)
            with sf.SoundFile(path) as handle:
                sr = handle.samplerate; source_offset = offset if role == 'kick' else 0.
                first = max(0, round((core['review_start_s']-source_offset)*sr))
                last = min(len(handle), round((core['review_end_s']-source_offset)*sr))
                handle.seek(first); values = handle.read(last-first, dtype='float64', always_2d=True)
            origin = first/sr+source_offset
            wav_path = out/f"{core['id']}_{role}.wav"
            sf.write(wav_path, values, sr, subtype='FLOAT')
            replay, replay_sr = sf.read(wav_path, dtype='float64', always_2d=True)
            if replay_sr != sr or not np.array_equal(values, replay):
                raise ValueError('native excerpt cache changed decoded samples')
            freq, times, power = spectrogram(values.T, fs=sr, window='hann', nperseg=2048,
                noverlap=1952, detrend=False, scaling='spectrum', mode='psd', axis=-1)
            times += origin; power = np.mean(power, axis=0)
            spec_path = out/f"{core['id']}_{role}_spectrogram.npz"
            np.savez_compressed(spec_path, frequency_hz=freq, times_master_s=times, power=power)
            core['excerpts'][role] = dict(path=str(wav_path), sha256=sha(wav_path), original_path=str(path),
                sample_rate=sr, source_sample_range=[first,last], master_axis_origin_s=origin,
                frames=len(values), channels=values.shape[1], decoded_samples_exact=True,
                mono_peak=float(np.max(np.abs(values.mean(axis=1)))), stereo_peak=float(np.max(np.abs(values))),
                spectrogram_path=str(spec_path), spectrogram_sha256=sha(spec_path))
            if role == 'master' and core['start_s'] == 0 and offset:
                core['leading_before_stem_available'] = dict(start_s=0., end_s=offset,
                    max_abs_stereo=float(np.max(np.abs(values[:round(offset*sr)]))))
        overview = out/f"{core['id']}_overview.png"
        edges = np.linspace(core['review_start_s'], core['review_end_s'], 5)
        render(core, list(zip(edges[:-1], edges[1:])), overview)
        core['overview'] = dict(path=str(overview), sha256=sha(overview))
        report['cores'].append(core)
        print('prepared', core['id'], 'source peak', core['excerpts']['kick']['mono_peak'], flush=True)
        (out/'raw_evidence_index.json').write_text(json.dumps(report, indent=2)+'\n')
    return report


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--selection', type=Path, default=CACHE/'validation_selection.json')
    parser.add_argument('--out', type=Path, default=CACHE/'additional_review')
    args = parser.parse_args()
    for name in ('OPENBLAS_NUM_THREADS','OMP_NUM_THREADS','VECLIB_MAXIMUM_THREADS'):
        if os.environ.get(name) != '1':
            raise ValueError(f'{name}=1 required')
    prepare(args.selection, args.out)
