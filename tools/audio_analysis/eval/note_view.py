#!/usr/bin/env python3
"""Render a label-check PNG for any detector family: the mix spectrogram, waveform and family-band level, with the
family's labels on top. Green = event (tag: kind's first letter), grey band = unscored, red tick = vouched
negative, blue triangle = fire (--fires).
Usage: note_view.py OUT.png FAMILY SONG START_S DUR_S [--fires a,b,c]   (reads GOAL/{FAMILY}_labels.json)"""
import argparse
import importlib
import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[3]))

import numpy as np  # noqa: E402
from PIL import Image, ImageDraw  # noqa: E402

from tools.audio_analysis.eval.kick_goal_view import FMAX, FMIN, LANE_H, LEFT, SPEC_H, W, band_env_db, segment, spectrogram  # noqa: E402


def render(out, family, song, start, dur, fires=()):
    from tools.audio_analysis.eval.detector_songs import audio, rate
    from tools.audio_analysis.eval.kick_goal_eval import GOAL, Goal
    fam = importlib.import_module(f'tools.audio_analysis.eval.{family}_labels').FAMILY
    s = json.loads((GOAL / f'{family}_labels.json').read_text())[song]
    g = Goal(mode='v3')
    x, sr = audio(g, song), rate(g, song)
    seg = segment(x, sr, start, dur)
    height = 24 + SPEC_H + 2 * LANE_H + 30
    img = Image.new('RGB', (W, height), (16, 16, 20))
    dr = ImageDraw.Draw(img)
    dr.text((6, 4), f'{song} {family} {start:.2f}-{start + dur:.2f}s   green event  grey unscored  red vouched negative  blue fire',
            fill=(230, 230, 230))

    def x_of(t):
        return LEFT + int((t - start) / dur * (W - LEFT))
    for a, b in s['unscored']:
        if b >= start and a <= start + dur:
            dr.rectangle((max(LEFT, x_of(a)), 24, min(W, x_of(b)), height - 30), fill=(48, 48, 56))
    spec = spectrogram(seg, sr)
    rgb = np.stack([np.clip(2 * spec, 0, 1), np.clip(2 * spec - .5, 0, 1), np.clip(2 * spec - 1, 0, 1)], axis=-1)
    img.paste(Image.fromarray((255 * rgb).astype(np.uint8)), (LEFT, 24))
    for f in (50, 100, 200, 500, 1000, 2000, 5000):
        dr.text((4, 24 + int(SPEC_H * np.log(FMAX / f) / np.log(FMAX / FMIN)) - 6), f'{f} Hz', fill=(200, 200, 200))
    y = 24 + SPEC_H
    peak = np.abs(seg).max() + 1e-9
    dr.text((4, y + 4), 'mix wave', fill=(220, 220, 220))
    for c, chunk in enumerate(np.array_split(seg, W - LEFT)):
        if len(chunk):
            mid = y + LANE_H // 2
            dr.line((LEFT + c, mid - int(chunk.max() / peak * (LANE_H / 2 - 3)), LEFT + c, mid - int(chunk.min() / peak * (LANE_H / 2 - 3))), fill=(170, 170, 170))
    y += LANE_H
    env = band_env_db(seg, sr, *fam.band)
    dr.text((4, y + 4), f'{fam.band[0]}-{fam.band[1]} Hz', fill=(220, 220, 220))
    top = env.max()
    pts = [(LEFT + int(i / max(1, len(env) - 1) * (W - LEFT - 1)), y + LANE_H - 4 - int(np.clip((v - top + 50) / 50, 0, 1) * (LANE_H - 8)))
           for i, v in enumerate(env)]
    dr.line(pts, fill=(120, 200, 255), width=1)
    y += LANE_H
    for t in s['hard_neg']:
        if start <= t <= start + dur:
            dr.line((x_of(t), y - 18, x_of(t), y), fill=(230, 60, 60), width=2)
    for t, k in zip(s['positives'], s.get('kinds') or ['e'] * len(s['positives'])):
        if start <= t <= start + dur:
            dr.line((x_of(t), 36, x_of(t), y - 20), fill=(60, 220, 90), width=1)
            dr.text((x_of(t) + 2, 24), k[0], fill=(60, 220, 90))
    for t in fires:
        if start <= t <= start + dur:
            dr.polygon([(x_of(t) - 5, 24), (x_of(t) + 5, 24), (x_of(t), 34)], fill=(80, 140, 255))
    for t in np.arange(np.ceil(start), start + dur, 1.0):
        dr.text((x_of(t) - 8, y + 8), f'{t:.0f}s', fill=(220, 220, 220))
    img.save(out)
    return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('out')
    ap.add_argument('family')
    ap.add_argument('song')
    ap.add_argument('start', type=float)
    ap.add_argument('dur', type=float)
    ap.add_argument('--fires', default='')
    a = ap.parse_args()
    render(a.out, a.family, a.song, a.start, a.dur, [float(v) for v in a.fires.split(',') if v])


if __name__ == '__main__':
    main()
