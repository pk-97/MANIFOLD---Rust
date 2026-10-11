#!/usr/bin/env python3
"""Label audit (docs/DETECTOR_LAB_DESIGN.md D5): checks raw known times, never loudness. Writes
<labels>.audit.json next to each labels file, keyed by the labels file's sha256 (the labels file is never touched),
and prints the table and the failures.

Project song fails when: its export offset has no proof against a stem, trigger track or the WIP kick-and-snare
agreement; or fewer than a quarter of its known times survive the prominence filter (with more than 20 known times);
or its fitted timing shift sits at the edge of the +-30 ms search. Generated song fails when a label is not a snare or
clap note of its section, or a snare or clap note of its section has no label. A failing song is excluded from every
run until Peter sets audit_override in songs.json. If more than MAX_FAILS project songs fail, stop: the table goes to
Peter before anything is excluded.

Usage: audit.py FAMILY"""
import hashlib
import json
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT))
from tools.audio_analysis.lab import recipe  # noqa: E402

recipe.apply()

import numpy as np  # noqa: E402

PROVEN = ('wip:proven', 'goal:trigger', 'goal:kick stem', 'generated')
MIN_KEPT, MIN_KNOWN, SHIFT_EDGE_MS, MAX_FAILS = .25, 20, 30.0, 3


def project_check(s, song):
    kept = len(s['positives']) + len(s.get('dense_pos', []))
    known = kept + len(s.get('dropped', []))
    why = []
    if not song.proof.startswith(PROVEN):
        why.append(f'offset proof: {song.proof}')
    if known > MIN_KNOWN and kept / known < MIN_KEPT:
        why.append(f'{kept} of {known} known times kept')
    if abs(s.get('timing_shift_ms', 0.0)) >= SHIFT_EDGE_MS:
        why.append(f"timing shift {s['timing_shift_ms']} ms at the search edge")
    return dict(known=known, kept=kept, shift_ms=s.get('timing_shift_ms', 0.0), proof=song.proof, why=why)


def generated_check(s, notes):
    """notes: the section's snare and clap note times (s, section time), layers merged as the builder does."""
    pos = np.array(s['positives'])
    dur = s['section'][1] - s['section'][0]
    inner = notes[(notes > .1) & (notes < dur - .1)]
    stray = int(np.sum(np.min(np.abs(pos[:, None] - notes[None, :]), axis=1) > .001)) if len(pos) and len(notes) else len(pos)
    unlabelled = int(np.sum(np.min(np.abs(inner[:, None] - pos[None, :]), axis=1) > .001)) if len(inner) and len(pos) else len(inner)
    why = ([f'{stray} labels on no snare note'] if stray else []) + ([f'{unlabelled} snare notes unlabelled'] if unlabelled else [])
    return dict(known=len(inner), kept=len(pos), why=why)


def generated_notes(lab):
    """{song: section-time snare and clap notes} for the generated songs, from the projects (read only)."""
    from tools.audio_analysis.eval.als_extract import extract
    from tools.audio_analysis.eval.note_clean import PROJECT
    from tools.audio_analysis.eval.snare_generated import SETS, SNARE_KEYS
    from tools.audio_analysis.eval.snare_labels import FAMILY
    out = {}
    for als, export, _ in SETS:
        res = extract(PROJECT / als)
        start = min(c['start_s'] for tr in res['tracks'] for c in tr['midi_clips'] if c['notes'])
        times = np.sort([n['s'] - start for tr in res['tracks'] if 'LinnDrum' in tr['name']
                         for c in tr['midi_clips'] for n in c['notes'] if n['key'] in SNARE_KEYS])
        for t, s in lab.items():
            if s.get('kind') == 'generated' and export in s.get('export', ''):
                a, b = s['section']
                sec = times[(times >= a) & (times < b)] - a
                out[t] = sec[np.concatenate([[True], np.diff(sec) > FAMILY.layer_s])] if len(sec) else sec
    return out


def write_sidecar(labels_path, songs_result):
    """<labels>.audit.json keyed by the labels file's sha256; the labels file itself is never written."""
    sha = hashlib.sha256(Path(labels_path).read_bytes()).hexdigest()
    side = Path(str(labels_path)[:-len('.json')] + '.audit.json')
    side.write_text(json.dumps(dict(labels_sha256=sha, songs=songs_result), indent=1))
    return side


def main():
    from tools.audio_analysis.eval.kick_goal_eval import GOAL
    from tools.audio_analysis.lab.registry import FAMILIES, load
    fam = sys.argv[1] if len(sys.argv) > 1 else sys.exit(__doc__)
    songs = load()
    fails = []
    for f in FAMILIES[fam]:
        path = GOAL / f
        lab = json.loads(path.read_text())
        notes = generated_notes(lab) if any(s.get('kind') == 'generated' for s in lab.values()) else {}
        result = {}
        for t, s in lab.items():
            r = generated_check(s, notes[t]) if s.get('kind') == 'generated' else project_check(s, songs[t])
            r['override'] = songs[t].audit_override
            r['pass'] = not r['why'] or bool(r['override'])
            result[t] = r
            flag = 'pass' if r['pass'] else 'FAIL'
            print(f"{t:18s} {s.get('kind', '?'):9s} {flag}  kept {r['kept']:4d} of {r['known']:4d}"
                  f"{'  ' + '; '.join(r['why']) if r['why'] else ''}{'  override: ' + r['override'] if r['override'] else ''}")
            if not r['pass']:
                fails.append((t, s.get('kind')))
        print(f'-> {write_sidecar(path, result)}')
    proj = [t for t, k in fails if k != 'generated']
    print(f'{len(fails)} failing: {[t for t, _ in fails]}')
    if len(proj) > MAX_FAILS:
        print(f'STOP: {len(proj)} project songs fail (more than {MAX_FAILS}). Show Peter this table before excluding any.')


if __name__ == '__main__':
    main()
