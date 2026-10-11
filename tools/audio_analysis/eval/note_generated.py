#!/usr/bin/env python3
"""Bass and synth training songs from Peter's generated label sets (generated_sets), one song per section, by the
same event rules as the project songs (note_labels.events), less the level test (every note sounds for certain):
every note on the family's part at its note start,
layers merged, fast runs unscored, bass notes on a kick not events, other parts' notes and the kicks vouched
negatives. Writes GOAL/{family}_labels_generated.json; the lab reads them (lab/registry.py, lab/run.py).
Usage: note_generated.py FAMILY"""
import importlib
import json
import sys
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parents[3]))


def main():
    from tools.audio_analysis.eval import generated_sets as gs
    from tools.audio_analysis.eval.detector_labels import builder_id, env_db
    from tools.audio_analysis.eval.kick_goal_eval import GOAL
    from tools.audio_analysis.eval.note_labels import events, project_notes
    family = sys.argv[1] if len(sys.argv) > 1 else sys.exit(__doc__)
    fam = importlib.import_module(f'tools.audio_analysis.eval.{family}_labels').FAMILY
    out = {}
    for als, export, how in gs.SETS:
        res, start, x = gs.load(als, export)
        notes = project_notes(res)
        kicks = gs.kicks(res, start)
        for name, a, b in gs.sections(res, how, start):
            sec = [n for n in notes if a <= n[0] - start < b]
            if not any(n[3] == fam.part for n in sec):
                continue
            y = x[int(a * gs.SR):int(b * gs.SR)].astype(float)
            k = kicks[(kicks >= a) & (kicks < b)] - a
            s = events(fam, sec, [], [], k, [], env_db(y, gs.SR, *fam.band), (b - a), -(start + a), check_audible=False)
            s.update(kind='generated', section=[float(a), float(b)], export=gs.export_path(export), offset_s=float(a),
                     sources=dict(generated=len(s['positives'])), builder=builder_id(__file__))
            out[name] = s
            c = s['counts']
            print(f"{name:14s} {b - a:6.1f} s: {len(s['positives']):4d} {family} events of {c['events']:4d} notes "
                  f"({c['inaudible']} inaudible, {c.get('on_kick', 0)} on a kick, {c['dense']} in fast runs)")
    (GOAL / f'{family}_labels_generated.json').write_text(json.dumps(out))
    print(f"total {sum(len(s['positives']) for s in out.values())} {family} events in {len(out)} songs")


if __name__ == '__main__':
    main()
