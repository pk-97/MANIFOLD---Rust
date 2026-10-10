#!/usr/bin/env python3
"""Render a label-check PNG: mix spectrogram and waveform, stem envelopes, labels.

Usage: kick_goal_view.py OUT.png SONG START_S DUR_S [--fires a,b,c]
SONG is a dev track or a new song in labels_v2.json. Markers: green = kept
label, red = removed label, orange = flagged unlabelled stem attack, magenta
tick = fresh kick-stem attack (stem time mapped to the mix), blue tick = fire.
"""
from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[3]))

import numpy as np  # noqa: E402
from PIL import Image, ImageDraw  # noqa: E402
from scipy.signal import butter, sosfilt  # noqa: E402

from tools.audio_analysis.eval.kick_attack_rejection import read_audio  # noqa: E402
from tools.audio_analysis.eval.kick_goal_labels import DEV_STEMS, GOAL, NEW, drum_kicks, fresh_onsets, kick_env_db  # noqa: E402
from tools.audio_analysis.eval.kick_night_common import NIGHT  # noqa: E402
from tools.audio_analysis.eval.kick_night_stem_audit import AUDIO  # noqa: E402

W, SPEC_H, LANE_H, LEFT = 1600, 220, 70, 90
FMIN, FMAX = 30.0, 8000.0


def song_audio(song, labels):
    """(sr, mix, {lane: (signal, lag_s)}) for a dev or new song."""
    if song in labels['new']:
        info, cfg = labels['new'][song], NEW[song]
        sr = info['sample_rate']
        mix = (np.load(GOAL / f'{song}_mix.npy').astype(np.float64) if info['mix_path'] is None
               else read_audio(info['mix_path'], sr)[1])
        shift = info['kick_lag_ms'] / 1000
        lanes = {}
        if cfg['kick'] is not None:
            lanes['kick stem'] = (read_audio(str(cfg['kick']), sr)[1], shift)
        if cfg['drums']:
            lanes['drum stem' if cfg['kick'] is None else 'drum bus'] = (read_audio(str(cfg['drums'][0]), sr)[1], shift)
        if cfg['bass']:
            lanes['bass'] = (read_audio(str(cfg['bass'][0]), sr)[1], shift)
        return sr, mix, lanes
    from tools.audio_analysis.eval.kick_night_common import Data
    src = Data().records[song]['source']
    sr, mix = read_audio(src['audio_path'])
    if song in DEV_STEMS:
        lag = json.loads((NIGHT / 'snapped_labels_frozen.json').read_text())['tracks'][song]['song_lag_ms'] / 1000
        cfg = DEV_STEMS[song]
        return sr, mix, {'kick stem': (read_audio(str(cfg['kick']), sr)[1], lag),
                         'drum bus': (read_audio(str(cfg['drums'][0]), sr)[1], lag),
                         'bass': (read_audio(str(cfg['bass'][0]), sr)[1], lag)}
    return sr, mix, {'drum stem': (read_audio(str(AUDIO / song / 'drums.wav'), sr)[1], 0.0),
                     'bass': (read_audio(str(AUDIO / song / 'bass.wav'), sr)[1], 0.0)}


def segment(x, sr, start, dur, lag=0.0):
    a, b = int((start - lag) * sr), int((start - lag + dur) * sr)
    out = np.zeros(b - a)
    lo, hi = max(a, 0), min(b, len(x))
    if hi > lo:
        out[lo - a:hi - a] = x[lo:hi]
    return out


def spectrogram(x, sr):
    n, hop = 2048, max(1, len(x) // (W - LEFT))
    pad = np.concatenate([np.zeros(n // 2), x, np.zeros(n)])
    win = np.hanning(n)
    cols = []
    for c in range(W - LEFT):
        f = pad[c * hop:c * hop + n] * win
        cols.append(np.abs(np.fft.rfft(f)))
    s = 20 * np.log10(np.array(cols).T + 1e-9)
    freqs = np.fft.rfftfreq(n, 1 / sr)
    rows = np.geomspace(FMAX, FMIN, SPEC_H)
    idx = np.clip(np.searchsorted(freqs, rows), 1, len(freqs) - 1)
    img = s[idx]
    top = img.max()
    return np.clip((img - (top - 80)) / 80, 0, 1)


def band_env_db(x, sr, lo=None, hi=None, frame=.002):
    if lo is not None:
        x = sosfilt(butter(4, (lo, hi), btype='band', fs=sr, output='sos'), x)
    n = max(1, int(frame * sr))
    k = len(x) // n
    return 20 * np.log10(np.sqrt(np.mean(x[:k * n].reshape(k, n) ** 2, axis=1)) + 1e-9)


def colour(v):
    v = float(v)
    return (int(255 * min(1, 2 * v)), int(255 * max(0, min(1, 2 * v - .5))), int(255 * max(0, 2 * v - 1)))


def render(out, song, start, dur, fires=()):
    labels = json.loads((GOAL / 'labels_v2.json').read_text())
    sr, mix, lanes = song_audio(song, labels)
    pre = 1.0
    seg = segment(mix, sr, start - pre, dur + pre)
    lanes_seg = {k: (segment(x, sr, start - pre, dur + pre, lag), x, lag) for k, (x, lag) in lanes.items()}
    body = slice(int(pre * sr), int((pre + dur) * sr))
    height = 24 + SPEC_H + LANE_H * (2 + len(lanes)) + 30
    img = Image.new('RGB', (W, height), (16, 16, 20))
    dr = ImageDraw.Draw(img)
    dr.text((6, 4), f'{song}  {start:.2f}-{start + dur:.2f}s   green kept  red removed  orange flagged  magenta fresh kick-stem attack  blue fire',
            fill=(230, 230, 230))
    spec = spectrogram(seg[body], sr)
    rgb = np.stack([np.clip(2 * spec, 0, 1), np.clip(2 * spec - .5, 0, 1), np.clip(2 * spec - 1, 0, 1)], axis=-1)
    img.paste(Image.fromarray((255 * rgb).astype(np.uint8)), (LEFT, 24))
    for f in (50, 100, 200, 500, 1000, 2000, 5000):
        r = 24 + int(SPEC_H * np.log(FMAX / f) / np.log(FMAX / FMIN))
        dr.text((4, r - 6), f'{f} Hz', fill=(200, 200, 200))
    y = 24 + SPEC_H

    def x_of(t):
        return LEFT + int((t - start) / dur * (W - LEFT))

    def lane(name, values, lo_db=-60):
        nonlocal y
        dr.rectangle((LEFT, y, W, y + LANE_H - 2), fill=(26, 26, 32))
        dr.text((4, y + 4), name, fill=(220, 220, 220))
        v = np.asarray(values)
        top = v.max() if len(v) else 0
        pts = []
        for i, val in enumerate(v):
            px = LEFT + int(i / max(1, len(v) - 1) * (W - LEFT - 1))
            py = y + LANE_H - 4 - int(np.clip((val - (top + lo_db)) / -lo_db, 0, 1) * (LANE_H - 8))
            pts.append((px, py))
        if len(pts) > 1:
            dr.line(pts, fill=(120, 200, 255), width=1)
        y += LANE_H
        return y - LANE_H

    w = seg[body]
    cols = np.array_split(w, W - LEFT)
    dr.rectangle((LEFT, y, W, y + LANE_H - 2), fill=(26, 26, 32))
    dr.text((4, y + 4), 'mix wave', fill=(220, 220, 220))
    peak = np.abs(w).max() + 1e-9
    for c, chunk in enumerate(cols):
        if len(chunk):
            a, b = chunk.min() / peak, chunk.max() / peak
            mid = y + LANE_H // 2
            dr.line((LEFT + c, mid - int(b * (LANE_H / 2 - 3)), LEFT + c, mid - int(a * (LANE_H / 2 - 3))), fill=(170, 170, 170))
    y += LANE_H
    lane('mix 45-140', band_env_db(seg, sr, 45, 140)[int(pre / .002):int((pre + dur) / .002)])
    kick_lane_y = None
    for name, (s_seg, full, lag) in lanes_seg.items():
        if name == 'kick stem':
            env = kick_env_db(s_seg, sr)[int(pre / .001):int((pre + dur) / .001)]
            ly = lane('kick stem env', env)
            ticks = fresh_onsets(kick_env_db(full, sr)) + lag
        else:
            ly = lane(name + ' 45-140', band_env_db(s_seg, sr, 45, 140)[int(pre / .002):int((pre + dur) / .002)])
            ticks = drum_kicks(full, sr) + lag if name in ('drum stem', 'drum bus') else np.array([])
        for t in ticks[(ticks >= start) & (ticks <= start + dur)]:
            dr.line((x_of(t), ly, x_of(t), ly + 14), fill=(255, 60, 255), width=3)
    info = labels['dev'].get(song) or {}
    tag = dict(kick_tail='tail', bass_only='bass', no_attack='none', drums_bus_kick='bus')
    marks = [(r['t'], (60, 220, 90), False, tag.get(r['cls'], 'K')) for r in info.get('kept', [])]
    marks += [(r['t'], (240, 60, 60), True, tag.get(r['cls'], 'X')) for r in info.get('removed', [])]
    marks += [(t, (255, 160, 0), True, '?') for t in info.get('flagged_missing', [])]
    if song in labels['new']:
        marks += [(t, (60, 220, 90), False, 'K') for t in labels['new'][song]['labels']]
    for t, col, dashed, text in marks:
        if start <= t <= start + dur:
            x = x_of(t)
            for yy in range(36, y, 7 if dashed else 1):
                dr.line((x, yy, x, yy + (4 if dashed else 1)), fill=col, width=2)
            dr.text((x + 3, 24), text, fill=col)
    for t in fires:
        if start <= t <= start + dur:
            dr.polygon([(x_of(t) - 5, 24), (x_of(t) + 5, 24), (x_of(t), 34)], fill=(80, 140, 255))
    for s in np.arange(np.ceil(start * 4) / 4, start + dur, .25):
        x = x_of(s)
        dr.line((x, y, x, y + 6), fill=(200, 200, 200))
        if abs(s - round(s)) < 1e-6:
            dr.text((x - 10, y + 8), f'{s:.0f}s', fill=(220, 220, 220))
        else:
            dr.text((x - 12, y + 8), f'{s:.2f}', fill=(140, 140, 140))
    img.save(out)
    return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('out')
    ap.add_argument('song')
    ap.add_argument('start', type=float)
    ap.add_argument('dur', type=float)
    ap.add_argument('--fires', default='')
    a = ap.parse_args()
    fires = [float(v) for v in a.fires.split(',') if v]
    print(render(a.out, a.song, a.start, a.dur, fires))


if __name__ == '__main__':
    main()
