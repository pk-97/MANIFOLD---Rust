"""Kick-stem truth confirmed by the project's kick trigger track.

Peter, 2026-10-10: the real kicks are the ones with a real attack that line up
with something in the project; a custom detector does not decide what a kick is.
A stem attack (kick_goal_rolls.kick_notes) is a kick only when a note of the
project's kick track lands within HIT_MS of it. Attacks without a note are not
kicks: Midnight Patience's kick runs through a delay, so 198 of its 320 stem
attacks were echoes at +0.5 and +1.25 beats. The attack's own time is kept, so
the label sits on the audio, not on the note.

The export offset is kick_goal_trigger_labels.export_offset: the candidate
offset that puts the most notes on an attack.
"""
from __future__ import annotations

import numpy as np

from tools.audio_analysis.eval.als_extract import extract
from tools.audio_analysis.eval.kick_goal_labels import ABL, GOAL
from tools.audio_analysis.eval.kick_goal_trigger_labels import HIT_MS, export_offset, nearest, note_times

# Every version of the Midnight project (Export, New Studio, FINALS) has the same 167 kick notes.
PROJECT = {
    'midnight_patience': dict(als=ABL / '2024/Bettys Project/Bettys (Midnight Patience) - Export.als', kick='43-DS Kick'),
}


def confirmed(track, attacks):
    """The stem attacks (stem time, s) that a project kick note lands on; cached per track."""
    path = GOAL / f'projkicks_{track}.npy'
    if not path.exists():
        notes = note_times(extract(PROJECT[track]['als']), PROJECT[track]['kick'])
        attacks = np.sort(np.asarray(attacks, float))
        off = export_offset(attacks, notes)
        keep = attacks[nearest(attacks, notes + off) <= HIT_MS / 1000]
        print(f'{track}: {len(keep)} of {len(attacks)} stem attacks on a kick note (offset {off:+.3f} s, '
              f'{len(notes)} notes)', flush=True)
        np.save(path, keep)
    return np.load(path)
