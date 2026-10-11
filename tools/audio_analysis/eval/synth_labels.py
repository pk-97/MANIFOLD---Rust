#!/usr/bin/env python3
"""Synth family labels: every audible synth note (leads, arps, plucks, stabs, chords, keys; Peter, 2026-10-11:
reliable triggering on every note first). Pads swell rather than attack: their notes are unscored for now.
Writes GOAL/synth_labels.json through note_labels."""
import sys
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parents[3]))
from tools.audio_analysis.eval.note_labels import NoteFamily  # noqa: E402

FAMILY = NoteFamily(name='synth', part='synth', band=(500, 6000), stem_band=(200, 10000), unscored_parts=('?', 'pad'))


def main():
    from tools.audio_analysis.eval.kick_goal_eval import GOAL
    from tools.audio_analysis.eval.note_labels import write
    write(FAMILY, GOAL / 'synth_labels.json', lambda s: print(s, flush=True))


if __name__ == '__main__':
    main()
