#!/usr/bin/env python3
"""Project songs from Peter's sets and their mixdowns. PROJECTS: newest WIP mixdown against the newest .als of a 2024/2025
project folder (Peter, 2026-10-10: the latest WIP matches the latest project). PAIRS: the label-ready songs of the
detector data catalogue (catalog.py usable --ready), each with the set and bounce chosen from every ready set against
every local bounce: the pair proven by the fit below, the set saved closest to the bounce, wav before mp3. A pair wins
over its song's project folder.

Timing proof (kick survey's wip_sections method): export start = whole bars + one lag (+-80 ms), chosen by the Pearson r
between a note pattern on the 16th grid and the mix's onset strength. Run twice, independently: kick notes against the
40-400 Hz onset strength, snare notes against the 1-8 kHz onset strength. Proven when both pick the same bar, or when
only one pattern exists and it wins clearly (r >= 0.3, margin >= 1 sd of the other candidates). The offset is the kick
fit's, unless it sits on the kick's body rather than its attack (export_offset).
Writes GOAL/snare_wip/{song}.npy (48 kHz mono float32) and GOAL/snare_wip.json ({song: als, wip, offset_s, bpm, proof}).
Projects are read only through als_extract (sha-checked); nothing is written near them.
Usage: detector_wip.py [SONG...]   (named songs are proven again and merged into snare_wip.json; the rest are kept)"""
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
# Song -> (set, bounce) under the music folder. Know You're There: the live-edit set holds the premaster set's notes and
# clips unchanged before the song's end (bar 136) and only mutes some; the premaster set's notes after the end send its
# snare fit to bar 184.
PAIRS = {
    'affect': ('Ableton Projects/2025/Affect Project/Affect V2.als', 'Album Draft 2027/11 - Affect (Give Me A Reason) - WIP 2.mp3'),
    'break': ('Ableton Projects/Ableton 11/Break Project/Break - SONIBLE.als',
              'Ableton Projects/Ableton 11/Break Project/FINALS/LATENT SPACE - Break - [FINAL 02].wav'),
    'concentrate': ('Ableton Projects/Ableton 11/Faux Project/Faux.als', 'Ableton Projects/Ableton 11/Faux Project/Concentrate WIP 3.mp3'),
    'control': ('Ableton Projects/Ableton 11/Rise Project/Rise.als', 'Ableton Projects/Ableton 11/Rise Project/Control - WIP 5.mp3'),
    'desire': ('Ableton Projects/Ableton 11/Wait Project/Wait.als', 'Ableton Projects/Ableton 11/Wait Project/Wait WIP 2.mp3'),
    'dream': ('Ableton Projects/Ableton 11/Idle Project/Idle.als',
              'Ableton Projects/Ableton 11/Idle Project/FINALS/LATENT SPACE - Dream [FINAL MIX 3].wav'),
    'echo': ('Ableton Projects/Ableton 11/Clue Project/Clue V2.als',
             'Ableton Projects/Ableton 11/Clue Project/FINAL - MAYBE/LATENT SPACE - Echo [FINAL].wav'),
    'fortuity': ('Ableton Projects/2025/Fortuity Project/Fortuity V2.als',
                 'Album Draft 2027/12 - Fortuity (Flowers In Her Head - Draft Vox) - WIP 3 - SINGLE.mp3'),
    'go_with_you': ('Ableton Projects/Ableton 11/Bright Project/Bright.als', 'Ableton Projects/Ableton 11/Bright Project/Go With You WIP 1.mp3'),
    'know_youre_there': ("Ableton Projects/2024/Sound Design Session Project/05 - Know You're There - LIVE EDITS.als",
                         "EMERGENCE - ALBUM 2026/PRE MASTERS T2/05 - Know You're There - PREMASTER T2.wav"),
    'mendez': ('Ableton Projects/Ableton 11/Mendez X PK Project/Mendez X PK V2.als',
               'Ableton Projects/Ableton 11/Mendez X PK Project/Mendez X PK - The Future.mp3'),
    'motion': ('Ableton Projects/2025/Motion Project/Motion - Dub - Mixdown V2.als', 'Ableton Projects/2025/Motion Project/Motion - WIP 9.mp3'),
    'paces': ('Ableton Projects/Ableton 11/Map Project/Map.als',
              'Ableton Projects/Ableton 11/Map Project/FINALS/LATENT SPACE - Paces - [FINAL 02].wav'),
    'pulse': ('Ableton Projects/Ableton 11/Pulse Project/Pulse.als', 'Ableton Projects/Ableton 11/Pulse Project/Pulse - Concept 1.mp3'),
    'running': ('Ableton Projects/Ableton 11/Running Remaster - SONIBLE Project/Running Remaster - FINAL.als',
                'Ableton Projects/Ableton 11/Running Remaster - SONIBLE Project/FINALS/LATENT SPACE - Running - [FINAL 03].wav'),
    'skyline': ('Ableton Projects/Ableton 11/Stay Project/Stay - BETA.als', 'Ableton Projects/Ableton 11/Stay Project/Skyline WIP 1.mp3'),
    'sol': ('Ableton Projects/Ableton 11/Sol Project/Sol V2 - Update.als',
            'Ableton Projects/Ableton 11/Sol Project/FINAL/LATENT SPACE - Sol [REMASTER FINAL].wav'),
    'touch': ('Ableton Projects/2024/Sense (Touch) Project/Sense - Album Master.als',
              'EMERGENCE - ALBUM 2026/PRE MASTERS/10 - Touch - ALBUM FINAL 2 - PREMASTER.wav'),
    'waypoints': ('Ableton Projects/Other And Backups/Ways Project/Waypoints PREMASTER T2.als',
                  'EMERGENCE - ALBUM 2026/PRE MASTERS T2/09 - Waypoints - PREMASTER T2.wav'),
}
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


# The kick fit reads the onset-envelope peak, which a kick whose sub swell peaks after its attack drags late (echo: 17 ms;
# BUG-a2pma (labels timing_shift)); the snare fit reads the 1-8 kHz rise itself. On every other proven song the two
# implied offsets differ by -8.6..+8.4 ms (2026-10-13); the early side has no late-swell failure, so that spread is the
# fits' ordinary disagreement. A kick fit later than the snare's by more than KICK_LATE_S is on the kick's body: the
# snare's lag is used (echo, crush, dusk, cadence). Beyond KICK_BODY_S no kick body explains the gap and the snare fit
# is the one that failed (concentrate, openings: 130 ms).
KICK_LATE_S = .010
KICK_BODY_S = .050
CLEAR_R, CLEAR_SD = .3, 1.0   # one pattern alone proves the offset when it wins this clearly


def implied_offsets(kick, snare, spb):
    """{pattern: the export offset its fit implies}."""
    from tools.audio_analysis.eval.kick_goal_wip_sections import ENV_BIAS
    return {name: -4 * p['k'] * spb + p['lag'] - bias for name, p, bias in (('kick', kick, ENV_BIAS), ('snare', snare, 0.0)) if p}


def export_offset(kick, snare, spb):
    """(status, offset s) from the kick and snare fits, or None when not proven. Proven when both pick the same bar
    (the kick's lag, or the snare's when the kick fit sits on the kick's body: KICK_LATE_S), or when only one pattern
    exists and it wins clearly (r >= 0.3, margin >= 1 sd)."""
    off = implied_offsets(kick, snare, spb)
    if kick and snare and kick['k'] == snare['k']:
        if KICK_LATE_S < off['kick'] - off['snare'] <= KICK_BODY_S:
            return 'proven (kick and snare agree; snare lag, the kick fit sits on the kick body)', off['snare']
        return 'proven (kick and snare agree)', off['kick']
    if len(off) == 1 and all(p['r'] >= CLEAR_R and p['margin_sd'] >= CLEAR_SD for p in (kick, snare) if p):
        return 'proven (one pattern, clear win)', next(iter(off.values()))
    return None


def source(song):
    """(set, mixdown) paths: the song's catalogue pair, else the newest .als and audio file of its project folder."""
    if song in PAIRS:
        return tuple(BASE.parent / p for p in PAIRS[song])
    proj = BASE / PROJECTS[song]
    als = max(proj.glob('*.als'), key=lambda p: p.stat().st_mtime)
    wip = max((p for p in proj.iterdir() if p.is_file() and p.suffix.lower() in AUDIO), key=lambda p: p.stat().st_mtime)
    return als, wip


def one(song):
    from scipy.ndimage import maximum_filter1d
    from tools.audio_analysis.eval.als_extract import extract
    from tools.audio_analysis.eval.kick_goal_eval import GOAL
    from tools.audio_analysis.eval.kick_goal_wip_sections import band_amp_rise, load, onset_env
    als, wip = source(song)
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
    got = export_offset(kick, snare, spb)
    if got is None:
        return dict(row, status='not proven')
    row.update(status=got[0], offset_s=round(got[1], 4))
    (GOAL / 'snare_wip').mkdir(exist_ok=True)
    np.save(GOAL / 'snare_wip' / f'{song}.npy', x.astype(np.float32))
    return row


def main():
    from tools.audio_analysis.eval.kick_goal_eval import GOAL
    named = sys.argv[1:]
    songs = named or list(dict.fromkeys(list(PROJECTS) + list(PAIRS)))
    unknown = [t for t in songs if t not in PROJECTS and t not in PAIRS]
    if unknown:
        sys.exit(f'not a project or catalogue pair: {unknown}')
    with ProcessPoolExecutor(6) as ex:
        rows = list(ex.map(one, songs))
    for r in rows:
        k, s = r.get('kick'), r.get('snare')
        print(f"{r['song']:16s} {r['status']:30s} kick {k and (k['k'] * 4, k['r'], k['margin_sd'])} snare {s and (s['k'] * 4, s['r'], s['margin_sd'])} | {Path(r['wip']).name}", flush=True)
    path = GOAL / 'snare_wip.json'
    out = json.loads(path.read_text()) if named and path.exists() else {}
    for r in rows:
        out.pop(r['song'], None)
        if 'offset_s' in r:
            out[r['song']] = r
    path.write_text(json.dumps(out, indent=1))


if __name__ == '__main__':
    main()
