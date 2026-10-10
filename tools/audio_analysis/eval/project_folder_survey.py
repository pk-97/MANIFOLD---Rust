#!/usr/bin/env python3
"""Read-only survey of Peter's 2024 and 2025 Ableton project folders for snare/clap training songs.

Per project: newest .als in the project root, rendered audio in the project root (a mixdown candidate), snare/clap
notes (snare_labels.project_parts: named tracks and drum-rack pads, rim excluded), other-drum audio spans, and whether
the goal set already uses it. Projects are read only through als_extract (sha-checked); nothing is written near them.
Writes project_folder_survey.json beside this script."""
import json
import os
import sys
from concurrent.futures import ProcessPoolExecutor
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parents[3]))

BASE = Path('/Users/peterkiemann/Library/CloudStorage/Dropbox/Music Production/Ableton Projects')
AUDIO = ('.wav', '.mp3', '.aif', '.aiff', '.flac', '.m4a')


def one(proj):
    from tools.audio_analysis.eval.snare_labels import project_parts
    from tools.audio_analysis.eval.als_extract import extract
    als = sorted(proj.glob('*.als'), key=lambda p: p.stat().st_mtime)
    renders = sorted(str(p.name) for p in proj.iterdir() if p.is_file() and p.suffix.lower() in AUDIO)
    row = dict(project=str(proj), als=str(als[-1]) if als else None, n_als=len(als), renders=renders)
    if als:
        try:
            res = extract(str(als[-1]))
            snare, rim, spans, clips = project_parts(res)
            row.update(snare_notes=len(snare), rim_notes=len(rim), loop_s=round(sum(b - a for a, b in spans), 1),
                       snare_clips=sum(len(v) for k, v in clips.items() if k != '_rim'),
                       span_s=[round(float(min(snare)), 1), round(float(max(snare)), 1)] if len(snare) else None)
        except Exception as e:  # a broken or unreadable set is reported, not fatal
            row['error'] = f'{type(e).__name__}: {e}'[:120]
    return row


def main():
    from tools.audio_analysis.eval.kick_goal_eval import Goal
    from tools.audio_analysis.eval.kick_goal_melodic import _source
    used = {}
    for t in Goal(mode='v3').records:
        s = _source(t)
        if s:
            used[str(Path(s[0]).parent)] = t
    projs = [p for y in ('2024', '2025') for p in sorted((BASE / y).iterdir()) if p.is_dir()]
    with ProcessPoolExecutor(6) as ex:
        rows = list(ex.map(one, projs))
    for r in rows:
        r['goal_song'] = used.get(r['project'])
        print(f"{Path(r['project']).parent.name}/{Path(r['project']).name[:34]:34s} snare {r.get('snare_notes', '-'):>4} "
              f"clips {r.get('snare_clips', '-'):>3} loops {r.get('loop_s', '-'):>6} s renders {len(r['renders']):2d}"
              f"{' GOAL:' + r['goal_song'] if r['goal_song'] else ''}{' ERR ' + r['error'] if 'error' in r else ''}", flush=True)
    (GOAL / 'project_folder_survey.json').write_text(json.dumps(rows, indent=1))


if __name__ == '__main__':
    main()
