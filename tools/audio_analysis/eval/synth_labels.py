#!/usr/bin/env python3
"""Synth family labels (Peter, 2026-10-10: leads, arps, stabs and plucks, each prominent note an event; full mixes).
Writes GOAL/synth_labels.json through detector_labels.

Synth: notes and audio clips on synth-named tracks (lead, arp, pluck, stab, chord, keys, piano, bell...); bass tracks
are excluded and their notes are vouched negatives. Prominent = an attack in the mix's 500-6000 Hz band (rise >= 6
dB) within 6 dB of the song's main synth level. A note within 30 ms of a kick is unscored. Runs under 90 ms apart are
unscored. Vouched negatives: kicks, bass and other melodic notes, drum-rack voices.
"""
import re
import sys
from dataclasses import replace
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parents[3]))
from tools.audio_analysis.eval.bass_labels import BASS  # noqa: E402
from tools.audio_analysis.eval.detector_labels import Family  # noqa: E402

FAMILY = Family(
    name='synth',
    voice=re.compile(r'synth|lead|arp|pluck|stab|chord|key|piano|rhodes|organ|bell|mallet|marimba|saw|square|poly|hook|melod|seq', re.I),
    exclude=re.compile(BASS.pattern + r'|kick|drum|vocal|vox|bus|send|return|resolume|trigger|pad|drone|atmos|riser|fx', re.I),
    other_voice=re.compile(r'snare|clap|hat|\bhh|ride|cymbal|crash|perc|shaker|tamb|tom|rim', re.I),
    other_parts=re.compile(r'resampl|bounce|print|freeze|loop', re.I),
    not_other_parts=re.compile(r'kick|drum|vocal|vox', re.I),
    band=(500, 6000),
    stem_band=(200, 10000),
    prominent_db=6.0,
    dense_s=.090,
    kick_clash_s=.030,
)


def main():
    from tools.audio_analysis.eval.detector_labels import write
    from tools.audio_analysis.eval.family_songs import family_songs
    from tools.audio_analysis.eval.kick_goal_eval import GOAL
    write(replace(FAMILY, midi_songs=family_songs(FAMILY), add_wip=False), GOAL / 'synth_labels.json', lambda s: print(s, flush=True))


if __name__ == '__main__':
    main()
