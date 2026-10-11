#!/usr/bin/env python3
"""Label audit (docs/DETECTOR_LAB_DESIGN.md D5): checks raw known times, never loudness. Writes
<labels>.audit.json next to each labels file, keyed by the labels file's sha256 (the labels file is never touched),
and prints the table and the failures.

Project song fails when its export offset has no proof (a kick stem, a trigger track, or the kick-and-snare grid fit
of detector_wip or lab/offsets.py) or its fitted timing shift sits at the edge of the +-30 ms search. The share of
known times kept is reported, never failed: quiet fills and rolls are dropped by design (Peter, 2026-10-11: "we should
focus on the snares on the main off beats, not all of the little fills and percusions"). Generated song fails when a
label sits on no family note of its section, or a family note of its section outside its unscored spans (rolls, fast
runs) has no label; a bass note on a kick is the kick's. A failing song is excluded from every
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

PROVEN = ('wip:proven', 'lab:proven', 'goal:trigger', 'goal:kick stem', 'generated')
# Label-level proofs (a labels entry's 'proof'): the times were measured where no project offset enters. 'stem': on
# the song's own kick stem, rendered with the mix; 'trigger': the trigger track's verified kicks; 'reviewed': Peter's
# reviewed passages. 'project' (project MIDI) falls back to the song's offset proof.
LABEL_PROOFS = ('stem', 'trigger', 'reviewed')
SHIFT_EDGE_MS, MAX_FAILS = 30.0, 3


def project_check(s, song):
    kept = len(s['positives']) + len(s.get('dense_pos', []))
    known = kept + len(s.get('dropped', []))
    why = []
    proof = s['proof'] if s.get('proof') in LABEL_PROOFS else song.proof
    if proof not in LABEL_PROOFS and not proof.startswith(PROVEN):
        why.append(f'offset proof: {proof}')
    if abs(s.get('timing_shift_ms', 0.0)) >= SHIFT_EDGE_MS:
        why.append(f"timing shift {s['timing_shift_ms']} ms at the search edge")
    return dict(known=known, kept=kept, shift_ms=s.get('timing_shift_ms', 0.0), proof=proof, why=why)


def generated_check(s, notes, kicks=(), kick_s=0.0):
    """notes: the section's family note times (s, section time), layers merged as the builder does. A note not
    labelled is fine inside an unscored span, or within kick_s of a kick (a bass note on a kick is the kick)."""
    from tools.audio_analysis.eval.detector_eval import inside, nearest
    pos = np.array(s['positives'])
    dur = s['section'][1] - s['section'][0]
    inner = notes[(notes > .1) & (notes < dur - .1) & ~inside(notes, s['unscored'])]
    if kick_s and len(kicks) and len(inner):
        inner = inner[nearest(inner, kicks) > kick_s]
    stray = int(np.sum(np.min(np.abs(pos[:, None] - notes[None, :]), axis=1) > .001)) if len(pos) and len(notes) else len(pos)
    unlabelled = int(np.sum(np.min(np.abs(inner[:, None] - pos[None, :]), axis=1) > .001)) if len(inner) and len(pos) else len(inner)
    why = ([f'{stray} labels on no family note'] if stray else []) + ([f'{unlabelled} family notes unlabelled'] if unlabelled else [])
    return dict(known=len(inner), kept=len(pos), why=why)


def generated_notes(lab, family):
    """{song: (section-time family note times, section-time kicks, kick_s)} for the generated songs, from the
    projects (read only), layers merged as the builders do."""
    import importlib
    from tools.audio_analysis.eval import generated_sets as gs
    from tools.audio_analysis.eval.note_labels import NoteFamily, project_notes
    fam = importlib.import_module(f'tools.audio_analysis.eval.{family}_labels').FAMILY
    note_family = isinstance(fam, NoteFamily)
    out = {}
    for als, export, _ in gs.SETS:
        res, start, _ = gs.load(als, export)
        if note_family:
            times = np.sort([n[0] - start for n in project_notes(res) if n[3] == fam.part and n[4] >= 1])
        else:
            from tools.audio_analysis.eval.snare_generated import SNARE_KEYS
            times = np.sort([n['s'] - start for tr in res['tracks'] if 'LinnDrum' in tr['name']
                             for c in tr['midi_clips'] for n in c['notes'] if n['key'] in SNARE_KEYS])
        kicks = gs.kicks(res, start)
        for t, s in lab.items():
            if s.get('kind') == 'generated' and s.get('export') == gs.export_path(export):
                a, b = s['section']
                sec = times[(times >= a) & (times < b)] - a
                sec = sec[np.concatenate([[True], np.diff(sec) > fam.layer_s])] if len(sec) else sec
                k = kicks[(kicks >= a) & (kicks < b)] - a
                on_kick = note_family and fam.on_kick == 'negative'
                out[t] = (sec, k, fam.kick_s if on_kick else 0.0)
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
        notes = generated_notes(lab, fam) if any(s.get('kind') == 'generated' for s in lab.values()) else {}
        result = {}
        for t, s in lab.items():
            r = generated_check(s, *notes[t]) if s.get('kind') == 'generated' else project_check(s, songs[t])
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
