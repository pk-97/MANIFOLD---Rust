#!/usr/bin/env python3
"""Bass family labels (Peter, 2026-10-10: prominent bass notes and bass hits in full mixes, Noisia/Skrillex drops
included; not every little note). Writes GOAL/bass_labels.json through detector_labels.

Bass: notes and audio clips on bass-named tracks (sub, 808, reese, growl, wobble, neuro...). Prominent = an attack
in the mix's 40-500 Hz band (rise >= 6 dB) within 6 dB of the song's main bass level. A bass note within 30 ms of a
kick is unscored: the mix cannot tell which made the attack. Runs under 90 ms apart (rolls, fast trills) are
unscored. Vouched negatives: kicks, other melodic notes, and drum-rack voices that are not bass.
"""
import re
import sys
from dataclasses import replace
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parents[3]))
from tools.audio_analysis.eval.detector_labels import Family  # noqa: E402

BASS = re.compile(r'bass|\bsub\b|growl|reese|wobble|neuro|808|donk|screech|riddim', re.I)
NEVER = re.compile(r'(?!)')
FAMILY = Family(
    name='bass',
    voice=BASS,
    exclude=re.compile(r'kick|drum|vocal|vox|bus|send|return|resolume|trigger', re.I),
    other_voice=re.compile(r'snare|clap|hat|\bhh|ride|cymbal|crash|perc|shaker|tamb|tom|rim', re.I),
    other_parts=re.compile(r'resampl|bounce|print|freeze', re.I),
    not_other_parts=NEVER,
    band=(40, 500),
    stem_band=(40, 2000),
    prominent_db=6.0,
    dense_s=.090,
    kick_clash_s=.030,
)


def main():
    from tools.audio_analysis.eval.detector_labels import write
    from tools.audio_analysis.eval.family_songs import family_songs
    from tools.audio_analysis.eval.kick_goal_eval import GOAL
    write(replace(FAMILY, midi_songs=family_songs(FAMILY), add_wip=False), GOAL / 'bass_labels.json', lambda s: print(s, flush=True))


if __name__ == '__main__':
    main()
