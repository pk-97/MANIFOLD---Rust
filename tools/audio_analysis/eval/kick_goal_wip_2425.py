#!/usr/bin/env python3
"""Kick section labels for the proven 2024/2025 WIP mixdowns (detector_wip: newest WIP against newest .als, offset
proven by matching kick and snare patterns), through kick_goal_wip_sections' own proof and section rules.

Kick sources: every unmuted MIDI track named for a kick without drum-rack pads (each project has a dedicated kick
track; drum racks and kick audio tracks stay drum sources that block their sections). The section tool re-proves the
offset; songs where it disagrees with detector_wip by more than 20 ms are reported and left out.
Usage: kick_goal_wip_2425.py   (writes ~/.cache/manifold/ableton/{project stem}.json extracts and wip_labels/{song}.json)
"""
import json
import re
import sys
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parents[3]))
from tools.audio_analysis.eval import kick_goal_wip_sections as S  # noqa: E402

KICK = re.compile(r'kick|\bbd\b|bass ?drum', re.I)
SKIP = ('one',)  # detector_songs.SKIP: its WIP duplicates a goal song


def has_pads(tr):
    def walk(devs):
        return any(b.get('key') is not None or walk(b.get('devices')) for d in devs or [] for b in d.get('branches') or [])
    return walk(tr.get('devices'))


def main():
    from tools.audio_analysis.eval.als_extract import extract
    from tools.audio_analysis.eval.kick_goal_eval import GOAL
    W = json.loads((GOAL / 'snare_wip.json').read_text())
    for t, w in W.items():
        if t in SKIP:
            continue
        res = extract(w['als'])
        js = Path(w['als']).stem
        (S.C / f'{js}.json').write_text(json.dumps(res))
        sources = [tr['name'] for tr in res['tracks'] if tr.get('kind') == 'MidiTrack' and tr.get('speaker_on', True)
                   and KICK.search(tr['name']) and not has_pads(tr) and any(c['notes'] for c in tr['midi_clips'])]
        S.PROJECTS[t] = (w['wip'], js, sources)
        print(f'{t}: sources {sources}', flush=True)
        S.run(t)
        out = json.loads((S.OUT / f'{t}.json').read_text())
        d = abs(out['offset_s'] - w['offset_s'])
        if d > .020:
            print(f'{t}: section proof offset {out["offset_s"]:.4f} s vs detector_wip {w["offset_s"]:.4f} s ({d * 1000:.0f} ms): left out', flush=True)
            (S.OUT / f'{t}.json').rename(S.OUT / f'{t}.disagree.json')


if __name__ == '__main__':
    main()
