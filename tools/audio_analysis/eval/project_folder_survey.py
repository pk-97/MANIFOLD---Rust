#!/usr/bin/env python3
"""Read-only survey of Peter's Ableton project folders (default 2024 and 2025) for snare/clap, hat and percussion
training songs.

Per project: newest .als in the project root, rendered audio in the project root (a mixdown candidate), snare/clap
notes (detector_labels.project_parts with the snare family: named tracks and drum-rack pads, rim excluded), other-drum audio spans, and whether
the goal set already uses it. Hats and percussion count their notes with the same rules (HATS, PERC families). Projects are read only through als_extract (sha-checked); nothing is written near them.
Writes GOAL/project_folder_survey[_YEARS].json. Usage: project_folder_survey.py [YEAR ...]"""
import json
import os
import sys
from concurrent.futures import ProcessPoolExecutor
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parents[3]))

BASE = Path('/Users/peterkiemann/Library/CloudStorage/Dropbox/Music Production/Ableton Projects')
AUDIO = ('.wav', '.mp3', '.aif', '.aiff', '.flac', '.m4a')
YEARS = tuple(sys.argv[1:]) or ('2024', '2025')


def one(proj):
    from tools.audio_analysis.eval.detector_labels import project_parts
    from tools.audio_analysis.eval.snare_labels import FAMILY
    from tools.audio_analysis.eval.als_extract import extract
    from tools.audio_analysis.eval.detector_labels import Family
    import re
    never = re.compile(r'(?!)')
    voices = {'hat': re.compile(r'hat|\bhh|ride|cymbal|shaker|shkr', re.I),
              'perc': re.compile(r'perc|tom|conga|bongo|cowbell|clave|wood|block|tamb|guiro|cabasa|triangle', re.I)}
    others = {k: Family(name=k, voice=v, exclude=FAMILY.voice, other_voice=never, other_parts=never, not_other_parts=never,
                        band=(0, 1)) for k, v in voices.items()}
    als = sorted(proj.glob('*.als'), key=lambda p: p.stat().st_mtime)
    renders = sorted(str(p.name) for p in proj.iterdir() if p.is_file() and p.suffix.lower() in AUDIO)
    row = dict(project=str(proj), als=str(als[-1]) if als else None, n_als=len(als), renders=renders)
    if als:
        try:
            res = extract(str(als[-1]))
            snare, rim, spans, clips = project_parts(res, FAMILY)
            row.update(snare_notes=len(snare), rim_notes=len(rim), loop_s=round(sum(b - a for a, b in spans), 1),
                       snare_clips=sum(len(v) for k, v in clips.items() if k != '_neg'),
                       span_s=[round(float(min(snare)), 1), round(float(max(snare)), 1)] if len(snare) else None)
            for k, fam in others.items():
                row[f'{k}_notes'] = len(project_parts(res, fam)[0])
        except Exception as e:  # a broken or unreadable set is reported, not fatal
            row['error'] = f'{type(e).__name__}: {e}'[:120]
    return row


def main():
    from tools.audio_analysis.eval.kick_goal_eval import GOAL, Goal
    from tools.audio_analysis.eval.kick_goal_melodic import _source
    used = {}
    for t in Goal(mode='v3').records:
        s = _source(t)
        if s:
            used[str(Path(s[0]).parent)] = t
    projs = [p for y in YEARS for p in sorted((BASE / y).iterdir()) if p.is_dir()]
    with ProcessPoolExecutor(6) as ex:
        rows = list(ex.map(one, projs))
    for r in rows:
        r['goal_song'] = used.get(r['project'])
        print(f"{Path(r['project']).parent.name}/{Path(r['project']).name[:34]:34s} snare {r.get('snare_notes', '-'):>4} "
              f"clips {r.get('snare_clips', '-'):>3} hat {r.get('hat_notes', '-'):>5} perc {r.get('perc_notes', '-'):>5} loops {r.get('loop_s', '-'):>6} s renders {len(r['renders']):2d}"
              f"{' GOAL:' + r['goal_song'] if r['goal_song'] else ''}{' ERR ' + r['error'] if 'error' in r else ''}", flush=True)
    tag = '' if YEARS == ('2024', '2025') else '_' + '_'.join(YEARS)
    (GOAL / f'project_folder_survey{tag}.json').write_text(json.dumps(rows, indent=1))


if __name__ == '__main__':
    main()
