#!/usr/bin/env python3
"""New snare songs from Peter's 2024/2025 projects: newest WIP mixdown against the newest .als (Peter, 2026-10-10:
the latest WIP matches the latest project; no other pairings are tried).

Timing proof (kick survey's wip_sections method): export start = whole bars + one lag (+-80 ms), chosen by the Pearson r
between a note pattern on the 16th grid and the mix's onset strength. Run twice, independently: kick notes against the
40-400 Hz onset strength, snare notes against the 1-8 kHz onset strength. Proven when both pick the same bar, or when
only one pattern exists and it wins clearly (r >= 0.3, margin >= 1 sd of the other candidates).
Writes GOAL/snare_wip/{song}.npy (48 kHz mono float32) and GOAL/snare_wip.json ({song: als, wip, offset_s, bpm, proof}).
Projects are read only through als_extract (sha-checked); nothing is written near them. Usage: detector_wip.py"""
import json
import os
import re
import sys
from concurrent.futures import ProcessPoolExecutor
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parents[3]))
import numpy as np  # noqa: E402

BASE = Path('/Users/peterkiemann/Library/CloudStorage/Dropbox/Music Production/Ableton Projects')
PROJECTS = {  # song -> project folder; duplicates of goal songs or of each other are left out
    'crush': '2024/Crush Project', 'dusk': '2024/Dusk Project', 'imitation': '2024/Imitation (Pusher) Project',
    'set_me_free': '2024/Limerant Project', 'one': '2024/One Project', 'openings': '2024/Openings 2024 Project',
    'touch': '2024/Sense (Touch) Project', 'better_strangers': '2024/Sound Design Session Project',
    'staged': '2024/Staged Project', 'just_the_way': '2024/Stay The Night Project', 'rush': '2024/Tension Project',
    'water_shimmer': '2024/Water Shimmer Project', 'basalt': '2025/Basalt Project', 'cadence': '2025/Cadence Project',
    'growth': '2025/Growth Project', 'linear': '2025/Linear Project', 'motion': '2025/Motion Project',
    'push': '2025/Push Project', 'stagnate': '2025/Stagnate Project', 'weight': '2025/Weight Project'}
AUDIO = ('.wav', '.mp3', '.aif', '.aiff', '.flac', '.m4a')
KICK = re.compile(r'kick|\bbd\b|bass ?drum', re.I)


def kick_beats(res):
    out = []
    for tr in res['tracks']:
        if tr.get('kind') != 'MidiTrack' or not tr.get('speaker_on', True):
            continue
        pads = {}

        def walk(devs):
            for d in devs or []:
                for b in d.get('branches') or []:
                    if b.get('key') is not None:
                        pads[b['key']] = b.get('name') or ''
                    walk(b.get('devices'))
        walk(tr.get('devices'))
        for c in tr['midi_clips']:
            for n in c['notes']:
                if (n['key'] in pads and KICK.search(pads[n['key']])) or (not pads and KICK.search(tr['name'])):
                    out.append(n['beat'])
    return np.unique(np.round(out, 4))


def snare_beats(res):
    from tools.audio_analysis.eval.snare_labels import NOT_SNARE, SNARE
    out = []
    for tr in res['tracks']:
        if tr.get('kind') != 'MidiTrack' or (not tr.get('speaker_on', True) and 'trigger' not in tr['name'].lower()):
            continue
        keys = set()

        def walk(devs):
            for d in devs or []:
                for b in d.get('branches') or []:
                    nm = b.get('name') or ''
                    if b.get('key') is not None and SNARE.search(nm) and not NOT_SNARE.search(nm):
                        keys.add(b['key'])
                    walk(b.get('devices'))
        walk(tr.get('devices'))
        for c in tr['midi_clips']:
            for n in c['notes']:
                if n['key'] in keys or (not keys and SNARE.search(tr['name']) and not NOT_SNARE.search(tr['name'])):
                    out.append(n['beat'])
    return np.unique(np.round(out, 4))


def prove(env, beats, dur, spb):
    from scipy.ndimage import maximum_filter1d
    from tools.audio_analysis.eval.kick_goal_wip_sections import LAGS, grid_corr, vals
    if len(beats) < 16:
        return None
    best, order = grid_corr(maximum_filter1d(env, 41), beats, dur, spb)
    if len(order) < 2:
        return None
    k0 = order[0]
    rs = np.array([best[k][0] for k in order])
    t = beats * spb - 4 * k0 * spb
    t = t[(t >= .6) & (t <= dur - .6)]
    lag = float(LAGS[int(np.argmax([vals(env, t + lg).mean() for lg in LAGS]))])
    return dict(k=int(k0), r=round(best[k0][0], 3), margin_sd=round(float((best[k0][0] - best[order[1]][0]) / (rs.std() + 1e-9)), 2), lag=lag)


def one(item):
    song, folder = item
    from scipy.ndimage import maximum_filter1d
    from tools.audio_analysis.eval.als_extract import extract
    from tools.audio_analysis.eval.kick_goal_eval import GOAL
    from tools.audio_analysis.eval.kick_goal_wip_sections import ENV_BIAS, band_amp_rise, load, onset_env
    proj = BASE / folder
    als = max(proj.glob('*.als'), key=lambda p: p.stat().st_mtime)
    wip = max((p for p in proj.iterdir() if p.is_file() and p.suffix.lower() in AUDIO), key=lambda p: p.stat().st_mtime)
    res = extract(str(als))
    row = dict(song=song, als=str(als), wip=str(wip))
    if len(res['tempo_points']) != 1:
        return dict(row, status='tempo automation')
    spb = 60 / res['tempo_points'][0]['bpm']
    x = load(str(wip))
    dur = len(x) / 48000
    kick = prove(onset_env(x), kick_beats(res), dur, spb)
    snare = prove(100 * maximum_filter1d(band_amp_rise(x, 1000, 8000), 7), snare_beats(res), dur, spb)
    row.update(bpm=res['tempo_points'][0]['bpm'], duration_s=round(dur, 1), kick=kick, snare=snare)
    clear = [p for p in (kick, snare) if p and p['r'] >= .3 and p['margin_sd'] >= 1.0]
    if kick and snare and kick['k'] == snare['k']:
        status = 'proven (kick and snare agree)'
    elif len([p for p in (kick, snare) if p]) == 1 and clear:
        status = 'proven (one pattern, clear win)'
    else:
        return dict(row, status='not proven')
    use = kick if kick and (not snare or kick['k'] == snare['k']) else snare
    lag = use['lag'] - (ENV_BIAS if use is kick else 0.0)
    row.update(status=status, offset_s=round(-4 * use['k'] * spb + lag, 4))
    (GOAL / 'snare_wip').mkdir(exist_ok=True)
    np.save(GOAL / 'snare_wip' / f'{song}.npy', x.astype(np.float32))
    return row


def main():
    from tools.audio_analysis.eval.kick_goal_eval import GOAL
    with ProcessPoolExecutor(6) as ex:
        rows = list(ex.map(one, PROJECTS.items()))
    for r in rows:
        k, s = r.get('kick'), r.get('snare')
        print(f"{r['song']:16s} {r['status']:30s} kick {k and (k['k'] * 4, k['r'], k['margin_sd'])} snare {s and (s['k'] * 4, s['r'], s['margin_sd'])} | {Path(r['wip']).name}", flush=True)
    (GOAL / 'snare_wip.json').write_text(json.dumps({r['song']: r for r in rows if 'offset_s' in r}, indent=1))


if __name__ == '__main__':
    main()
