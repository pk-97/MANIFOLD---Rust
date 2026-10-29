#!/usr/bin/env python3
"""Project songs from Peter's sets and their mixdowns. PROJECTS: newest WIP mixdown against the newest .als of a 2024/2025
project folder (Peter, 2026-10-10: the latest WIP matches the latest project). PAIRS: the label-ready songs of the
detector data catalogue (catalog.py usable --ready), each with the set and bounce chosen from every ready set against
every local bounce: the pair proven by the fit below, the set saved closest to the bounce, wav before mp3. A pair wins
over its song's project folder.

Timing proof (lab/offsets.prove_seconds, the kick survey's wip_sections method on the set's tempo map): export start =
a whole bar's time on the map + one lag (+-80 ms), chosen by the Pearson r between a note pattern on the 16th grid and
the mix's onset strength. Run twice, independently: kick notes against the 40-400 Hz onset strength, snare notes against
the 1-8 kHz onset strength. Proven when both pick the same bar, or when only one pattern exists and it wins clearly
(r >= 0.3, margin >= 1 sd of the other candidates), and the kick attacks at that offset are not off.
Writes GOAL/snare_wip/{song}.npy (48 kHz mono float32) and GOAL/snare_wip.json ({song: als, wip, offset_s, bpm, proof}).
Projects are read only through als_extract (sha-checked); nothing is written near them.
A recorded song is never proven again unless named, and a named project-folder song keeps its recorded set and mixdown
(a newer save in its folder never moves a proven offset); rows are merged into snare_wip.json, never rebuilt.
Usage: detector_wip.py [SONG...]   (no names: every song not yet recorded)"""
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
    'linear': '2025/Linear Project', 'motion': '2025/Motion Project',
    'push': '2025/Push Project', 'stagnate': '2025/Stagnate Project', 'weight': '2025/Weight Project'}
AUDIO = ('.wav', '.mp3', '.aif', '.aiff', '.flac', '.m4a')
# Song -> (set, bounce) under the music folder. All In For You (all_in_for_you, the Growth project folder) is left out:
# its sets' kick notes hit the T2 kick stem before the 164-110-164 bpm ramps and miss it after them, and the grid fit
# picks the live-edit block after the song's end (27 of 129 kicks) over the song itself (BUG-fffrv (album and EP songs)).
# Know You're There: the live-edit set holds the premaster set's notes and
# clips unchanged before the song's end (bar 136) and only mutes some; the premaster set's notes after the end send its
# snare fit to bar 184.
PAIRS = {
    'affect': ('Ableton Projects/2025/Affect Project/Affect V2.als', 'Album Draft 2027/11 - Affect (Give Me A Reason) - WIP 2.mp3'),
    'break': ('Ableton Projects/Ableton 11/Break Project/Break - SONIBLE.als',
              'Ableton Projects/Ableton 11/Break Project/FINALS/LATENT SPACE - Break - [FINAL 02].wav'),
    'concentrate': ('Ableton Projects/Ableton 11/Faux Project/Faux.als', 'Ableton Projects/Ableton 11/Faux Project/Concentrate WIP 3.mp3'),
    'control': ('Ableton Projects/Ableton 11/Rise Project/Rise.als', 'Ableton Projects/Ableton 11/Rise Project/Control - WIP 5.mp3'),
    'desire': ('Ableton Projects/Ableton 11/Wait Project/Wait.als', 'Ableton Projects/Ableton 11/Wait Project/Wait WIP 2.mp3'),
    'drmendez': ('Ableton Projects/Ableton 11/DRMENDEZ TIMEPLATE Project/DRMENDEZ - PROJECT 3 ( ORIGNAL MIX )  - No Suite.als',
                 'Ableton Projects/Ableton 11/DRMENDEZ TIMEPLATE Project/DRMENDEZ - PROJECT 3 (PK EDIT).mp3'),
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


def kick_seconds(res):
    """kick_beats in arrangement seconds through the set's tempo map. One tempo keeps beats * 60 / bpm, the arithmetic
    the recorded labels were built with, bit for bit."""
    from tools.audio_analysis.eval.als_extract import beat_seconds
    points = [(q['beat'], q['bpm']) for q in res['tempo_points']]
    beats = kick_beats(res)
    if len(points) == 1:
        return beats * 60 / points[0][1]
    return np.array([beat_seconds(points, b) for b in beats], float)


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


def source(song, recorded=None):
    """(set, mixdown) paths: the song's catalogue pair, else its recorded pair (a snare_wip.json row), else the newest
    .als and audio file of its project folder."""
    if song in PAIRS:
        return tuple(BASE.parent / p for p in PAIRS[song])
    if recorded:
        return Path(recorded['als']), Path(recorded['wip'])
    proj = BASE / PROJECTS[song]
    als = max(proj.glob('*.als'), key=lambda p: p.stat().st_mtime)
    wip = max((p for p in proj.iterdir() if p.is_file() and p.suffix.lower() in AUDIO), key=lambda p: p.stat().st_mtime)
    return als, wip


def one(song, recorded=None):
    from tools.audio_analysis.eval.als_extract import extract
    from tools.audio_analysis.eval.kick_goal_eval import GOAL
    from tools.audio_analysis.eval.kick_goal_wip_sections import load
    from tools.audio_analysis.lab.offsets import prove_seconds
    als, wip = source(song, recorded)
    row = dict(song=song, als=str(als), wip=str(wip))
    from tools.audio_analysis.catalog.catalog import is_dataless
    online = [str(p) for p in (als, wip) if is_dataless(p)]
    if online:
        return dict(row, status=f'not proven (online-only, not read: {online})')
    res = extract(str(als))
    x = load(str(wip))
    got = prove_seconds(res, x)
    row.update(bpm=res['tempo_points'][0]['bpm'], tempo_points=len(res['tempo_points']), duration_s=round(len(x) / 48000, 1),
               kick=got['kick'], snare=got['snare'], attack=got.get('attack'), status=got['status'])
    if 'offset_s' not in got or not got['status'].startswith('proven'):
        return row
    row.update(offset_s=got['offset_s'])
    (GOAL / 'snare_wip').mkdir(exist_ok=True)
    np.save(GOAL / 'snare_wip' / f'{song}.npy', x.astype(np.float32))
    return row


def main():
    from tools.audio_analysis.lab import recipe
    recipe.apply()
    from tools.audio_analysis.eval.kick_goal_eval import GOAL
    path = GOAL / 'snare_wip.json'
    out = json.loads(path.read_text()) if path.exists() else {}
    named = sys.argv[1:]
    songs = named or [t for t in dict.fromkeys(list(PROJECTS) + list(PAIRS)) if t not in out]
    unknown = [t for t in songs if t not in PROJECTS and t not in PAIRS]
    if unknown:
        sys.exit(f'not a project or catalogue pair: {unknown}')
    with ProcessPoolExecutor(6) as ex:
        rows = list(ex.map(one, songs, [out.get(t) for t in songs]))
    for r in rows:
        k, s = r.get('kick'), r.get('snare')
        print(f"{r['song']:16s} {r['status']:50s} kick {k and (k['k'] * 4, k['r'], k['margin_sd'])} snare {s and (s['k'] * 4, s['r'], s['margin_sd'])} | {Path(r['wip']).name}", flush=True)
        if r['song'] in out and 'offset_s' not in r:
            print(f"  {r['song']}: recorded at {out[r['song']]['offset_s']} s, no longer proven: removed", flush=True)
    for r in rows:
        out.pop(r['song'], None)
        if 'offset_s' in r:
            out[r['song']] = r
    path.write_text(json.dumps(out, indent=1))


if __name__ == '__main__':
    main()
