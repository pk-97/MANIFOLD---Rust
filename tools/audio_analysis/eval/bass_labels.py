#!/usr/bin/env python3
"""Bass family labels: every audible bass note (Peter, 2026-10-11: reliable triggering on every note first). A bass
note on a kick is not a bass event: the kick fires there. Writes GOAL/bass_labels.json through note_labels."""
import sys
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parents[3]))
from tools.audio_analysis.eval.note_labels import NoteFamily  # noqa: E402

FAMILY = NoteFamily(name='bass', part='bass', band=(40, 500), stem_band=(40, 2000), on_kick='negative')


def main():
    from tools.audio_analysis.eval.kick_goal_eval import GOAL
    from tools.audio_analysis.eval.note_labels import write
    write(FAMILY, GOAL / 'bass_labels.json', lambda s: print(s, flush=True))


if __name__ == '__main__':
    main()
